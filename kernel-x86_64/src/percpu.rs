//! El bloque privado de cada nucleo (D13).
//!
//! # Por que hace falta
//!
//! Con un solo nucleo corriendo codigo del agente, guardar "hay un `exec` en
//! curso" en una variable global alcanza. Con varios no: dos nucleos que fallan
//! a la vez se pisan el reporte, y el punto de recuperacion de uno manda al
//! otro a volver adonde no era.
//!
//! Asi que cada nucleo tiene su bloque, y **lo encuentra sin pasar por nada
//! compartido**: la direccion vive en la base del segmento GS, que en x86_64 no
//! es un segmento de verdad sino un registro por CPU pensado justo para esto.
//!
//! Es lo que permite que el handler de excepciones sepa de quien es el fault que
//! esta atendiendo sin tener que preguntarle a nadie — cosa importante, porque
//! preguntar implicaria leer memoria compartida desde adentro de un fault.

use kernel_core::cores;

/// El nucleo de arranque no esta en la tabla de nucleos reclamados, asi que se
/// le reserva la ranura de mas arriba.
pub const BOOT_SLOT: usize = cores::MAX;

/// Cuantos bloques hay: uno por nucleo reclamable, mas el de arranque.
pub const SLOTS: usize = cores::MAX + 1;

/// Lo privado de cada nucleo.
///
/// El orden de los campos **es interfaz**: el ensamblador de `exec` los alcanza
/// por su posicion, no por su nombre. Los `assert!` de abajo lo verifican en
/// tiempo de compilacion.
#[repr(C, align(64))]
pub struct PerCpu {
    /// Hay un `exec` en curso y el punto de recuperacion esta armado.
    pub armed: u64,
    /// Adonde saltar si el codigo del agente falla.
    pub rip: u64,
    /// La pila del kernel de este nucleo, para recuperarla.
    pub rsp: u64,
    /// La cima de la pila donde corre el codigo del agente.
    pub stack: u64,
    /// Los registros al terminar un `exec` que volvio solo.
    pub regs: [u64; 18],
    /// Que ranura es esta. Lo lee el handler para saber donde anotar.
    pub slot: u64,
    /// A donde saltar. Va en el bloque y no en un registro porque los registros
    /// se cargan con lo que pidio el agente justo antes de saltar, asi que no
    /// queda ninguno libre para llevar la direccion.
    pub entry: u64,
}

impl PerCpu {
    const fn new() -> Self {
        Self { armed: 0, rip: 0, rsp: 0, stack: 0, regs: [0; 18], slot: 0, entry: 0 }
    }
}

// El ensamblador usa estos numeros escritos a mano. Si alguien agrega un campo
// arriba, esto no compila en vez de leer basura.
const _: () = assert!(core::mem::offset_of!(PerCpu, armed) == 0);
const _: () = assert!(core::mem::offset_of!(PerCpu, rip) == 8);
const _: () = assert!(core::mem::offset_of!(PerCpu, rsp) == 16);
const _: () = assert!(core::mem::offset_of!(PerCpu, stack) == 24);
const _: () = assert!(core::mem::offset_of!(PerCpu, regs) == 32);
const _: () = assert!(core::mem::offset_of!(PerCpu, slot) == 176);
const _: () = assert!(core::mem::offset_of!(PerCpu, entry) == 184);

static mut BLOCKS: [PerCpu; SLOTS] = [const { PerCpu::new() }; SLOTS];

/// Donde el CPU guarda la base de GS.
const IA32_GS_BASE: u32 = 0xC000_0101;

/// Engancha el bloque de esta ranura al nucleo que esta corriendo.
///
/// # Safety
///
/// Se llama una vez por nucleo, y `slot` tiene que ser suya y de nadie mas.
pub unsafe fn install_block(slot: usize) {
    let b = &mut (*core::ptr::addr_of_mut!(BLOCKS))[slot];
    b.slot = slot as u64;

    let addr = b as *mut PerCpu as u64;
    core::arch::asm!(
        "wrmsr",
        in("ecx") IA32_GS_BASE,
        in("eax") addr as u32,
        in("edx") (addr >> 32) as u32,
        options(nomem, nostack, preserves_flags),
    );
}

/// La ranura del nucleo que esta corriendo.
///
/// Una sola lectura, sin tocar memoria compartida: es lo que hace que se pueda
/// llamar desde adentro de un handler de excepciones.
pub fn slot() -> usize {
    let r: u64;
    // SAFETY: `install_block` dejo GS apuntando a un bloque valido; el 176 es el
    // offset de `slot`, verificado arriba en tiempo de compilacion.
    unsafe { core::arch::asm!("mov {}, gs:[176]", out(reg) r, options(nostack, readonly)) };
    r as usize
}

/// El bloque de esta ranura.
///
/// # Safety
///
/// Quien lo use tiene que respetar que es de un solo nucleo.
pub unsafe fn block(slot: usize) -> *mut PerCpu {
    core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(BLOCKS))[slot])
}

/// Si hay un `exec` en curso en **este** nucleo.
///
/// Lo llama el handler de excepciones, que no puede darse el lujo de leer
/// memoria compartida para averiguarlo.
pub fn armed() -> u64 {
    let v: u64;
    // SAFETY: offset 0 del bloque, verificado arriba.
    unsafe { core::arch::asm!("mov {}, gs:[0]", out(reg) v, options(nostack, readonly)) };
    v
}

/// La pila del kernel de este nucleo, la que `exec` dejo anotada.
///
/// La necesita el desvio de faults cuando el codigo venia de anillo 3: ahi el
/// marco lleva el puntero de pila del agente, y volver con ese seria volver a
/// la pila de anillo 3 con codigo de anillo 0 (D27).
pub fn kernel_stack() -> u64 {
    let v: u64;
    // SAFETY: offset 16 del bloque, verificado arriba.
    unsafe { core::arch::asm!("mov {}, gs:[16]", out(reg) v, options(nostack, readonly)) };
    v
}

/// Adonde desviar el regreso si el codigo del agente fallo.
pub fn return_point() -> u64 {
    let v: u64;
    // SAFETY: offset 8 del bloque, verificado arriba.
    unsafe { core::arch::asm!("mov {}, gs:[8]", out(reg) v, options(nostack, readonly)) };
    v
}
