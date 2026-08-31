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
/// Se parten los que tienen memoria del kernel adentro, que en la practica son
/// uno o dos: la imagen del kernel entra en unos cientos de KiB.
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
            // Sin memoria del kernel adentro: un solo bloque de 1 GiB, y el
            // agente lo alcanza entero.
            pdpt[cual].0[cual_entrada] =
                (gib * paging::GIB) | PRESENTE | ESCRIBIBLE | GRANDE | cache;
            continue;
        }

        // Con memoria del kernel adentro: se parte en bloques de 2 MiB para
        // poder marcar cuales alcanza el agente y cuales no.
        if partidos >= MAX_PD {
            return Err("hay mas pedazos con kernel adentro de los que se pueden partir");
        }
        let tabla = &mut pd[partidos];
        for i in 0..ENTRADAS {
            let base = gib * paging::GIB + i as u64 * paging::BLOQUE;
            // Todavia sin marcar quien alcanza que: el permiso va junto con la
            // transicion de privilegio, no antes.
            tabla.0[i] = base | PRESENTE | ESCRIBIBLE | GRANDE | cache;
        }
        pdpt[cual].0[cual_entrada] =
            (core::ptr::addr_of!(*tabla) as u64) | PRESENTE | ESCRIBIBLE;
        partidos += 1;
    }

    // Cuantos PDPT se llegaron a usar, y se cuelgan del PML4.
    let usados = total.div_ceil(ENTRADAS as u64) as usize;
    for i in 0..usados {
        let dir = core::ptr::addr_of!(pdpt[i]) as u64;
        pml4.0[i] = dir | PRESENTE | ESCRIBIBLE;
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

    Ok(Mapping { gib: total, device_gib, root: raiz })
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
