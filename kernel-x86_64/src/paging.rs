//! Tablas de paginas propias en x86_64 (D12).
//!
//! # Como pagina x86_64
//!
//! Cuatro niveles de tablas, cada una de 512 entradas de 8 bytes (4 KiB justos):
//!
//! ```text
//!   CR3 -> PML4 -> PDPT -> PD -> PT -> pagina de 4 KiB
//!                    |
//!                    +--- con el bit PS: pagina de 1 GiB, y se acabo el paseo
//! ```
//!
//! Poniendo el bit PS (bit 7) en una entrada del PDPT, esa entrada **es** una
//! pagina de 1 GiB y no hace falta bajar mas. Eso deja el identity map en dos
//! niveles y con muy pocas entradas, que es justamente lo que busca D12: la
//! presion sobre el TLB queda en casi nada.
//!
//! # Las tablas viven adentro del kernel
//!
//! Son arreglos estaticos, o sea que caen en la imagen que UEFI cargo como
//! `LoaderData` y el mapa informa como `Kind::Kernel`. Es el mismo motivo que
//! con la pila: si vivieran en memoria libre, `mem.claim` se las podria
//! entregar al agente.

use kernel_core::machine::Machine;
use kernel_core::paging::{self, Attr, Mapping};

/// 512 entradas de 8 bytes: una tabla ocupa una pagina de 4 KiB.
const ENTRADAS: usize = 512;

/// Cada PDPT cubre 512 GiB. Con ocho se llegan a 4 TiB, que es mucho mas de lo
/// que direcciona cualquier maquina donde esto vaya a correr — y si algun dia
/// no alcanza, se avisa en vez de mapear a medias y callarse.
const MAX_PDPT: usize = 8;

/// Cuantos pedazos de 1 GiB se pueden partir en bloques de 2 MiB.
///
/// Se parten los que tienen kernel o memoria libre adentro, que en la practica
/// son uno o dos: la RAM utilizable de una maquina chica entra ahi.
const MAX_PD: usize = 4;

#[repr(C, align(4096))]
struct Tabla([u64; ENTRADAS]);

static mut PML4: Tabla = Tabla([0; ENTRADAS]);
static mut PDPT: [Tabla; MAX_PDPT] = [const { Tabla([0; ENTRADAS]) }; MAX_PDPT];
static mut PD: [Tabla; MAX_PD] = [const { Tabla([0; ENTRADAS]) }; MAX_PD];

// --- Los bits de una entrada ------------------------------------------------
const PRESENTE: u64 = 1 << 0;
const ESCRIBIBLE: u64 = 1 << 1;
/// PWT: write-through.
const PWT: u64 = 1 << 3;
/// PCD: cache disable. Con PWT arriba da "no cacheable" con el PAT por defecto.
const PCD: u64 = 1 << 4;
/// PS en el PDPT: esta entrada es una pagina de 1 GiB.
const GRANDE: u64 = 1 << 7;

/// Arma las tablas y las carga en CR3.
///
/// # Safety
///
/// Solo despues de `ExitBootServices`: cambiar CR3 con el firmware todavia vivo
/// le sacaria el piso a sus propias estructuras.
pub unsafe fn install(m: &Machine) -> Result<Mapping, &'static str> {
    if !hay_paginas_de_1gib() {
        return Err("el CPU no tiene paginas de 1 GiB (CPUID 80000001h EDX.26)");
    }

    let total = paging::span_gib(m);
    if total == 0 {
        return Err("el mapa de memoria esta vacio");
    }
    if total > (MAX_PDPT * ENTRADAS) as u64 {
        return Err("la maquina direcciona mas de 4 TiB y las tablas no llegan");
    }

    let pml4 = &mut *core::ptr::addr_of_mut!(PML4);
    let pdpt = &mut *core::ptr::addr_of_mut!(PDPT);

    let pd = &mut *core::ptr::addr_of_mut!(PD);
    let mut device_gib = 0;
    let mut partidos = 0usize;

    for gib in 0..total {
        let cual = (gib / ENTRADAS as u64) as usize;
        let cual_entrada = (gib % ENTRADAS as u64) as usize;

        let device = paging::attr_of(m, gib) == Attr::Device;
        if device {
            device_gib += 1;
        }
        let cache = if device { PCD | PWT } else { 0 };

        if !paging::needs_split(m, gib) {
            // Nada que distinguir adentro: un solo bloque de 1 GiB. Sin el bit
            // de usuario, o sea que es del kernel — de ahi no salen reclamos.
            pdpt[cual].0[cual_entrada] =
                (gib * paging::GIB) | PRESENTE | ESCRIBIBLE | GRANDE | cache;
            continue;
        }

        // Con kernel o con memoria libre adentro: se parte en bloques de 2 MiB
        // para poder marcar cuales alcanza el agente y cuales no.
        if partidos >= MAX_PD {
            return Err("hay mas pedazos con kernel adentro de los que se pueden partir");
        }
        let tabla = &mut pd[partidos];
        for i in 0..ENTRADAS {
            let base = gib * paging::GIB + i as u64 * paging::BLOQUE;
            // Arrancan siendo del kernel. El permiso se prende bloque por
            // bloque cuando el agente reclama memoria pidiendolo (D27).
            tabla.0[i] = base | PRESENTE | ESCRIBIBLE | GRANDE | cache;
        }
        // La entrada que apunta a la tabla de bloques lleva el bit de usuario
        // **prendido**, y no porque todo lo de abajo sea del agente: el permiso
        // efectivo es el AND de todos los niveles, asi que si esta apagado aca
        // arriba, lo que digan los bloques de abajo no importa. Quien decide es
        // cada bloque.
        pdpt[cual].0[cual_entrada] =
            (core::ptr::addr_of!(*tabla) as u64) | PRESENTE | ESCRIBIBLE | USUARIO;
        partidos += 1;
    }

    // Cuantos PDPT se llegaron a usar, y se cuelgan del PML4.
    let usados = total.div_ceil(ENTRADAS as u64) as usize;
    for i in 0..usados {
        let dir = core::ptr::addr_of!(pdpt[i]) as u64;
        // Mismo motivo que arriba: el nivel de mas arriba tiene que dejar pasar
        // para que los de abajo puedan decidir.
        pml4.0[i] = dir | PRESENTE | ESCRIBIBLE | USUARIO;
    }

    // El salto. Desde la instruccion siguiente, todas las direcciones se
    // traducen con estas tablas — por eso el identity map tiene que incluir el
    // codigo que esta corriendo, y lo incluye: mapea todo.
    let raiz = core::ptr::addr_of!(*pml4) as u64;
    core::arch::asm!("mov cr3, {}", in(reg) raiz, options(nostack, preserves_flags));

    // Se relee para confirmar que el cambio ocurrio. Sin esto, un `install`
    // que no hiciera nada se veria igual que uno que anduvo — y el sintoma
    // seria que mem.claim entrega las tablas del firmware creyendolas libres.
    // Los 12 bits de abajo de CR3 son banderas, no direccion.
    let puesto: u64;
    core::arch::asm!("mov {}, cr3", out(reg) puesto, options(nostack, preserves_flags));
    if puesto & !0xFFF != raiz {
        return Err("CR3 no quedo apuntando a nuestras tablas");
    }

    // Y prender SMEP, que es lo que le prohibe al kernel **ejecutar** una
    // pagina marcada para el agente. Sin esto el bit de usuario queda puesto
    // pero no significa nada.
    //
    // Va despues de cargar las tablas y antes de que exista ningun reclamo: si
    // hubiera una pagina ya marcada, el kernel dejaria de poder ejecutar la
    // suya en el mismo instante.
    let smep = prender_smep();

    Ok(Mapping { gib: total, device_gib, root: raiz, isolation: smep })
}

/// Prende SMEP si el CPU lo tiene.
///
/// Si no lo tiene, no se puede hacer nada: la memoria marcada para el agente
/// sigue siendo ejecutable con privilegio. Eso no rompe nada —el agente igual
/// no puede enmascarar interrupciones, que es el punto de D27— pero conviene
/// saber que la separacion es mas floja en esa maquina.
///
/// # Safety
///
/// Ninguna pagina puede estar marcada para el agente todavia, o el kernel
/// dejaria de poder ejecutar la suya.
unsafe fn prender_smep() -> bool {
    use core::arch::x86_64::__cpuid_count;

    // Hoja 7, subhoja 0, bit 7 de EBX.
    if __cpuid_count(0, 0).eax < 7 {
        return false;
    }
    if __cpuid_count(7, 0).ebx & (1 << 7) == 0 {
        return false;
    }

    let mut cr4: u64;
    core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    cr4 |= 1 << 20;
    core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack));

    // Se relee: pedirle algo al CPU y no comprobarlo es suponer.
    core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    cr4 & (1 << 20) != 0
}

/// Pregunta si el CPU soporta paginas de 1 GiB.
///
/// No se da por sentado: es una extension, y un CPU viejo o un modelo de QEMU
/// austero no la tiene. Si faltara y se armaran las tablas igual, el bit PS se
/// interpretaria como parte de una direccion y el salto seria a la nada.
fn hay_paginas_de_1gib() -> bool {
    use core::arch::x86_64::__cpuid;

    // Primero hay que preguntar si existen las hojas extendidas: pedir una que
    // no existe devuelve basura de otra hoja, no un cero.
    if __cpuid(0x8000_0000).eax < 0x8000_0001 {
        return false;
    }
    __cpuid(0x8000_0001).edx & (1 << 26) != 0
}

/// El bit que hace a una pagina alcanzable desde el nivel sin privilegio.
///
/// **Y por eso mismo deja de ser ejecutable por el kernel**: eso es SMEP, una
/// funcion del CPU que el firmware suele prender. Una pagina es del agente o la
/// ejecuta el kernel, nunca las dos — la misma regla que aarch64 tiene metida en
/// el modelo de permisos.
const USUARIO: u64 = 1 << 2;

/// Marca un rango como alcanzable, o no, desde el nivel sin privilegio.
///
/// # Safety
///
/// El rango tiene que estar alineado a `BLOQUE` y caer en pedazos ya partidos.
pub unsafe fn set_user_access(
    m: &Machine,
    start: u64,
    bytes: u64,
    user: bool,
) -> Result<(), &'static str> {
    if start % paging::BLOQUE != 0 || bytes % paging::BLOQUE != 0 || bytes == 0 {
        return Err("el rango no esta alineado al bloque");
    }

    let pdpt = &mut *core::ptr::addr_of_mut!(PDPT);
    let pd = &mut *core::ptr::addr_of_mut!(PD);

    let mut dir = start;
    while dir < start + bytes {
        let gib = dir / paging::GIB;
        if !paging::needs_split(m, gib) {
            return Err("ese pedazo no tiene grano fino");
        }
        // Encontrar la tabla de bloques que cuelga de esa entrada.
        let cual = (gib / ENTRADAS as u64) as usize;
        let entrada = pdpt[cual].0[(gib % ENTRADAS as u64) as usize];
        if entrada & GRANDE != 0 {
            return Err("ese pedazo quedo como un bloque entero");
        }
        let tabla_dir = entrada & 0x000F_FFFF_FFFF_F000;

        let indice = ((dir % paging::GIB) / paging::BLOQUE) as usize;
        let tabla = pd
            .iter_mut()
            .find(|t| core::ptr::addr_of!(**t) as u64 == tabla_dir)
            .ok_or("no se encontro la tabla de bloques")?;

        if user {
            tabla.0[indice] |= USUARIO;
        } else {
            tabla.0[indice] &= !USUARIO;
        }
        dir += paging::BLOQUE;
    }

    // Lo que el CPU se acuerde de antes ya no vale. Recargar la raiz vacia el
    // recuerdo entero: mas caro que borrar entrada por entrada, pero esto pasa
    // al reclamar memoria y no en un camino caliente.
    let raiz: u64;
    core::arch::asm!("mov {}, cr3", out(reg) raiz, options(nomem, nostack));
    core::arch::asm!("mov cr3, {}", in(reg) raiz, options(nostack));
    Ok(())
}
