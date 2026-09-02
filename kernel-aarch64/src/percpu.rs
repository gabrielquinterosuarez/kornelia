//! El bloque privado de cada nucleo (D13).
//!
//! # Por que hace falta
//!
//! Con un solo nucleo corriendo codigo del agente, guardar "hay un `exec` en
//! curso" en una variable global alcanza. Con varios no: dos nucleos que fallan
//! a la vez se pisan el reporte, y el punto de recuperacion de uno manda al otro
//! a volver adonde no era.
//!
//! Cada nucleo tiene su bloque y lo encuentra en `TPIDR_EL1`, un registro por
//! nucleo que la arquitectura reserva exactamente para esto.
//!
//! # Una cosa menos que en x86_64
//!
//! Aca las pilas de excepcion no hay que separarlas: `SP_EL1` ya esta bancado
//! por nucleo en el hardware, asi que cada uno entra a sus excepciones sobre la
//! suya sin que nadie haga nada. En x86_64 eso hay que construirlo con un TSS
//! por nucleo.

use kernel_core::cores;

/// El nucleo de arranque no esta en la tabla de nucleos reclamados, asi que se
/// le reserva la ranura de mas arriba.
pub const BOOT_SLOT: usize = cores::MAX;

/// Uno por nucleo reclamable, mas el de arranque.
pub const SLOTS: usize = cores::MAX + 1;

/// Lo privado de cada nucleo.
///
/// El orden de los campos **es interfaz**: el ensamblador de `exec` los alcanza
/// por su posicion. Los `assert!` de abajo lo verifican al compilar.
#[repr(C, align(64))]
pub struct PerCpu {
    /// Hay un `exec` en curso y el punto de recuperacion esta armado.
    pub armed: u64,
    /// Adonde saltar si el codigo del agente falla.
    pub rip: u64,
    /// La pila del kernel de este nucleo, para recuperarla.
    pub sp: u64,
    /// La cima de la pila donde corre el codigo del agente.
    pub stack: u64,
    /// Los registros al terminar un `exec` que volvio solo. Son 34: x0 a x30,
    /// el puntero de pila, el pc y el pstate — los mismos nombres, y en el
    /// mismo orden, que informa `REGISTERS`.
    pub regs: [u64; 34],
    /// Que ranura es esta.
    pub slot: u64,
    /// A donde saltar. Va en el bloque y no en un registro porque los registros
    /// se cargan con lo que pidio el agente justo antes de saltar, asi que no
    /// queda ninguno libre para llevar la direccion.
    pub entry: u64,
}

impl PerCpu {
    const fn new() -> Self {
        Self { armed: 0, rip: 0, sp: 0, stack: 0, regs: [0; 34], slot: 0, entry: 0 }
    }
}

const _: () = assert!(core::mem::offset_of!(PerCpu, armed) == 0);
const _: () = assert!(core::mem::offset_of!(PerCpu, rip) == 8);
const _: () = assert!(core::mem::offset_of!(PerCpu, sp) == 16);
const _: () = assert!(core::mem::offset_of!(PerCpu, stack) == 24);
const _: () = assert!(core::mem::offset_of!(PerCpu, regs) == 32);
const _: () = assert!(core::mem::offset_of!(PerCpu, slot) == 304);
const _: () = assert!(core::mem::offset_of!(PerCpu, entry) == 312);

static mut BLOCKS: [PerCpu; SLOTS] = [const { PerCpu::new() }; SLOTS];

/// Engancha el bloque de esta ranura al nucleo que esta corriendo.
///
/// # Safety
///
/// Se llama una vez por nucleo, y `slot` tiene que ser suya y de nadie mas.
pub unsafe fn install_block(slot: usize) {
    let b = &mut (*core::ptr::addr_of_mut!(BLOCKS))[slot];
    b.slot = slot as u64;

    let addr = b as *mut PerCpu as u64;
    core::arch::asm!("msr tpidr_el1, {}", in(reg) addr, options(nomem, nostack));
    core::arch::asm!("isb", options(nomem, nostack));
}

/// El bloque del nucleo que esta corriendo.
fn current() -> *mut PerCpu {
    let p: u64;
    // SAFETY: `install_block` dejo TPIDR_EL1 apuntando a un bloque valido.
    unsafe { core::arch::asm!("mrs {}, tpidr_el1", out(reg) p, options(nomem, nostack)) };
    p as *mut PerCpu
}

/// La ranura del nucleo que esta corriendo.
///
/// Una sola lectura de un registro, sin tocar memoria compartida: es lo que
/// permite llamarlo desde adentro de un handler de excepciones.
pub fn slot() -> usize {
    // SAFETY: el puntero sale de TPIDR_EL1, que apunta a un bloque estatico.
    unsafe { (*current()).slot as usize }
}

/// Si hay un `exec` en curso en **este** nucleo.
pub fn armed() -> u64 {
    unsafe { (*current()).armed }
}

/// Adonde desviar el regreso si el codigo del agente fallo.
pub fn return_point() -> u64 {
    unsafe { (*current()).rip }
}

/// El bloque de esa ranura.
///
/// # Safety
///
/// Quien lo use tiene que respetar que es de un solo nucleo.
pub unsafe fn block(slot: usize) -> *mut PerCpu {
    core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(BLOCKS))[slot])
}
