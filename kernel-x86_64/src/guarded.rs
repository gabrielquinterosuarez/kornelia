//! Tocar memoria que la maquina puede rechazar, sin morirse en el intento (P5).
//!
//! # Que problema resuelve
//!
//! `mem.read` y `mem.write` pueden apuntar al registro de un dispositivo, y un
//! dispositivo **puede rechazar el acceso**: un registro que solo acepta cuatro
//! bytes, leido de a uno, es un acceso invalido. En x86_64 eso devuelve ceros y
//! sigue; en aarch64 el bus lo rechaza con un abort y la maquina queda muda.
//!
//! Y ese acceso lo hace el **kernel**, en el camino del protocolo. Durante un
//! `exec` un fault vuelve como dato porque hay un punto de recuperacion armado;
//! aca no habia ninguno, asi que el agente podia quedarse sin cordon con un
//! pedido legitimo — justo lo que D5 y D17 dicen que no puede pasar.
//!
//! # Como
//!
//! Con la misma maquinaria de `exec`, usada afuera de `exec`: se arma el punto
//! de recuperacion en el bloque de este nucleo, se hace **un** acceso, y se
//! desarma. Si la maquina lo rechaza, el handler desvia el regreso al punto de
//! recuperacion igual que haria con codigo del agente.
//!
//! La ventana armada es de una sola instruccion, a proposito: cuanto mas corta,
//! menos posibilidades de que capture un fault que no era el que se esperaba.

core::arch::global_asm!(
    r#"
.section .text
.globl guarded_access

// rdi = direccion, rsi = ancho en bytes, rdx = puntero al valor, rcx != 0 si
// escribe. Devuelve 0 si el acceso ocurrio, 1 si la maquina lo rechazo.
//
// `extern "sysv64"` del lado de Rust, y el bloque de este nucleo se alcanza por
// `gs:` con los offsets de `PerCpu`, verificados al compilar.
guarded_access:
    lea rax, [rip + guarded_recovery]
    mov gs:[8], rax                    // rip: adonde volver si falla
    mov gs:[16], rsp
    mov qword ptr gs:[0], 1            // armado

    test rcx, rcx
    jnz guarded_store

    cmp rsi, 1
    je guarded_load1
    cmp rsi, 2
    je guarded_load2
    cmp rsi, 4
    je guarded_load4
    mov rax, [rdi]
    jmp guarded_loaded
guarded_load1:
    movzx rax, byte ptr [rdi]
    jmp guarded_loaded
guarded_load2:
    movzx rax, word ptr [rdi]
    jmp guarded_loaded
guarded_load4:
    mov eax, [rdi]
guarded_loaded:
    mov [rdx], rax
    jmp guarded_done

guarded_store:
    mov rax, [rdx]
    cmp rsi, 1
    je guarded_store1
    cmp rsi, 2
    je guarded_store2
    cmp rsi, 4
    je guarded_store4
    mov [rdi], rax
    jmp guarded_done
guarded_store1:
    mov [rdi], al
    jmp guarded_done
guarded_store2:
    mov [rdi], ax
    jmp guarded_done
guarded_store4:
    mov [rdi], eax

guarded_done:
    mov qword ptr gs:[0], 0
    xor rax, rax
    ret

guarded_recovery:
    // Aca aterriza el `iretq` del handler. La pila se recupera del bloque
    // porque el acceso pudo haber entrado por una pila de excepcion.
    mov rsp, gs:[16]
    mov qword ptr gs:[0], 0
    mov rax, 1
    ret
"#
);

extern "sysv64" {
    fn guarded_access(addr: u64, width: u64, value: *mut u64, write: u64) -> u64;
}

/// Lee `width` bytes. `None` si la maquina rechazo el acceso.
///
/// El detalle de por que lo rechazo queda donde queda cualquier fault, y se
/// pide con `Platform::last_fault`: un acceso rechazado es un fault como
/// cualquier otro, y tener dos caminos para contar lo mismo seria tener dos
/// formatos que se van separando.
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
