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

const ENTRADAS: usize = 512;

/// Cada tabla de nivel 1 cubre 512 GiB.
const MAX_NIVEL1: usize = 8;

#[repr(C, align(4096))]
struct Tabla([u64; ENTRADAS]);

static mut NIVEL0: Tabla = Tabla([0; ENTRADAS]);
static mut NIVEL1: [Tabla; MAX_NIVEL1] = [const { Tabla([0; ENTRADAS]) }; MAX_NIVEL1];

// --- Descriptores -----------------------------------------------------------
/// Los dos bits de abajo en 0b11: esta entrada apunta a otra tabla.
const ES_TABLA: u64 = 0b11;
/// En 0b01: esta entrada ES un bloque de memoria.
const ES_BLOQUE: u64 = 0b01;
/// Access Flag. Si esta en cero, el primer acceso da fault en vez de andar.
const AF: u64 = 1 << 10;
/// Inner shareable: coherente con los otros nucleos. Solo para memoria normal;
/// la de dispositivo no lo lleva.
const COMPARTIDA: u64 = 0b11 << 8;

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
    if nivel_de_excepcion() != 1 {
        // A EL2 le corresponden otros registros. UEFI en la maquina `virt` sin
        // virtualizacion entrega en EL1, pero se comprueba en vez de suponerlo.
        return Err("no estamos en EL1");
    }

    let total = paging::span_gib(m);
    if total == 0 {
        return Err("el mapa de memoria esta vacio");
    }
    if total > (MAX_NIVEL1 * ENTRADAS) as u64 {
        return Err("la maquina direcciona mas de 4 TiB y las tablas no llegan");
    }

    let n0 = &mut *core::ptr::addr_of_mut!(NIVEL0);
    let n1 = &mut *core::ptr::addr_of_mut!(NIVEL1);

    let mut device_gib = 0;

    for gib in 0..total {
        let cual = (gib / ENTRADAS as u64) as usize;
        let cual_entrada = (gib % ENTRADAS as u64) as usize;

        let d = if paging::attr_of(m, gib) == Attr::Device {
            device_gib += 1;
            (gib * paging::GIB) | AF | ATTR_DEVICE | ES_BLOQUE
        } else {
            (gib * paging::GIB) | AF | COMPARTIDA | ATTR_NORMAL | ES_BLOQUE
        };
        n1[cual].0[cual_entrada] = d;
    }

    let usadas = total.div_ceil(ENTRADAS as u64) as usize;
    for i in 0..usadas {
        n0.0[i] = (core::ptr::addr_of!(n1[i]) as u64) | ES_TABLA;
    }

    let raiz = core::ptr::addr_of!(*n0) as u64;
    cargar(raiz, tcr());

    // Se relee para confirmar que el cambio ocurrio. Los bits de arriba de
    // TTBR0_EL1 llevan el ASID, no direccion.
    let puesto: u64;
    core::arch::asm!("mrs {}, ttbr0_el1", out(reg) puesto, options(nomem, nostack));
    if puesto & 0x0000_FFFF_FFFF_FFFE != raiz {
        return Err("TTBR0_EL1 no quedo apuntando a nuestras tablas");
    }

    Ok(Mapping { gib: total, device_gib, root: raiz })
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

fn nivel_de_excepcion() -> u64 {
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
unsafe fn cargar(raiz: u64, tcr: u64) {
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
        "msr ttbr0_el1, {raiz}",
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
        raiz = in(reg) raiz,
        options(nostack, preserves_flags),
    );
}
