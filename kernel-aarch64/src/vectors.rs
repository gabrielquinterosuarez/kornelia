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
pub struct Marco {
    /// x0 a x30.
    x: [u64; 31],
    /// Donde volver. En una excepcion sincronica apunta a la instruccion que
    /// fallo, no a la siguiente.
    elr: u64,
    /// El estado del procesador al momento de fallar.
    spsr: u64,
    _relleno: u64,
}

pub const REGISTROS: &[&str] = &[
    "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13",
    "x14", "x15", "x16", "x17", "x18", "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26",
    "x27", "x28", "x29", "x30", "pc", "pstate",
];

static mut VALORES: [u64; REGISTROS.len()] = [0; REGISTROS.len()];
static mut ULTIMO: Option<Fault> = None;

core::arch::global_asm!(
    r#"
.section .text

// Cada entrada de la tabla son 128 bytes. Con saltar al handler comun alcanza:
// de que se trata lo dice ESR_EL1, no la posicion.
.macro ENTRADA
    b vec_common
    .balign 0x80
.endm

// La tabla entera tiene que estar alineada a 2048.
.balign 2048
.globl VECTORES
VECTORES:
    ENTRADA   // mismo EL, SP0:  sincronica
    ENTRADA   //                 IRQ
    ENTRADA   //                 FIQ
    ENTRADA   //                 SError
    ENTRADA   // mismo EL, SPx:  sincronica   <- por aca entra lo nuestro
    ENTRADA   //                 IRQ
    ENTRADA   //                 FIQ
    ENTRADA   //                 SError
    ENTRADA   // EL mas bajo, 64 bits: sincronica
    ENTRADA   //                       IRQ
    ENTRADA   //                       FIQ
    ENTRADA   //                       SError
    ENTRADA   // EL mas bajo, 32 bits: sincronica
    ENTRADA   //                       IRQ
    ENTRADA   //                       FIQ
    ENTRADA   //                       SError

vec_common:
    // 34 huecos de 8 bytes: 31 registros, ELR, SPSR y uno de relleno para que
    // la pila quede alineada a 16, que ARM exige.
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
    stp x0, x1,   [sp, #(31 * 8)]

    mov x0, sp
    bl fault_rust

    // El handler puede haber corrido ELR para saltearse la instruccion.
    ldp x0, x1,   [sp, #(31 * 8)]
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
"#
);

extern "C" {
    static VECTORES: u8;
}

#[no_mangle]
extern "C" fn fault_rust(m: &mut Marco) {
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
    let cause = traducir(ec);

    // FAR solo tiene sentido cuando la excepcion fue por tocar una direccion.
    let address = matches!(
        cause,
        Cause::PageFault | Cause::InstructionFetch | Cause::Alignment
    )
    .then_some(far);

    let regs = unsafe {
        let v = &mut *core::ptr::addr_of_mut!(VALORES);
        v[..31].copy_from_slice(&m.x);
        v[31] = m.elr;
        v[32] = m.spsr;
        &*core::ptr::addr_of!(VALORES)
    };

    let f = Fault { cause, raw: ec, detail: iss, pc: m.elr, address, regs };
    unsafe { ULTIMO = Some(f) };

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
    if unsafe { crate::exec::EXEC_ARMADO } != 0 {
        m.elr = unsafe { crate::exec::EXEC_RIP };
        return;
    }

    let mut s = crate::Serie;
    let _ = s.write_str("\r\n");
    kernel_core::fault::report(&f, REGISTROS, &mut s);
    loop {
        unsafe { core::arch::asm!("msr daifset, #0xf; wfi", options(nomem, nostack)) }
    }
}

/// Traduce la clase de excepcion del ESR.
fn traducir(ec: u64) -> Cause {
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
pub unsafe fn install() -> Result<(), &'static str> {
    let dir = core::ptr::addr_of!(VECTORES) as u64;
    if dir % 2048 != 0 {
        return Err("la tabla de vectores no quedo alineada a 2048");
    }
    core::arch::asm!("msr vbar_el1, {}", in(reg) dir, options(nomem, nostack));
    core::arch::asm!("isb", options(nomem, nostack));

    // Se relee, por el mismo motivo que con las tablas de paginas: un install
    // que no hiciera nada se veria igual que uno que anduvo.
    let puesto: u64;
    core::arch::asm!("mrs {}, vbar_el1", out(reg) puesto, options(nomem, nostack));
    if puesto != dir {
        return Err("VBAR_EL1 no quedo apuntando a nuestra tabla");
    }
    Ok(())
}

pub fn breakpoint() {
    unsafe { core::arch::asm!("brk #0", options(nomem, nostack)) }
}

pub fn last() -> Option<Fault> {
    unsafe { ULTIMO }
}
