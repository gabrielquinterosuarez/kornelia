//! Tocar memoria que la maquina puede rechazar, sin morirse en el intento (P5).
//!
//! Misma idea que en x86_64 —ver el comentario de `kernel-x86_64/src/guarded.rs`
//! para el por que— pero **aca es donde importa de verdad**: un acceso de ancho
//! invalido a un registro de dispositivo en x86_64 devuelve ceros y sigue,
//! mientras que en aarch64 el bus lo rechaza con un abort externo. Sin punto de
//! recuperacion, esa lectura dejaba la maquina muda.
//!
//! El mismo pedido: en una arquitectura miente, en la otra mata. Es exactamente
//! la clase de diferencia por la que D22 pide las dos desde el primer commit.

core::arch::global_asm!(
    r#"
.section .text
.globl guarded_access

// x0 = direccion, x1 = ancho en bytes, x2 = puntero al valor, x3 != 0 si
// escribe. Devuelve 0 si el acceso ocurrio, 1 si la maquina lo rechazo.
//
// El bloque de este nucleo sale de TPIDR_EL1, con los offsets de `PerCpu`.
guarded_access:
    mrs  x9,  tpidr_el1
    adrp x10, guarded_recovery
    add  x10, x10, :lo12:guarded_recovery
    str  x10, [x9, #8]                // rip: adonde volver si falla
    mov  x10, sp
    str  x10, [x9, #16]
    mov  x10, #1
    str  x10, [x9, #0]                // armado

    cbnz x3,  guarded_store

    cmp  x1,  #1
    b.eq guarded_load1
    cmp  x1,  #2
    b.eq guarded_load2
    cmp  x1,  #4
    b.eq guarded_load4
    ldr  x10, [x0]
    b    guarded_loaded
guarded_load1:
    ldrb w10, [x0]
    b    guarded_loaded
guarded_load2:
    ldrh w10, [x0]
    b    guarded_loaded
guarded_load4:
    ldr  w10, [x0]
guarded_loaded:
    str  x10, [x2]
    b    guarded_done

guarded_store:
    ldr  x10, [x2]
    cmp  x1,  #1
    b.eq guarded_store1
    cmp  x1,  #2
    b.eq guarded_store2
    cmp  x1,  #4
    b.eq guarded_store4
    str  x10, [x0]
    b    guarded_done
guarded_store1:
    strb w10, [x0]
    b    guarded_done
guarded_store2:
    strh w10, [x0]
    b    guarded_done
guarded_store4:
    str  w10, [x0]

guarded_done:
    // Que el acceso haya terminado antes de desarmar. Sin esto, un abort que
    // el bus todavia no informo podria llegar con el punto ya desarmado — y
    // entonces mata, que es justo lo que esto viene a evitar.
    dsb sy
    isb
    mrs  x9,  tpidr_el1
    str  xzr, [x9, #0]
    mov  x0,  #0
    ret

guarded_recovery:
    // Aca aterriza el `eret` del handler. La pila se recupera del bloque, igual
    // que hace el punto de recuperacion de `exec`.
    mrs  x9,  tpidr_el1
    ldr  x10, [x9, #16]
    mov  sp,  x10
    str  xzr, [x9, #0]
    mov  x0,  #1
    ret
"#
);

extern "C" {
    fn guarded_access(addr: u64, width: u64, value: *mut u64, write: u64) -> u64;
}

/// Lee `width` bytes. `None` si la maquina rechazo el acceso.
///
/// # Safety
///
/// `addr` tiene que estar mapeada y alineada a `width`. Que el aparato del otro
/// lado acepte el acceso **no** hace falta: de eso se trata.
pub unsafe fn read(addr: u64, width: u64) -> Option<u64> {
    let mut value = 0u64;
    (guarded_access(addr, width, &mut value, 0) == 0).then_some(value)
}

/// Escribe `width` bytes. `false` si la maquina rechazo el acceso.
///
/// # Safety
///
/// Lo mismo que `read`.
pub unsafe fn write(addr: u64, width: u64, mut value: u64) -> bool {
    guarded_access(addr, width, &mut value, 1) == 0
}
