//! Correr codigo del agente en x86_64 (P3, P5).
//!
//! # El problema
//!
//! El codigo que sube el agente puede estar mal — es un generador estocastico
//! de codigo maquina, va a estar mal seguido. Saltar ahi sin mas significa que
//! el primer error se lleva puesto el kernel y el agente no se entera de nada.
//!
//! # La solucion: un punto de retorno armado
//!
//! Antes de saltar se anota **a donde volver** si algo falla. Cuando el handler
//! de excepciones ve que hay un punto armado, en vez de detener el nucleo
//! reescribe la direccion de retorno del marco de interrupcion: el `iretq` que
//! iba a devolver el control al codigo que fallo lo devuelve al punto de
//! recuperacion.
//!
//! Es la misma idea que un `setjmp`/`longjmp`, pero sin necesidad de uno: el
//! salto ya lo hace el `iretq`, solo hay que cambiarle el destino.
//!
//! # Limite conocido
//!
//! Si el codigo del agente rompe RSP y despues falla, el CPU intenta apilar el
//! marco de excepcion sobre una pila invalida y eso escala a doble y triple
//! fault: la maquina se reinicia y no hay nada que capturar. La solucion es una
//! pila de excepcion aparte (IST), que necesita GDT y TSS propios. Queda
//! anotado como deuda.

use kernel_core::fault::Outcome;

/// El punto de recuperacion esta armado. Lo lee el handler.
#[no_mangle]
pub static mut EXEC_ARMADO: u64 = 0;

/// A donde saltar si hay fault. Lo escribe el trampolin, lo lee el handler.
#[no_mangle]
pub static mut EXEC_RIP: u64 = 0;

/// La pila del kernel, para recuperarla: la del agente puede estar en cualquier
/// lado cuando falle.
#[no_mangle]
static mut EXEC_RSP: u64 = 0;

/// Los registros tal como quedaron al volver bien. En el orden de
/// `idt::REGISTROS`.
#[no_mangle]
static mut EXEC_REGS: [u64; 18] = [0; 18];

/// 64 KiB de pila para el codigo del agente.
///
/// Aparte de la del kernel a proposito: el agente puede desbordarla o dejarla
/// en cualquier lado sin llevarse puesto nada nuestro. Y como las excepciones
/// entran por la IST, romperla tampoco impide capturar el fault.
const TAM_PILA_AGENTE: usize = 64 * 1024;

#[repr(C, align(16))]
struct PilaAgente([u8; TAM_PILA_AGENTE]);

static mut PILA_AGENTE: PilaAgente = PilaAgente([0; TAM_PILA_AGENTE]);

/// La cima de esa pila, que es por donde se empieza.
#[no_mangle]
static mut EXEC_PILA: u64 = 0;

core::arch::global_asm!(
    r#"
.section .text
.globl exec_trampolin

// rdi = direccion de entrada. Devuelve 0 si volvio solo, 1 si hubo fault.
//
// OJO: el argumento va en RDI porque esto se declara `extern "sysv64"` del lado
// de Rust. En el target `x86_64-unknown-uefi`, `extern "C"` NO es System V sino
// la ABI de Windows, donde el primer argumento va en RCX. Declararlo "C" hace
// que se salte a lo que hubiera en RDI, que es basura.
exec_trampolin:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15

    // Se arma el punto de recuperacion ANTES de saltar.
    lea rax, [rip + exec_recuperacion]
    mov qword ptr [rip + EXEC_RIP], rax
    mov qword ptr [rip + EXEC_RSP], rsp
    mov qword ptr [rip + EXEC_ARMADO], 1

    // El codigo recibe en rdi su propia direccion, para poder encontrar sus
    // datos sin depender de donde lo hayan cargado.
    mov rax, rdi
    // Y corre en su propia pila: si la rompe, la del kernel queda entera.
    mov rsp, qword ptr [rip + EXEC_PILA]
    call rax

    // Volvio solo. El punto de recuperacion sigue armado mientras se toma la
    // foto, porque la foto tambien apila: si la pila del agente quedo rota,
    // este push falla y se captura como cualquier otro fault.
    push rax
    lea rax, [rip + EXEC_REGS]
    mov [rax + 8],   rbx
    mov [rax + 16],  rcx
    mov [rax + 24],  rdx
    mov [rax + 32],  rsi
    mov [rax + 40],  rdi
    mov [rax + 48],  rbp
    mov [rax + 64],  r8
    mov [rax + 72],  r9
    mov [rax + 80],  r10
    mov [rax + 88],  r11
    mov [rax + 96],  r12
    mov [rax + 104], r13
    mov [rax + 112], r14
    mov [rax + 120], r15
    // rcx ya quedo guardado, asi que sirve de andamio para el rax de verdad.
    pop rcx
    mov [rax + 0],   rcx
    // El rsp con el que quedo el agente, no el nuestro.
    mov [rax + 56],  rsp
    // El codigo ya volvio: no hay un "donde estaba ejecutando" que informar.
    mov qword ptr [rax + 128], 0
    pushfq
    pop rdx
    mov [rax + 136], rdx

    // Recien ahora se vuelve a la pila del kernel y se desarma.
    mov rsp, qword ptr [rip + EXEC_RSP]
    mov qword ptr [rip + EXEC_ARMADO], 0
    xor eax, eax
    jmp exec_salida

exec_recuperacion:
    // Aca aterriza el `iretq` del handler cuando hubo fault. La pila del agente
    // puede estar rota, asi que lo primero es recuperar la nuestra.
    mov rsp, qword ptr [rip + EXEC_RSP]
    mov qword ptr [rip + EXEC_ARMADO], 0
    mov eax, 1

exec_salida:
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret
"#
);

extern "sysv64" {
    fn exec_trampolin(entry: u64) -> u64;
}

/// Salta al codigo y vuelve con lo que haya pasado.
///
/// # Safety
///
/// `entry` tiene que apuntar a memoria mapeada y ejecutable.
pub unsafe fn run(entry: u64) -> Outcome {
    EXEC_PILA = core::ptr::addr_of!(PILA_AGENTE) as u64 + TAM_PILA_AGENTE as u64;

    let hubo_fault = exec_trampolin(entry) != 0;

    if hubo_fault {
        // El handler ya dejo anotado el fault, con los registros del momento
        // exacto en que fallo — que son mas utiles que los de ahora.
        let f = crate::idt::last();
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
