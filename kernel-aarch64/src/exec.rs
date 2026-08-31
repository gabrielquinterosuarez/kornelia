//! Correr codigo del agente en aarch64 (P3, P5).
//!
//! Misma idea que en x86_64 —un punto de retorno armado, y el handler desvia el
//! regreso en vez de detener el nucleo— pero la mecanica cambia: aca el que
//! vuelve es `eret`, y lo que hay que reescribir es `ELR_EL1`.
//!
//! Y hay una diferencia mas: en x86 el marco de interrupcion lleva RSP, asi que
//! el handler podria devolver el control con otra pila. Aca no: una excepcion
//! tomada en EL1 usa el mismo SP, y `eret` lo deja como estaba. Por eso el punto
//! de recuperacion se recupera la pila el mismo, con lo cual las dos
//! arquitecturas terminan haciendo lo mismo.
//!
//! # Limite conocido
//!
//! Igual que en x86: si el codigo del agente rompe SP y despues falla, el
//! handler apila sobre una pila invalida y no hay nada que capturar. La solucion
//! aca es correr el codigo del agente con SP_EL0 y tomar las excepciones con
//! SP_EL1, que ARM tiene bancados justo para esto. Queda anotado como deuda.

use kernel_core::fault::Outcome;

#[no_mangle]
pub static mut EXEC_ARMADO: u64 = 0;

#[no_mangle]
pub static mut EXEC_RIP: u64 = 0;

#[no_mangle]
static mut EXEC_SP: u64 = 0;

/// En el orden de `vectors::REGISTROS`: x0..x30, pc, pstate.
#[no_mangle]
static mut EXEC_REGS: [u64; 33] = [0; 33];

core::arch::global_asm!(
    r#"
.section .text
.globl exec_trampolin

// x0 = direccion de entrada. Devuelve 0 si volvio solo, 1 si hubo fault.
exec_trampolin:
    stp x29, x30, [sp, #-96]!
    stp x19, x20, [sp, #16]
    stp x21, x22, [sp, #32]
    stp x23, x24, [sp, #48]
    stp x25, x26, [sp, #64]
    stp x27, x28, [sp, #80]

    // Se arma el punto de recuperacion ANTES de saltar.
    adrp x9,  exec_recuperacion
    add  x9,  x9, :lo12:exec_recuperacion
    adrp x10, EXEC_RIP
    str  x9,  [x10, :lo12:EXEC_RIP]

    mov  x9,  sp
    adrp x10, EXEC_SP
    str  x9,  [x10, :lo12:EXEC_SP]

    mov  x9,  #1
    adrp x10, EXEC_ARMADO
    str  x9,  [x10, :lo12:EXEC_ARMADO]

    // El codigo recibe en x0 su propia direccion.
    mov  x9,  x0
    blr  x9

    // Volvio solo. Se desarma y se fotografian los registros.
    adrp x10, EXEC_ARMADO
    str  xzr, [x10, :lo12:EXEC_ARMADO]

    adrp x10, EXEC_REGS
    add  x10, x10, :lo12:EXEC_REGS
    stp  x0,  x1,  [x10, #(0 * 8)]
    stp  x2,  x3,  [x10, #(2 * 8)]
    stp  x4,  x5,  [x10, #(4 * 8)]
    stp  x6,  x7,  [x10, #(6 * 8)]
    stp  x8,  x9,  [x10, #(8 * 8)]
    // x10 se esta usando de puntero: se guarda cero en vez de mentir.
    str  xzr,      [x10, #(10 * 8)]
    stp  x11, x12, [x10, #(11 * 8)]
    stp  x13, x14, [x10, #(13 * 8)]
    stp  x15, x16, [x10, #(15 * 8)]
    stp  x17, x18, [x10, #(17 * 8)]
    stp  x19, x20, [x10, #(19 * 8)]
    stp  x21, x22, [x10, #(21 * 8)]
    stp  x23, x24, [x10, #(23 * 8)]
    stp  x25, x26, [x10, #(25 * 8)]
    stp  x27, x28, [x10, #(27 * 8)]
    stp  x29, x30, [x10, #(29 * 8)]
    mov  x9,  sp
    str  x9,       [x10, #(31 * 8)]   // pc: ya volvio, se informa el sp
    mrs  x9,  nzcv
    str  x9,       [x10, #(32 * 8)]

    mov  x0,  #0
    b    exec_salida

exec_recuperacion:
    // Aca aterriza el `eret` del handler cuando hubo fault. La pila del agente
    // puede estar rota, asi que lo primero es recuperar la nuestra.
    adrp x9,  EXEC_SP
    ldr  x9,  [x9, :lo12:EXEC_SP]
    mov  sp,  x9
    adrp x10, EXEC_ARMADO
    str  xzr, [x10, :lo12:EXEC_ARMADO]
    mov  x0,  #1

exec_salida:
    ldp  x19, x20, [sp, #16]
    ldp  x21, x22, [sp, #32]
    ldp  x23, x24, [sp, #48]
    ldp  x25, x26, [sp, #64]
    ldp  x27, x28, [sp, #80]
    ldp  x29, x30, [sp], #96
    ret
"#
);

extern "C" {
    fn exec_trampolin(entry: u64) -> u64;
}

/// Salta al codigo y vuelve con lo que haya pasado.
///
/// # Safety
///
/// `entry` tiene que apuntar a memoria mapeada y ejecutable.
pub unsafe fn run(entry: u64) -> Outcome {
    let hubo_fault = exec_trampolin(entry) != 0;

    if hubo_fault {
        // El handler ya dejo anotado el fault, con los registros del momento
        // exacto en que fallo — que son mas utiles que los de ahora.
        let f = crate::vectors::last();
        Outcome {
            faulted: true,
            regs: f.map(|f| f.regs).unwrap_or(&[]),
            fault: f,
        }
    } else {
        Outcome {
            faulted: false,
            regs: &*core::ptr::addr_of!(EXEC_REGS),
            fault: None,
        }
    }
}

/// Empuja el codigo recien escrito hasta donde lo ve el camino de instrucciones.
///
/// En aarch64 la cache de datos y la de instrucciones **no** son coherentes: lo
/// que se escribio como dato puede seguir en la cache de datos mientras el
/// camino de instrucciones lee de memoria lo que habia antes. Saltar ahi seria
/// ejecutar basura vieja, y el sintoma no se parece en nada a la causa.
///
/// x86_64 no necesita nada de esto: el hardware mantiene la coherencia solo.
///
/// # Safety
///
/// El rango tiene que estar mapeado.
pub unsafe fn sincronizar_cache(start: u64, bytes: u64) {
    // El tamano de linea de cada cache lo informa el propio CPU (P4). No se
    // hornea: varia entre implementaciones.
    let ctr: u64;
    core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack));
    let linea_datos = 4u64 << ((ctr >> 16) & 0xF);
    let linea_instr = 4u64 << (ctr & 0xF);

    let fin = start.saturating_add(bytes);

    // Primero limpiar la de datos hasta el punto de unificacion, que es donde
    // las dos se encuentran.
    let mut a = start & !(linea_datos - 1);
    while a < fin {
        core::arch::asm!("dc cvau, {}", in(reg) a, options(nostack));
        a += linea_datos;
    }
    core::arch::asm!("dsb ish", options(nostack));

    // Y recien despues invalidar la de instrucciones, para que vaya a buscar lo
    // nuevo. Al reves no serviria: invalidaria y volveria a traer lo viejo.
    let mut a = start & !(linea_instr - 1);
    while a < fin {
        core::arch::asm!("ic ivau, {}", in(reg) a, options(nostack));
        a += linea_instr;
    }
    core::arch::asm!("dsb ish", options(nostack));
    core::arch::asm!("isb", options(nostack));
}
