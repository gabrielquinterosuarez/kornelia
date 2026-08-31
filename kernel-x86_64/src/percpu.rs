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
pub const RANURA_ARRANQUE: usize = cores::MAX;

/// Cuantos bloques hay: uno por nucleo reclamable, mas el de arranque.
pub const RANURAS: usize = cores::MAX + 1;

/// Lo privado de cada nucleo.
///
/// El orden de los campos **es interfaz**: el ensamblador de `exec` los alcanza
/// por su posicion, no por su nombre. Los `assert!` de abajo lo verifican en
/// tiempo de compilacion.
#[repr(C, align(64))]
pub struct PerCpu {
    /// Hay un `exec` en curso y el punto de recuperacion esta armado.
    pub armado: u64,
    /// Adonde saltar si el codigo del agente falla.
    pub rip: u64,
    /// La pila del kernel de este nucleo, para recuperarla.
    pub rsp: u64,
    /// La cima de la pila donde corre el codigo del agente.
    pub pila: u64,
    /// Los registros al terminar un `exec` que volvio solo.
    pub regs: [u64; 18],
    /// Que ranura es esta. Lo lee el handler para saber donde anotar.
    pub ranura: u64,
}

impl PerCpu {
    const fn nuevo() -> Self {
        Self { armado: 0, rip: 0, rsp: 0, pila: 0, regs: [0; 18], ranura: 0 }
    }
}

// El ensamblador usa estos numeros escritos a mano. Si alguien agrega un campo
// arriba, esto no compila en vez de leer basura.
const _: () = assert!(core::mem::offset_of!(PerCpu, armado) == 0);
const _: () = assert!(core::mem::offset_of!(PerCpu, rip) == 8);
const _: () = assert!(core::mem::offset_of!(PerCpu, rsp) == 16);
const _: () = assert!(core::mem::offset_of!(PerCpu, pila) == 24);
const _: () = assert!(core::mem::offset_of!(PerCpu, regs) == 32);
const _: () = assert!(core::mem::offset_of!(PerCpu, ranura) == 176);

static mut BLOQUES: [PerCpu; RANURAS] = [const { PerCpu::nuevo() }; RANURAS];

/// Donde el CPU guarda la base de GS.
const IA32_GS_BASE: u32 = 0xC000_0101;

/// Engancha el bloque de esta ranura al nucleo que esta corriendo.
///
/// # Safety
///
/// Se llama una vez por nucleo, y `ranura` tiene que ser suya y de nadie mas.
pub unsafe fn instalar(ranura: usize) {
    let b = &mut (*core::ptr::addr_of_mut!(BLOQUES))[ranura];
    b.ranura = ranura as u64;

    let dir = b as *mut PerCpu as u64;
    core::arch::asm!(
        "wrmsr",
        in("ecx") IA32_GS_BASE,
        in("eax") dir as u32,
        in("edx") (dir >> 32) as u32,
        options(nomem, nostack, preserves_flags),
    );
}

/// La ranura del nucleo que esta corriendo.
///
/// Una sola lectura, sin tocar memoria compartida: es lo que hace que se pueda
/// llamar desde adentro de un handler de excepciones.
pub fn ranura() -> usize {
    let r: u64;
    // SAFETY: `instalar` dejo GS apuntando a un bloque valido; el 176 es el
    // offset de `ranura`, verificado arriba en tiempo de compilacion.
    unsafe { core::arch::asm!("mov {}, gs:[176]", out(reg) r, options(nostack, readonly)) };
    r as usize
}

/// El bloque de esta ranura.
///
/// # Safety
///
/// Quien lo use tiene que respetar que es de un solo nucleo.
pub unsafe fn bloque(ranura: usize) -> *mut PerCpu {
    core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(BLOQUES))[ranura])
}

/// Si hay un `exec` en curso en **este** nucleo.
///
/// Lo llama el handler de excepciones, que no puede darse el lujo de leer
/// memoria compartida para averiguarlo.
pub fn armado() -> u64 {
    let v: u64;
    // SAFETY: offset 0 del bloque, verificado arriba.
    unsafe { core::arch::asm!("mov {}, gs:[0]", out(reg) v, options(nostack, readonly)) };
    v
}

/// Adonde desviar el regreso si el codigo del agente fallo.
pub fn punto_de_retorno() -> u64 {
    let v: u64;
    // SAFETY: offset 8 del bloque, verificado arriba.
    unsafe { core::arch::asm!("mov {}, gs:[8]", out(reg) v, options(nostack, readonly)) };
    v
}
