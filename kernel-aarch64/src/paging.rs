//! Tablas de traduccion propias en aarch64 (D12).
//!
//! # Como traduce aarch64
//!
//! La idea es la misma que en x86_64 pero cambia todo lo demas. Con grano de
//! 4 KiB y direcciones de 48 bits hay cuatro niveles, numerados del 0 al 3:
//!
//! ```text
//!   TTBR0_EL1 -> nivel 0 -> nivel 1 -> nivel 2 -> nivel 3 -> pagina de 4 KiB
//!                              |
//!                              +--- descriptor de bloque: 1 GiB, y listo
//! ```
//!
//! Un descriptor de **bloque** en el nivel 1 mapea 1 GiB de una, que es el
//! equivalente del bit PS de x86.
//!
//! # La diferencia grande: los atributos no van en el descriptor
//!
//! En x86 los bits de cacheabilidad estan en la entrada. Aca el descriptor solo
//! lleva un **indice de 3 bits** a `MAIR_EL1`, un registro con ocho ranuras que
//! definen que significa cada indice. Se configuran dos: la 0 es memoria normal
//! cacheable y la 1 es memoria de dispositivo.
//!
//! # Por que se apaga la MMU para el cambio
//!
//! Se toca `TCR_EL1`, que define el grano y el tamano de las direcciones.
//! Cambiarlo con la MMU prendida no esta garantizado por la arquitectura. Se
//! apaga, se configura y se prende — y el bloque de abajo no toca la pila
//! mientras tanto, porque con la MMU apagada los accesos a memoria dejan de
//! pasar por la cache y lo que hubiera quedado sucio ahi no se veria.

use kernel_core::machine::Machine;
use kernel_core::paging::{self, Attr, Mapping};

const ENTRIES: usize = 512;

/// Cada tabla de nivel 1 cubre 512 GiB.
const MAX_LEVEL1: usize = 8;

/// Cuantos pedazos de 1 GiB se pueden partir en bloques de 2 MiB.
///
/// Se parten los que tienen kernel o memoria libre adentro: uno o dos.
const MAX_LEVEL2: usize = 4;

#[repr(C, align(4096))]
struct Table([u64; ENTRIES]);

static mut LEVEL0: Table = Table([0; ENTRIES]);
static mut LEVEL1: [Table; MAX_LEVEL1] = [const { Table([0; ENTRIES]) }; MAX_LEVEL1];
static mut LEVEL2: [Table; MAX_LEVEL2] = [const { Table([0; ENTRIES]) }; MAX_LEVEL2];

// --- Descriptores -----------------------------------------------------------
/// Los dos bits de abajo en 0b11: esta entrada apunta a otra tabla.
const IS_TABLE: u64 = 0b11;
/// En 0b01: esta entrada ES un bloque de memoria.
const IS_BLOCK: u64 = 0b01;
/// Access Flag. Si esta en cero, el primer acceso da fault en vez de andar.
const AF: u64 = 1 << 10;
/// Inner shareable: coherente con los otros nucleos. Solo para memoria normal;
/// la de dispositivo no lo lleva.
const SHAREABLE: u64 = 0b11 << 8;

/// Ranura 0 de MAIR: memoria normal, write-back, se cachea.
const ATTR_NORMAL: u64 = 0 << 2;
/// Ranura 1 de MAIR: dispositivo.
const ATTR_DEVICE: u64 = 1 << 2;

/// El contenido de MAIR_EL1: ranura 0 = 0xFF (normal write-back), ranura 1 =
/// 0x04 (Device-nGnRE).
const MAIR: u64 = 0x0000_0000_0000_04FF;

/// Arma las tablas y las carga.
///
/// # Safety
///
/// Solo despues de `ExitBootServices`, y solo desde EL1.
pub unsafe fn install(m: &Machine) -> Result<Mapping, &'static str> {
    if exception_level() != 1 {
        // A EL2 le corresponden otros registros. UEFI en la maquina `virt` sin
        // virtualizacion entrega en EL1, pero se comprueba en vez de suponerlo.
        return Err("no estamos en EL1");
    }

    let total = paging::span_gib(m);
    if total == 0 {
        return Err("el mapa de memoria esta vacio");
    }
    if total > (MAX_LEVEL1 * ENTRIES) as u64 {
        return Err("la maquina direcciona mas de 4 TiB y las tablas no llegan");
    }

    let n0 = &mut *core::ptr::addr_of_mut!(LEVEL0);
    let n1 = &mut *core::ptr::addr_of_mut!(LEVEL1);

    let n2 = &mut *core::ptr::addr_of_mut!(LEVEL2);
    let mut device_gib = 0;
    let mut split_count = 0usize;

    for gib in 0..total {
        let which = (gib / ENTRIES as u64) as usize;
        let which_entry = (gib % ENTRIES as u64) as usize;

        let device = paging::attr_of(m, gib) == Attr::Device;
        if device {
            device_gib += 1;
        }
        // Los atributos, sin el permiso: eso se decide aparte.
        let attr = if device {
            AF | ATTR_DEVICE
        } else {
            AF | SHAREABLE | ATTR_NORMAL
        };

        if !paging::needs_split(m, gib) {
            // Nada que distinguir adentro: un bloque de 1 GiB entero, del
            // kernel — de ahi no salen reclamos.
            n1[which].0[which_entry] =
                (gib * paging::GIB) | attr | IS_BLOCK;
            continue;
        }

        // Con kernel o con memoria libre adentro: se parte en bloques de 2 MiB.
        if split_count >= MAX_LEVEL2 {
            return Err("hay mas pedazos con kernel adentro de los que se pueden partir");
        }
        let table = &mut n2[split_count];
        for i in 0..ENTRIES {
            let base = gib * paging::GIB + i as u64 * paging::BLOCK;
            // Arrancan siendo del kernel. El permiso se prende bloque por
            // bloque cuando el agente reclama memoria pidiendolo (D27).
            table.0[i] = base | attr | IS_BLOCK;
        }
        n1[which].0[which_entry] = (core::ptr::addr_of!(*table) as u64) | IS_TABLE;
        split_count += 1;
    }

    let used_count = total.div_ceil(ENTRIES as u64) as usize;
    for i in 0..used_count {
        n0.0[i] = (core::ptr::addr_of!(n1[i]) as u64) | IS_TABLE;
    }

    let root = core::ptr::addr_of!(*n0) as u64;
    load_root(root, tcr());

    // Se relee para confirmar que el cambio ocurrio. Los bits de arriba de
    // TTBR0_EL1 llevan el ASID, no direccion.
    let read_back: u64;
    core::arch::asm!("mrs {}, ttbr0_el1", out(reg) read_back, options(nomem, nostack));
    if read_back & 0x0000_FFFF_FFFF_FFFE != root {
        return Err("TTBR0_EL1 no quedo apuntando a nuestras tablas");
    }

    // En aarch64 no hay nada que prender: que una pagina alcanzable desde EL0
    // no sea ejecutable desde EL1 viene en el modelo de permisos.
    Ok(Mapping { gib: total, device_gib, root: root, isolation: true })
}

/// El valor de TCR_EL1 para grano de 4 KiB y direcciones de 48 bits.
fn tcr() -> u64 {
    // Cuantos bits fisicos soporta este CPU, tal como el lo informa (P4).
    let mmfr0: u64;
    unsafe { core::arch::asm!("mrs {}, id_aa64mmfr0_el1", out(reg) mmfr0, options(nomem, nostack)) };
    let ips = (mmfr0 & 0xF).min(5); // 5 = 48 bits, que es hasta donde vamos

    // T0SZ=16 da 48 bits de direccion virtual.
    16
        | (0b01 << 8)   // IRGN0: las tablas se leen cacheables por dentro
        | (0b01 << 10)  // ORGN0: idem por fuera
        | (0b11 << 12)  // SH0: inner shareable
        | (0b00 << 14)  // TG0: grano de 4 KiB
        | (1 << 23)     // EPD1: nadie camina por TTBR1, no lo usamos
        | (ips << 32)   // IPS
}

fn exception_level() -> u64 {
    let el: u64;
    unsafe { core::arch::asm!("mrs {}, currentel", out(reg) el, options(nomem, nostack)) };
    (el >> 2) & 0b11
}

/// Apaga la MMU, cambia la configuracion y la vuelve a prender.
///
/// Va todo en un solo bloque de asm y sin tocar la pila a proposito: entre que
/// se apaga y se prende, los accesos a memoria no pasan por la cache, y lo que
/// hubiera quedado sucio ahi no se veria.
///
/// # Safety
///
/// Las tablas que apunta `raiz` tienen que identity-mapear el codigo que esta
/// corriendo. Si no, la instruccion siguiente al `isb` final se busca en una
/// direccion que no existe.
unsafe fn load_root(root: u64, tcr: u64) {
    core::arch::asm!(
        "dsb sy",
        "isb",

        // Afuera la MMU (M) y la cache de datos (C).
        "mrs {t}, sctlr_el1",
        "bic {t}, {t}, #1",
        "bic {t}, {t}, #(1 << 2)",
        "msr sctlr_el1, {t}",
        "isb",

        // Nada de lo que habia traducido antes sirve.
        "tlbi vmalle1",
        "dsb sy",
        "isb",

        "msr mair_el1, {mair}",
        "msr tcr_el1, {tcr}",
        "msr ttbr0_el1, {root}",
        "dsb sy",
        "isb",

        // Y de vuelta: MMU, cache de datos y cache de instrucciones.
        "mrs {t}, sctlr_el1",
        "orr {t}, {t}, #1",
        "orr {t}, {t}, #(1 << 2)",
        "orr {t}, {t}, #(1 << 12)",
        "msr sctlr_el1, {t}",
        "isb",

        t = out(reg) _,
        mair = in(reg) MAIR,
        tcr = in(reg) tcr,
        root = in(reg) root,
        options(nostack, preserves_flags),
    );
}

/// El campo que hace a un bloque alcanzable desde EL0.
///
/// `00` es "solo EL1", `01` es "EL1 y EL0". Y marcarlo **deja el bloque fuera
/// del alcance del kernel para ejecutar**: es la misma regla que SMEP en x86,
/// pero acá metida en el modelo de permisos y sin forma de apagarla.
const AP_USER: u64 = 0b01 << 6;

/// Marca un rango como alcanzable, o no, desde EL0.
///
/// # Safety
///
/// El rango tiene que estar alineado a `BLOCK` y caer en pedazos ya partidos.
pub unsafe fn set_user_access(
    m: &Machine,
    start: u64,
    bytes: u64,
    user: bool,
) -> Result<(), &'static str> {
    if start % paging::BLOCK != 0 || bytes % paging::BLOCK != 0 || bytes == 0 {
        return Err("el rango no esta alineado al bloque");
    }

    let n1 = &mut *core::ptr::addr_of_mut!(LEVEL1);
    let n2 = &mut *core::ptr::addr_of_mut!(LEVEL2);

    let mut addr = start;
    while addr < start + bytes {
        let gib = addr / paging::GIB;
        if !paging::needs_split(m, gib) {
            return Err("ese pedazo no tiene grano fino");
        }
        let which = (gib / ENTRIES as u64) as usize;
        let entry = n1[which].0[(gib % ENTRIES as u64) as usize];
        if entry & 0b11 != IS_TABLE {
            return Err("ese pedazo quedo como un bloque entero");
        }
        let table_addr = entry & 0x0000_FFFF_FFFF_F000;

        let index = ((addr % paging::GIB) / paging::BLOCK) as usize;
        let table = n2
            .iter_mut()
            .find(|t| core::ptr::addr_of!(**t) as u64 == table_addr)
            .ok_or("no se encontro la tabla de bloques")?;

        if user {
            table.0[index] |= AP_USER;
        } else {
            table.0[index] &= !AP_USER;
        }
        addr += paging::BLOCK;
    }

    // Lo que el CPU se acuerde de antes ya no vale.
    core::arch::asm!("dsb ishst", options(nostack));
    core::arch::asm!("tlbi vmalle1", options(nostack));
    core::arch::asm!("dsb ish", options(nostack));
    core::arch::asm!("isb", options(nostack));
    Ok(())
}
