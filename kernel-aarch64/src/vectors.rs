//! Captura de excepciones en aarch64 (P5, D7).
//!
//! # Como avisa ARM que algo salio mal
//!
//! Nada que ver con la IDT de x86. Aca hay **una sola tabla de 16 entradas**, y
//! cada entrada no es una direccion sino 128 bytes de codigo: se salta ahi
//! directamente. Las 16 salen de cruzar cuatro clases de excepcion (sincronica,
//! IRQ, FIQ, SError) con cuatro procedencias (mismo EL con SP0, mismo EL con
//! SPx, EL mas bajo en 64 bits, EL mas bajo en 32 bits).
//!
//! El motivo de la excepcion no viene en el numero de entrada como en x86: hay
//! que leerlo de `ESR_EL1`, cuyos 6 bits de arriba son la clase.
//!
//! # Por que hay que sumarle 4 al volver de un `brk`
//!
//! En x86 un `int3` deja RIP apuntando a la instruccion siguiente. En ARM
//! `ELR_EL1` apunta **a la instruccion que fallo**, asi que volver sin tocarlo
//! reejecutaria el mismo `brk` para siempre.

use core::fmt::Write;
use kernel_core::fault::{Cause, Fault};

/// El estado que ve el handler, en el orden en que lo dejo el codigo de abajo.
#[repr(C)]
pub struct Frame {
    /// x0 a x30.
    x: [u64; 31],
    /// El puntero de pila de quien fallo. Ocupa el hueco que antes era relleno
    /// —el marco necesita un largo par para quedar alineado a 16— asi que no
    /// cuesta nada, y evita que el reporte tenga que callarse un registro.
    ///
    /// Va **antes** que los otros dos porque el orden de este struct es el
    /// orden en que el ensamblador de abajo deja las cosas en la pila, y es
    /// tambien el orden de `REGISTERS`.
    sp: u64,
    /// Donde volver. En una excepcion sincronica apunta a la instruccion que
    /// fallo, no a la siguiente.
    elr: u64,
    /// El estado del procesador al momento de fallar.
    spsr: u64,
}

/// Los nombres de esta maquina, en el orden en que vienen los valores (D3).
///
/// `sp` esta aparte de x0-x30 porque en aarch64 no es uno de ellos: es un
/// registro propio, y ademas bancado por nivel de excepcion. Antes no figuraba
/// y el camino de retorno de `exec` guardaba el puntero de pila en la posicion
/// de `pc` — el kernel informando un registro con el nombre de otro, que es
/// exactamente lo que P4 no permite.
pub const REGISTERS: &[&str] = &[
    "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13",
    "x14", "x15", "x16", "x17", "x18", "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26",
    "x27", "x28", "x29", "x30", "sp", "pc", "pstate",
];

/// Uno por nucleo: dos que fallan a la vez no se pisan el reporte.
static mut VALUES: [[u64; REGISTERS.len()]; crate::percpu::SLOTS] =
    [[0; REGISTERS.len()]; crate::percpu::SLOTS];
static mut LAST: [Option<Fault>; crate::percpu::SLOTS] = [None; crate::percpu::SLOTS];

core::arch::global_asm!(
    r#"
.section .text

// Cada entrada de la tabla son 128 bytes. Alcanza con saltar a donde
// corresponda: de que se trata una excepcion sincronica lo dice ESR_EL1, no la
// posicion — pero un IRQ **si** se distingue por la posicion, y tiene que ir a
// otro lado: para el handler de faults un IRQ seria un error sin causa.
.macro ENTRY target
    b \target
    .balign 0x80
.endm

// La tabla entera tiene que estar alineada a 2048.
.balign 2048
.globl VECTORES
VECTORES:
    ENTRY vec_common   // mismo EL, SP0:  sincronica
    ENTRY vec_irq      //                 IRQ  <- durante un exec entra aca
    ENTRY vec_common   //                 FIQ
    ENTRY vec_common   //                 SError
    ENTRY vec_common   // mismo EL, SPx:  sincronica
    ENTRY vec_irq      //                 IRQ  <- y aca si corre el kernel
    ENTRY vec_common   //                 FIQ
    ENTRY vec_common   //                 SError
    ENTRY vec_lower    // EL mas bajo, 64 bits: sincronica  <- la ventanilla
    ENTRY vec_irq      //                       IRQ
    ENTRY vec_common   //                       FIQ
    ENTRY vec_common   //                       SError
    ENTRY vec_common   // EL mas bajo, 32 bits: sincronica
    ENTRY vec_irq      //                       IRQ
    ENTRY vec_common   //                       FIQ
    ENTRY vec_common   //                       SError

// Una excepcion sincronica que llega desde EL0 puede ser dos cosas muy
// distintas: el codigo `supervised` del agente abriendo la ventanilla para
// volver (D27), o ese mismo codigo fallando. Las dos entran por el mismo lugar
// —la posicion en la tabla solo dice de donde vino, no que paso— asi que hay
// que preguntarle al ESR, cuyos 6 bits de arriba son la clase: 0x15 es `svc`
// desde 64 bits.
//
// El andamio va a la pila del kernel y no a un registro: en este punto todos
// los registros son del agente, y pisarle uno seria informarle mal el estado
// con el que volvio.
vec_lower:
    stp x9, x10, [sp, #-16]!
    mrs x9, esr_el1
    lsr x10, x9, #26
    cmp x10, #0x15
    b.ne 8f
    // Es un `svc`, y cual: el numero que se le puso queda en los 16 bits de
    // abajo del ESR. El cero termina el `exec`; el uno pide un verbo y sigue.
    and x9, x9, #0xFFFF
    cbz x9, exec_window
    b   exec_service
8:
    ldp x9, x10, [sp], #16
    // No era la ventanilla: es un fault de verdad, y sigue el camino de todos.
    b vec_common

vec_common:
    // 34 huecos de 8 bytes: 31 registros, el puntero de pila, ELR y SPSR. El
    // largo es par, que es lo que ARM exige para que la pila quede alineada
    // a 16.
    sub sp, sp, #(34 * 8)

    stp x0,  x1,  [sp, #(0 * 8)]
    stp x2,  x3,  [sp, #(2 * 8)]
    stp x4,  x5,  [sp, #(4 * 8)]
    stp x6,  x7,  [sp, #(6 * 8)]
    stp x8,  x9,  [sp, #(8 * 8)]
    stp x10, x11, [sp, #(10 * 8)]
    stp x12, x13, [sp, #(12 * 8)]
    stp x14, x15, [sp, #(14 * 8)]
    stp x16, x17, [sp, #(16 * 8)]
    stp x18, x19, [sp, #(18 * 8)]
    stp x20, x21, [sp, #(20 * 8)]
    stp x22, x23, [sp, #(22 * 8)]
    stp x24, x25, [sp, #(24 * 8)]
    stp x26, x27, [sp, #(26 * 8)]
    stp x28, x29, [sp, #(28 * 8)]
    str x30,      [sp, #(30 * 8)]

    mrs x0, elr_el1
    mrs x1, spsr_el1
    stp x0, x1,   [sp, #(32 * 8)]

    // Y el puntero de pila de quien fallo. Cual es depende de donde venia: si
    // el SPSR dice EL0 es SP_EL0 —el agente corriendo supervisado—, y si no es
    // este mismo sp, pero antes de que el marco lo bajara.
    tst  x1, #0xF
    mrs  x0, sp_el0
    add  x2, sp, #(34 * 8)
    csel x0, x0, x2, eq
    str  x0,      [sp, #(31 * 8)]

    mov x0, sp
    bl fault_rust

    // El handler puede haber corrido ELR para saltearse la instruccion.
    ldp x0, x1,   [sp, #(32 * 8)]
    msr elr_el1, x0
    msr spsr_el1, x1

    ldp x0,  x1,  [sp, #(0 * 8)]
    ldp x2,  x3,  [sp, #(2 * 8)]
    ldp x4,  x5,  [sp, #(4 * 8)]
    ldp x6,  x7,  [sp, #(6 * 8)]
    ldp x8,  x9,  [sp, #(8 * 8)]
    ldp x10, x11, [sp, #(10 * 8)]
    ldp x12, x13, [sp, #(12 * 8)]
    ldp x14, x15, [sp, #(14 * 8)]
    ldp x16, x17, [sp, #(16 * 8)]
    ldp x18, x19, [sp, #(18 * 8)]
    ldp x20, x21, [sp, #(20 * 8)]
    ldp x22, x23, [sp, #(22 * 8)]
    ldp x24, x25, [sp, #(24 * 8)]
    ldp x26, x27, [sp, #(26 * 8)]
    ldp x28, x29, [sp, #(28 * 8)]
    ldr x30,      [sp, #(30 * 8)]

    add sp, sp, #(34 * 8)
    eret

// Lo que corre cuando suena un timbre. No es un fault: no hay causa que leer ni
// registros que informar, solo hay que atender y volver.
//
// Se salva lo que la convencion de llamadas permite pisar —x0 a x18 y x30, que
// es donde `bl` deja la direccion de retorno—. Los demas los salva Rust.
vec_irq:
    sub sp, sp, #(20 * 8)
    stp x0,  x1,  [sp, #(0 * 8)]
    stp x2,  x3,  [sp, #(2 * 8)]
    stp x4,  x5,  [sp, #(4 * 8)]
    stp x6,  x7,  [sp, #(6 * 8)]
    stp x8,  x9,  [sp, #(8 * 8)]
    stp x10, x11, [sp, #(10 * 8)]
    stp x12, x13, [sp, #(12 * 8)]
    stp x14, x15, [sp, #(14 * 8)]
    stp x16, x17, [sp, #(16 * 8)]
    stp x18, x30, [sp, #(18 * 8)]

    bl irq_rust

    ldp x0,  x1,  [sp, #(0 * 8)]
    ldp x2,  x3,  [sp, #(2 * 8)]
    ldp x4,  x5,  [sp, #(4 * 8)]
    ldp x6,  x7,  [sp, #(6 * 8)]
    ldp x8,  x9,  [sp, #(8 * 8)]
    ldp x10, x11, [sp, #(10 * 8)]
    ldp x12, x13, [sp, #(12 * 8)]
    ldp x14, x15, [sp, #(14 * 8)]
    ldp x16, x17, [sp, #(16 * 8)]
    ldp x18, x30, [sp, #(18 * 8)]
    add sp, sp, #(20 * 8)
    eret
"#
);

/// Lo que llama `vec_irq`. El reparto vive en `irq`.
#[no_mangle]
extern "C" fn irq_rust() {
    // SAFETY: corre con los timbres cerrados por el hardware, asi que nadie mas
    // esta adentro del reparto.
    unsafe { crate::irq::dispatch() }
}

extern "C" {
    static VECTORES: u8;
}

#[no_mangle]
extern "C" fn fault_rust(m: &mut Frame) {
    let esr: u64;
    let far: u64;
    unsafe {
        core::arch::asm!("mrs {}, esr_el1", out(reg) esr, options(nomem, nostack));
        core::arch::asm!("mrs {}, far_el1", out(reg) far, options(nomem, nostack));
    }

    // Los 6 bits de arriba del ESR son la clase de excepcion; los 25 de abajo,
    // el detalle propio de esa clase.
    let ec = esr >> 26;
    let iss = esr & 0x01FF_FFFF;
    let cause = translate(ec);

    // FAR solo tiene sentido cuando la excepcion fue por tocar una direccion.
    let address = matches!(
        cause,
        Cause::PageFault | Cause::InstructionFetch | Cause::Alignment
    )
    .then_some(far);

    // De quien es este fault. Una sola lectura de un registro del CPU.
    let slot = crate::percpu::slot();

    let regs = unsafe {
        let v = &mut (*core::ptr::addr_of_mut!(VALUES))[slot];
        v[..31].copy_from_slice(&m.x);
        v[31] = m.sp;
        v[32] = m.elr;
        v[33] = m.spsr;
        &(*core::ptr::addr_of!(VALUES))[slot]
    };

    let f = Fault { cause, raw: ec, detail: iss, pc: m.elr, address, regs };
    unsafe { (*core::ptr::addr_of_mut!(LAST))[slot] = Some(f) };

    if cause.resumable() {
        // ELR apunta al `brk` mismo: sin correrlo, se reejecuta para siempre.
        // Toda instruccion de aarch64 mide 4 bytes.
        m.elr += 4;
        return;
    }

    // Si hay un `exec` en curso, hay a donde volver: se le cambia el destino al
    // `eret`. En vez de devolverle el control al codigo que fallo, lo devuelve
    // al punto de recuperacion, que le contesta al agente con este fault como
    // dato (P5). Esto es lo que hace que el codigo del agente no pueda matar al
    // kernel.
    if crate::percpu::armed() != 0 {
        m.elr = crate::percpu::return_point();
        // Y con que estado volver. Los cuatro bits de abajo del SPSR son el
        // modo: 0b0000 es EL0 —el agente corriendo supervisado (D27)— y 0b0100
        // es EL1 sobre SP_EL0, que es como corre el codigo `raw`. Los dos hay
        // que llevarlos a 0b0101, EL1 con SP_EL1: la pila del kernel.
        //
        // Se pone el campo entero y no se prende un bit: desde EL0 prender el
        // de abajo daria 0b0001, que no es un modo valido, y el `eret` tomaria
        // el camino de estado de ejecucion ilegal en vez de volver.
        m.spsr = (m.spsr & !0xF) | 0b0101;
        return;
    }

    let mut s = crate::SerialText;
    let _ = s.write_str("\r\n");
    kernel_core::fault::report(&f, REGISTERS, &mut s);
    loop {
        unsafe { core::arch::asm!("msr daifset, #0xf; wfi", options(nomem, nostack)) }
    }
}

/// Traduce la clase de excepcion del ESR.
fn translate(ec: u64) -> Cause {
    match ec {
        0x00 => Cause::Unknown,
        0x0E => Cause::Protection,      // estado de ejecucion ilegal
        0x18 => Cause::InvalidOpcode,   // acceso a un registro de sistema que no toca
        0x20 | 0x21 => Cause::InstructionFetch,
        0x22 => Cause::Alignment,       // PC desalineado
        0x24 | 0x25 => Cause::PageFault, // data abort
        0x26 => Cause::Alignment,       // SP desalineado
        0x3C => Cause::Breakpoint,      // brk
        _ => Cause::Unknown,
    }
}

/// Carga la tabla de vectores.
///
/// # Safety
///
/// Solo desde EL1.
pub unsafe fn install(slot: usize) -> Result<(), &'static str> {
    // El bloque privado primero: el handler lo lee para saber de quien es el
    // fault, asi que tiene que estar puesto antes de que pueda haber uno.
    crate::percpu::install_block(slot);

    let addr = core::ptr::addr_of!(VECTORES) as u64;
    if addr % 2048 != 0 {
        return Err("the vector table did not end up 2048-aligned");
    }
    core::arch::asm!("msr vbar_el1, {}", in(reg) addr, options(nomem, nostack));
    core::arch::asm!("isb", options(nomem, nostack));

    // Se relee, por el mismo motivo que con las tablas de paginas: un install
    // que no hiciera nada se veria igual que uno que anduvo.
    let read_back: u64;
    core::arch::asm!("mrs {}, vbar_el1", out(reg) read_back, options(nomem, nostack));
    if read_back != addr {
        return Err("VBAR_EL1 did not end up pointing at our table");
    }
    Ok(())
}

pub fn breakpoint() {
    unsafe { core::arch::asm!("brk #0", options(nomem, nostack)) }
}

/// El ultimo fault de **este** nucleo.
pub fn last() -> Option<Fault> {
    unsafe { (*core::ptr::addr_of!(LAST))[crate::percpu::slot()] }
}
