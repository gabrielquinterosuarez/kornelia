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
//! # Tres pilas (P5)
//!
//! El codigo del agente corre en su propia pila, no en la del kernel. Y las
//! excepciones entran en una **tercera**, la de la IST, a la que el CPU cambia
//! *antes* de apilar el marco (ver `gdt`).
//!
//! Sin eso, el agente podia matar la maquina sin siquiera querer: bastaba con
//! romper RSP y despues fallar, porque el marco de excepcion se apilaba sobre
//! esa pila rota y eso escalaba a doble y triple fault.
//!
//! # Y las tres son por nucleo
//!
//! Todo el estado de aca vive en el bloque privado de cada nucleo, alcanzable
//! por `gs:` (ver `percpu`). Compartirlo seria que dos nucleos corriendo codigo
//! del agente a la vez se pisen el punto de retorno — y volver al punto de
//! retorno del otro es una forma particularmente confusa de fallar.

use kernel_core::fault::Outcome;

/// 64 KiB de pila para el codigo del agente de cada nucleo.
const STACK_SIZE: usize = 64 * 1024;

#[repr(C, align(16))]
struct Stacks([[u8; STACK_SIZE]; crate::percpu::SLOTS]);

static mut AGENT_STACKS: Stacks = Stacks([[0; STACK_SIZE]; crate::percpu::SLOTS]);

core::arch::global_asm!(
    r#"
.section .text
.globl exec_trampoline

// rdi = direccion de entrada. Devuelve 0 si volvio solo, 1 si hubo fault.
//
// Todo lo que toca esta en el bloque privado de este nucleo, que se alcanza con
// el prefijo `gs:`. Los numeros son los offsets del struct `PerCpu`, y estan
// verificados en tiempo de compilacion del lado de Rust.
//
// OJO: el argumento va en RDI porque esto se declara `extern "sysv64"` del lado
// de Rust. En el target `x86_64-unknown-uefi`, `extern "C"` NO es System V sino
// la ABI de Windows, donde el primer argumento va en RCX. Declararlo "C" hace
// que se salte a lo que hubiera en RDI, que es basura.
exec_trampoline:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15

    // Se arma el punto de recuperacion ANTES de saltar.
    lea rax, [rip + exec_recovery]
    mov gs:[8], rax                    // rip
    mov gs:[16], rsp                   // rsp del kernel
    mov qword ptr gs:[0], 1            // armado

    // El codigo recibe en rdi su propia direccion, para poder encontrar sus
    // datos sin depender de donde lo hayan cargado.
    mov rax, rdi
    // Y corre en su propia pila: si la rompe, la del kernel queda entera.
    mov rsp, gs:[24]
    call rax

    // Volvio solo. El punto de recuperacion sigue armado mientras se toma la
    // foto: `pushfq` de mas abajo tambien apila, y si la pila del agente quedo
    // rota eso falla y se captura como cualquier otro fault.
    mov gs:[32],  rax                  // regs[0]
    mov gs:[40],  rbx
    mov gs:[48],  rcx
    mov gs:[56],  rdx
    mov gs:[64],  rsi
    mov gs:[72],  rdi
    mov gs:[80],  rbp
    mov gs:[88],  rsp                  // el rsp con el que quedo el agente
    mov gs:[96],  r8
    mov gs:[104], r9
    mov gs:[112], r10
    mov gs:[120], r11
    mov gs:[128], r12
    mov gs:[136], r13
    mov gs:[144], r14
    mov gs:[152], r15
    // El codigo ya volvio: no hay un "donde estaba ejecutando" que informar.
    mov qword ptr gs:[160], 0
    pushfq
    pop rax
    mov gs:[168], rax

    // Recien ahora se vuelve a la pila del kernel y se desarma.
    mov rsp, gs:[16]
    mov qword ptr gs:[0], 0
    xor eax, eax
    jmp exec_exit

exec_recovery:
    // Aca aterriza el `iretq` del handler cuando hubo fault. La pila del agente
    // puede estar rota, asi que lo primero es recuperar la nuestra — y eso se
    // puede hacer porque `gs:` no depende de la pila.
    mov rsp, gs:[16]
    mov qword ptr gs:[0], 0
    mov eax, 1

exec_exit:
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
    fn exec_trampoline(entry: u64) -> u64;
}

/// Salta al codigo y vuelve con lo que haya pasado.
///
/// # Safety
///
/// `entry` tiene que apuntar a memoria mapeada y ejecutable. Corre en el nucleo
/// que la llama, sobre la pila de agente de ese nucleo.
pub unsafe fn run(entry: u64) -> Outcome {
    let slot = crate::percpu::slot();
    let block = crate::percpu::block(slot);

    // La pila de agente de este nucleo. Se pone en cada llamada y no una vez al
    // arrancar: es barato, y asi no hay un orden de inicializacion que recordar.
    (*block).stack =
        core::ptr::addr_of!(AGENT_STACKS) as u64 + ((slot + 1) * STACK_SIZE) as u64;

    let had_fault = exec_trampoline(entry) != 0;

    if had_fault {
        // El handler ya dejo anotado el fault, con los registros del momento
        // exacto en que fallo — que son mas utiles que los de ahora.
        let f = crate::idt::last();
        Outcome { faulted: true, regs: f.map(|f| f.regs).unwrap_or(&[]), fault: f }
    } else {
        Outcome {
            faulted: false,
            regs: core::slice::from_raw_parts(
                core::ptr::addr_of!((*block).regs) as *const u64,
                18,
            ),
            fault: None,
        }
    }
}
