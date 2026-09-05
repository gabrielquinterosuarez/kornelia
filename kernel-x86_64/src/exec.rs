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

/// El vector de la ventanilla: la puerta por la que el codigo `supervised`
/// vuelve al kernel (D27).
///
/// Desde anillo 3 un `ret` no vuelve — no hay a donde: se entro por un `iretq`
/// y no por un `call`. La unica forma de subir de privilegio es un trap, asi
/// que se le deja una compuerta con `DPL=3` para que el agente pueda invocarla
/// con `int`. Cual es el numero no lo tiene que saber: lo publica `describe`
/// como los bytes exactos a emitir (P4).
///
/// El 0x80 esta lejos de los que reparte `irq`, que van del 0x30 para arriba.
pub const WINDOW_VECTOR: usize = 0x80;

/// Los bytes de `int 0x80`, que es lo que `describe` publica.
pub const RETURN_BYTES: &[u8] = &[0xCD, 0x80];

/// La otra puerta: **atendeme esto y devolveme el control**.
///
/// Va aparte de la de retorno y no distinguida por un registro, por dos
/// razones. Una: el codigo que vuelve ya usa `rax` para dejar su resultado, y
/// robarselo cambiaria una convencion que ya existe. Dos: dos puertas con dos
/// significados se leen; un registro con dos significados hay que explicarlo.
pub const SERVICE_VECTOR: usize = 0x81;

/// Los bytes de `int 0x81`. Tambien se publican (P4): el agente no tiene que
/// saber que esto es un `int`, solo que emitiendo estos bytes le contestan.
pub const SERVICE_BYTES: &[u8] = &[0xCD, 0x81];

core::arch::global_asm!(
    r#"
.section .text
.globl exec_trampoline

// rdi = direccion de entrada, rsi = 1 si va supervisado.
// Devuelve 0 si volvio solo, 1 si hubo fault.
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

    test rsi, rsi
    jnz exec_supervised

    // Corre en su propia pila: si la rompe, la del kernel queda entera.
    mov rsp, gs:[24]
    // Y con los registros que pidio el agente. Se cargan **todos** desde el
    // bloque —el de Rust ya resolvio cual queda en cero y cual lleva la
    // direccion de entrada— asi que despues de esto no queda ninguno libre:
    // por eso el salto sale del bloque y no de un registro.
    //
    // rsp no se carga: la pila la pone el kernel y lo publica `describe`.
    mov rax, gs:[32]
    mov rbx, gs:[40]
    mov rcx, gs:[48]
    mov rdx, gs:[56]
    mov rsi, gs:[64]
    mov rdi, gs:[72]
    mov rbp, gs:[80]
    mov r8,  gs:[96]
    mov r9,  gs:[104]
    mov r10, gs:[112]
    mov r11, gs:[120]
    mov r12, gs:[128]
    mov r13, gs:[136]
    mov r14, gs:[144]
    mov r15, gs:[152]
    call qword ptr gs:[184]

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

exec_supervised:
    // Bajar de privilegio no es un salto: es volver de una interrupcion que
    // nunca ocurrio. Asi que se arma a mano el marco que el `iretq` consume, y
    // el CPU "vuelve" a anillo 3 (D27).
    //
    // El orden es el que el hardware desapila: SS y RSP arriba de todo, y la
    // direccion de entrada al final.
    push 0x23                          // SS  = datos de anillo 3
    push qword ptr gs:[24]             // el puntero de pila del agente
    push 0x202                         // RFLAGS: bit 1 fijo en uno, y el 9
                                       // —interrupciones— prendido (D29). IOPL
                                       // queda en cero: sin puertos de E/S.
    push 0x1b                          // CS  = codigo de anillo 3
    push rdi                           // la direccion de entrada
    // El marco ya esta armado, asi que recien ahora se cargan los registros
    // que pidio el agente: cargarlos antes los habria pisado el `push rdi`.
    // Los `push` de arriba mueven rsp y memoria, no los registros, asi que el
    // `iretq` sigue encontrando su marco donde lo dejo.
    mov rax, gs:[32]
    mov rbx, gs:[40]
    mov rcx, gs:[48]
    mov rdx, gs:[56]
    mov rsi, gs:[64]
    mov rdi, gs:[72]
    mov rbp, gs:[80]
    mov r8,  gs:[96]
    mov r9,  gs:[104]
    mov r10, gs:[112]
    mov r11, gs:[120]
    mov r12, gs:[128]
    mov r13, gs:[136]
    mov r14, gs:[144]
    mov r15, gs:[152]
    iretq

.globl exec_window
// La ventanilla: llega un `int 0x80` desde el codigo del agente. Ya corre en
// anillo 0, sobre la pila de `RSP0`, y con las interrupciones cerradas porque
// la compuerta es de interrupcion y no de trap.
//
// No vuelve por `iretq`: salta al mismo lugar donde termina un `exec` que
// volvio solo. La pila de la ventanilla se abandona, que no cuesta nada — el
// CPU la vuelve a tomar desde arriba la proxima vez.
exec_window:
    // Sin `exec` en curso no hay a donde volver. Puede pasar: un handler del
    // agente corre privilegiado (D27) y podria ejecutar esto. Se vuelve como si
    // nada en vez de saltar a una pila vieja.
    cmp qword ptr gs:[0], 0
    je 3f

    mov gs:[32],  rax                  // regs[0]
    mov gs:[40],  rbx
    mov gs:[48],  rcx
    mov gs:[56],  rdx
    mov gs:[64],  rsi
    mov gs:[72],  rdi
    mov gs:[80],  rbp
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
    // El puntero de pila y las banderas del agente no estan en sus registros:
    // los tiene el marco que apilo el CPU al entrar. Desde rsp: rip, cs,
    // rflags, rsp, ss.
    mov rax, [rsp + 16]
    mov gs:[168], rax                  // regs[17] = rflags
    mov rax, [rsp + 24]
    mov gs:[88], rax                   // regs[7]  = el rsp del agente

    mov rsp, gs:[16]
    mov qword ptr gs:[0], 0
    xor eax, eax
    jmp exec_exit
3:
    iretq

.globl exec_service
// La otra ventanilla: el codigo del agente pide un verbo y **sigue corriendo**.
//
// A diferencia de `exec_window`, esta SI vuelve por `iretq`: el codigo retoma
// en la instruccion siguiente, que es lo que hace cualquier llamada al sistema.
// La de al lado es la rara — termina el `exec` en vez de volver.
//
// Los cuatro argumentos ya vienen donde la ABI de este target los pone (rcx,
// rdx, r8, r9), asi que no hay que moverlos. Lo unico que hay que cuidar es no
// pisarle al agente los registros que la ABI deja en manos del que llama.
exec_service:
    push rax
    push r10
    push r11
    // La ABI de este target (UEFI, o sea la de Windows) exige 32 bytes de pila
    // vacia antes de la llamada. Sin eso, la funcion escribe donde no debe.
    sub rsp, 40
    call exec_service_rust
    add rsp, 40
    // El resultado queda en rax, pero rax se restaura abajo: se guarda en el
    // lugar de la pila donde estaba el rax viejo, asi el agente lo recibe.
    mov [rsp + 16], rax
    pop r11
    pop r10
    pop rax
    iretq

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
    fn exec_trampoline(entry: u64, supervised: u64) -> u64;
    /// La ventanilla. La engancha `idt` con una compuerta de `DPL=3`.
    pub fn exec_window();
    /// Y la de servicio, con la misma compuerta pero que vuelve al agente.
    pub fn exec_service();
}

// El ensamblador de arriba lleva los selectores escritos a mano, porque un
// `push` no toma una constante de Rust. Esto los ata a la GDT: si alguien
// mueve un descriptor, no compila en vez de saltar a anillo 3 con un selector
// que apunta a otra cosa.
const _: () = assert!(crate::gdt::CODE_USER == 0x1b);
const _: () = assert!(crate::gdt::DATA_USER == 0x23);

/// Salta al codigo y vuelve con lo que haya pasado.
///
/// # Safety
///
/// `entry` tiene que apuntar a memoria mapeada y ejecutable. Corre en el nucleo
/// que la llama, sobre la pila de agente de ese nucleo — o, si va supervisado,
/// sobre el final de `region`.
/// Los registros que el agente puede poner al arrancar (D3, P4).
///
/// Son los de proposito general y nada mas. `rip` queda afuera porque donde
/// empieza a ejecutar lo dice `off`; `rsp` porque la pila la pone el kernel y
/// lo publica `describe`; y `rflags` porque no es un valor que se cargue sino
/// consecuencia de como se entra — en `supervised` lleva las interrupciones
/// prendidas a proposito (D29), y dejar que el agente lo pisara seria darle por
/// la ventana lo que D29 le niega por la puerta.
pub const INITIAL: &[&str] = &[
    "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];

/// Los registros por los que pasan los argumentos en esta arquitectura, como
/// indices dentro de `REGISTERS`, que es como viene `initial`.
///
/// **RCX y RDX, no RDI y RSI.** El kernel se compila para UEFI, y ahi la ABI de
/// C no es la de Linux sino la de Windows, que pasa los argumentos por otros
/// registros. Importa porque asi el codigo del agente puede ser una funcion
/// compilada para este mismo target y recibir lo que el kernel le pasa sin
/// traduccion.
/// A donde va un pedido que entra por la ventanilla de servicio.
///
/// Es un puntero y no una llamada directa porque quien atiende es generico
/// sobre la arquitectura, y un stub de ensamblador no puede nombrar algo
/// generico. Se fija una vez, al arrancar.
static mut SERVICE: usize = 0;

/// Deja dicho quien atiende los pedidos de la ventanilla de servicio.
///
/// # Safety
///
/// `addr` tiene que ser una `extern "C" fn(*const u8, usize, *mut u8, usize)
/// -> usize` viva.
pub unsafe fn set_service(addr: u64) {
    SERVICE = addr as usize;
}

/// Lo que llama el stub. Si nadie dejo a quien llamar, contesta que no hay
/// nada — que es mejor que saltar a cero.
#[no_mangle]
extern "C" fn exec_service_rust(req: *const u8, len: usize, out: *mut u8, cap: usize) -> usize {
    let who = unsafe { SERVICE };
    if who == 0 {
        return 0;
    }
    // SAFETY: lo dejo `set_service`, y apunta a una funcion del kernel.
    let who: extern "C" fn(*const u8, usize, *mut u8, usize) -> usize =
        unsafe { core::mem::transmute(who) };
    who(req, len, out, cap)
}

pub const ARGUMENTS: &[usize] = &[2, 3]; // rcx, rdx

/// El primero de esos: donde `exec` deja la direccion de entrada.
const FIRST_ARGUMENT: usize = ARGUMENTS[0];

pub unsafe fn run(
    entry: u64,
    region: (u64, u64),
    supervised: bool,
    initial: &[Option<u64>],
) -> Outcome {
    let slot = crate::percpu::slot();
    let block = crate::percpu::block(slot);

    // Los valores con los que arranca. El que no pidio queda en cero, salvo el
    // primer argumento: ahi va la direccion de entrada, para que el codigo
    // pueda encontrar sus datos sin depender de donde lo hayan cargado. Si el
    // agente **si** lo puso, gana el agente: es su codigo (P2).
    for i in 0..(*block).regs.len() {
        (*block).regs[i] = match initial.get(i).copied().flatten() {
            Some(v) => v,
            None if i == FIRST_ARGUMENT => entry,
            None => 0,
        };
    }
    (*block).entry = entry;

    // De donde sale la pila. Se pone en cada llamada y no una vez al arrancar:
    // es barato, y asi no hay un orden de inicializacion que recordar.
    (*block).stack = if supervised {
        // En anillo 3 la pila del kernel no se puede ni escribir, asi que la
        // pila es el final del reclamo del propio agente (D27). Alineada a 16,
        // que es lo que pide la ABI.
        region.0.saturating_add(region.1) & !0xF
    } else {
        core::ptr::addr_of!(AGENT_STACKS) as u64 + ((slot + 1) * STACK_SIZE) as u64
    };

    // Un pedido de corte que quedo de antes no vale para este trabajo: se
    // limpia al empezar, o el primer `exec` nuevo se cortaria solo.
    kernel_core::work::clear_cancel(slot);

    let diverted = exec_trampoline(entry, supervised as u64) != 0;

    // El desvio es el mismo camino para las dos cosas —un fault y un corte
    // aterrizan en el mismo punto de recuperacion— asi que hay que preguntar
    // cual fue. Se pregunta primero por el corte porque es el que tiene una
    // marca propia: un fault no la deja.
    if diverted && kernel_core::work::was_cancelled(slot) {
        Outcome {
            faulted: false,
            cancelled: true,
            // Los registros del momento en que se lo corto. No hay fault, asi
            // que no hay un juego mejor que este.
            regs: core::slice::from_raw_parts(
                core::ptr::addr_of!((*block).regs) as *const u64,
                18,
            ),
            fault: None,
        }
    } else if diverted {
        // El handler ya dejo anotado el fault, con los registros del momento
        // exacto en que fallo — que son mas utiles que los de ahora.
        let f = crate::idt::last();
        Outcome {
            faulted: true,
            cancelled: false,
            regs: f.map(|f| f.regs).unwrap_or(&[]),
            fault: f,
        }
    } else {
        Outcome {
            faulted: false,
            cancelled: false,
            regs: core::slice::from_raw_parts(
                core::ptr::addr_of!((*block).regs) as *const u64,
                18,
            ),
            fault: None,
        }
    }
}
