//! El timbre del cable serie en x86_64.
//!
//! # Las dos mitades del APIC
//!
//! El controlador de interrupciones de x86 viene en dos partes:
//!
//! - El **APIC local**, uno por nucleo, pegado al nucleo. Es el que le toca el
//!   timbre y al que hay que decirle "ya atendi".
//! - El **IO-APIC**, uno por maquina. Es la central: recibe los cables de los
//!   aparatos y decide a que nucleo mandar cada uno, y con que numero de timbre.
//!
//! Programar el ruteo es escribir una entrada del IO-APIC: "cuando suene el
//! cable numero N, tocale el timbre V al nucleo tal".
//!
//! # De donde sale el numero de cable del serie
//!
//! El puerto serie de la PC original es el cable 4. Ese numero es una convencion
//! de la plataforma, igual que la direccion `0x3F8` de sus registros — no lo
//! informa ACPI. Pero la maquina **si** puede haberlo movido de lugar, y eso lo
//! dice la MADT: por eso se pregunta con `gsi_of` en vez de usar el 4 directo.
//!
//! # El que atiende tiene que vaciar la cola
//!
//! El UART mantiene el timbre sonando mientras haya un byte sin leer. Si el que
//! atiende solo dijera "ya te oi", el timbre volveria a sonar de inmediato y la
//! maquina giraria sin avanzar.

use kernel_core::acpi::Hardware;

/// El numero de timbre que le asignamos al cable serie.
///
/// Del 0 al 31 son las excepciones del CPU; del 32 para arriba quedan libres
/// para los aparatos. El 0x40 esta bien arriba de todo lo legado.
const SERIAL_VECTOR: u8 = 0x40;

/// El cable del puerto serie en la PC original.
const SERIAL_CABLE: u8 = 4;

/// El numero de timbre del buzon del agente.
///
/// **Mas bajo que el del cable a proposito.** En x86 la prioridad sale del
/// numero: el CPU atiende primero los de numero mas alto, en grupos de 16. Con
/// el cable en 0x40 (grupo 4) y el buzon en 0x30 (grupo 3), por mas que el
/// agente inunde de llamadas el cordon pasa primero (D17, P6).
const MAILBOX_VECTOR: u8 = 0x30;

/// Y el que usa el nucleo del protocolo para despertar a un nucleo reclamado
/// que esta durmiendo esperando trabajo.
///
/// Lejos del 0x31 en adelante, que se reparten los handlers del agente, y del
/// 0x40 del cable serie.
const WAKE_VECTOR: u8 = 0x41;

/// Registro de control de interrupciones del APIC local: la parte de abajo
/// dispara el envio, la de arriba dice a quien. Son los mismos que usa `smp`
/// para arrancar los otros nucleos — un IPI es un IPI.
const ICR_LOW: u64 = 0x300;
const ICR_HIGH: u64 = 0x310;

/// Donde el APIC local dice "ya atendi".
const EOI: u64 = 0xB0;

/// El registro que configura el reloj local de este nucleo.
const LVT_TIMER: u64 = 0x320;
/// Bit 16 del LVT: enmascarada. Se usa para calibrar sin que avise.
const LVT_MASKED: u32 = 1 << 16;
/// El vector con el que avisa el plazo.
const DEADLINE_VECTOR: u8 = 0x42;
/// Cuenta inicial: el reloj baja desde aca hasta cero y avisa una vez.
const TMICT: u64 = 0x380;
/// La cuenta actual, para poder medir cuanto bajo.
const TMCCT: u64 = 0x390;
/// El divisor. `0b1011` es dividir por uno: la maxima resolucion que da.
const TDCR: u64 = 0x3E0;
const TDCR_BY_ONE: u32 = 0b1011;

/// A que ritmo baja el reloj del APIC, medido contra el TSC.
///
/// **Hay que medirlo y no hay a quien preguntarle.** La arquitectura tiene un
/// modo donde el plazo se compara directo contra el TSC y este numero no hace
/// falta, pero es **opcional** y este CPU no lo tiene: el bit 24 de
/// `CPUID.01H:ECX` viene en cero. Se comprobo leyendolo, despues de que un
/// `asm!` mal declarado hiciera creer lo contrario.
///
/// Asi que se usa el modo de cuenta atras, que necesita saber a que ritmo
/// cuenta. Se mide contra el TSC porque el TSC ya esta calibrado (deuda 17): es
/// un reloj del que se conoce el ritmo, que es justo lo que hace falta para
/// averiguar el de otro.
static mut APIC_HZ: u64 = 0;

/// Si el aviso de plazo quedo instalado. Lo consulta `describe`: un plazo que no
/// va a llegar es peor que no ofrecerlo (P4).
static mut DEADLINE_READY: bool = false;

pub fn deadline_ready() -> bool {
    unsafe { DEADLINE_READY }
}

/// Instala el aviso de plazo, midiendo el reloj del APIC contra el TSC.
///
/// # Safety
///
/// Despues de que la IDT y el APIC local esten puestos.
pub unsafe fn install_deadline(tsc_hz: u64) -> Result<(), &'static str> {
    if APIC == 0 {
        return Err("no hay APIC local");
    }
    if tsc_hz == 0 {
        // Sin un reloj del que se sepa el ritmo no hay contra que medir, y sin
        // medir no se puede traducir "cien milisegundos" a nada.
        return Err("sin reloj no hay con que traducir un plazo");
    }
    crate::idt::set_gate(DEADLINE_VECTOR as usize, irq_deadline_stub as *const () as u64)?;

    // Calibrar con la interrupcion enmascarada: mientras se mide no tiene que
    // avisar nada.
    core::ptr::write_volatile((APIC + TDCR) as *mut u32, TDCR_BY_ONE);
    core::ptr::write_volatile(
        (APIC + LVT_TIMER) as *mut u32,
        DEADLINE_VECTOR as u32 | LVT_MASKED,
    );
    core::ptr::write_volatile((APIC + TMICT) as *mut u32, u32::MAX);

    // Un milisegundo de TSC alcanza: con el divisor en uno, el reloj del APIC
    // baja millones de pasos en ese rato.
    let span = tsc_hz / 1000;
    let start_tsc = crate::clock::ticks();
    let start_apic = core::ptr::read_volatile((APIC + TMCCT) as *const u32);
    while crate::clock::ticks().wrapping_sub(start_tsc) < span {
        core::hint::spin_loop();
    }
    let end_apic = core::ptr::read_volatile((APIC + TMCCT) as *const u32);
    let elapsed_tsc = crate::clock::ticks().wrapping_sub(start_tsc);

    // Baja, asi que la resta va al reves.
    let ticked = start_apic.wrapping_sub(end_apic) as u64;
    if ticked == 0 || elapsed_tsc == 0 {
        return Err("el reloj del APIC no avanza");
    }
    APIC_HZ = ticked.saturating_mul(tsc_hz) / elapsed_tsc;
    if APIC_HZ == 0 {
        return Err("no se pudo medir el reloj del APIC");
    }

    // Y desarmarlo hasta que alguien declare un plazo.
    core::ptr::write_volatile((APIC + TMICT) as *mut u32, 0);
    core::ptr::write_volatile((APIC + LVT_TIMER) as *mut u32, DEADLINE_VECTOR as u32);
    DEADLINE_READY = true;
    Ok(())
}

/// Programa el aviso para ese valor del TSC, o lo desarma con `None`.
///
/// # Safety
///
/// `install_deadline` tiene que haber andado.
pub unsafe fn set_deadline(at: Option<u64>) {
    if APIC == 0 || APIC_HZ == 0 {
        return;
    }
    let Some(when) = at else {
        // Cuenta cero: desarmado.
        core::ptr::write_volatile((APIC + TMICT) as *mut u32, 0);
        return;
    };
    // El plazo viene en pasos del TSC —el contador que usa el resto del
    // kernel— y este reloj cuenta a otro ritmo, asi que hay que pasarlo de uno
    // al otro. Es lo que la medicion del arranque hace posible.
    let now = crate::clock::ticks();
    let left = when.saturating_sub(now);
    let tsc_hz = crate::clock::hz();
    if left == 0 || tsc_hz == 0 {
        // Ya vencio: la cuenta mas corta que existe, para que el aviso llegue
        // igual en vez de no armar nada.
        core::ptr::write_volatile((APIC + TMICT) as *mut u32, 1);
        return;
    }
    let count = left.saturating_mul(APIC_HZ) / tsc_hz;
    core::ptr::write_volatile((APIC + TMICT) as *mut u32, count.clamp(1, u32::MAX as u64) as u32);
}

/// Vencio el plazo que el agente declaro para su codigo.
///
/// Corta lo que este corriendo, con el mismo desvio que un fault y que el corte
/// pedido de afuera. Es la unica forma de recuperar el nucleo que atiende el
/// protocolo: ahi el codigo del agente corre sin privilegio (D29) asi que las
/// interrupciones entran, pero el que tendria que mirar el reloj es justamente
/// el que se colgo.
#[no_mangle]
extern "sysv64" fn irq_deadline_rust(frame: *mut Frame) {
    // SAFETY: el APIC de este nucleo esta donde el identity map lo cubre.
    unsafe {
        if APIC != 0 {
            core::ptr::write_volatile((APIC + EOI) as *mut u32, 0);
        }
        // Y desarmarlo, o vuelve a avisar en cuanto se habiliten los timbres.
        set_deadline(None);
    }

    if crate::percpu::armed() == 0 {
        return;
    }
    kernel_core::work::mark_cancelled(crate::percpu::slot());
    // SAFETY: el stub paso el marco que el procesador apilo.
    unsafe { divert(&mut *frame) };
}
/// Registro de interrupcion espuria: su bit 8 prende el APIC local.
const SVR: u64 = 0xF0;

/// La direccion del APIC local, para poder decir "ya atendi" desde el handler.
static mut APIC: u64 = 0;

core::arch::global_asm!(
    r#"
.section .text
.globl irq_serial_stub

// Lo que corre cuando suena el timbre del cable serie.
//
// Salva los registros que la convencion de llamadas permite pisar: esto
// interrumpe codigo cualquiera, que no tiene idea de que va a pasar.
irq_serial_stub:
    push rax
    push rcx
    push rdx
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push rbx

    // La ABI pide la pila alineada a 16 antes de un `call`. rbx ya quedo
    // salvado arriba, asi que sirve de andamio.
    mov rbx, rsp
    and rsp, -16
    call irq_serial_rust
    mov rsp, rbx

    pop rbx
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rax
    iretq

.globl irq_deadline_stub

// Lo que corre cuando vence el plazo que el agente declaro. Igual que el del
// despertador: guarda lo que va a tocar y le pasa al handler el marco, que es lo
// unico con lo que se puede cambiar a donde vuelve.
irq_deadline_stub:
    push rax
    push rcx
    push rdx
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push rbx

    lea rdi, [rsp + 80]
    mov rbx, rsp
    and rsp, -16
    call irq_deadline_rust
    mov rsp, rbx

    pop rbx
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rax
    iretq

.globl irq_wake_stub

// Lo que corre cuando a un nucleo reclamado lo despiertan. No tiene nada que
// hacer: alcanza con que el `hlt` haya vuelto, porque el trabajo ya estaba en
// el buzon antes de que sonara. Solo avisa que atendio.
irq_wake_stub:
    push rax
    push rcx
    push rdx
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push rbx

    // El marco que armo el procesador esta **arriba** de los diez registros que
    // acabamos de guardar: son 80 bytes. Se le pasa el puntero al handler para
    // que pueda cambiar a donde vuelve el `iretq` — es la unica forma de sacar a
    // este nucleo de un bucle del que no sale solo.
    lea rdi, [rsp + 80]
    mov rbx, rsp
    and rsp, -16
    call irq_wake_rust
    mov rsp, rbx

    pop rbx
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rax
    iretq

.globl irq_mailbox_stub

// Lo que corre cuando el agente toca el timbre del buzon. Solo despierta: el
// trabajo lo hace el bucle cuando termina lo que estaba haciendo.
irq_mailbox_stub:
    push rax
    push rcx
    push rdx
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push rbx

    mov rbx, rsp
    and rsp, -16
    call irq_mailbox_rust
    mov rsp, rbx

    pop rbx
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rax
    iretq
"#
);

extern "sysv64" {
    fn irq_serial_stub();
    fn irq_mailbox_stub();
    fn irq_wake_stub();
    fn irq_deadline_stub();
}

/// El marco que el procesador apila al entrar a una interrupcion.
///
/// Es el mismo que mira el handler de faults. Cambiarlo cambia a donde vuelve
/// el `iretq`, y eso es lo que permite desviar la ejecucion.
#[repr(C)]
struct Frame {
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

/// Atiende el despertador, y **corta el trabajo si se pidio**.
///
/// Despertar no trae informacion —la trae el buzon— asi que normalmente esto
/// solo avisa que atendio. Pero el mismo timbre sirve para lo contrario: si
/// alguien pidio cortar lo que corre aca, este es el unico momento en que se
/// puede hacer, porque **desde otro nucleo no se puede desviar la ejecucion de
/// este**: solo pedirle que se desvie solo.
///
/// El desvio es el mismo que usa un fault: se le cambia el destino al `iretq`
/// para que aterrice en el punto de recuperacion de `exec` en vez de volver al
/// codigo. Con eso, un bucle del que el codigo del agente no sale se convierte
/// en una respuesta (P5).
#[no_mangle]
extern "sysv64" fn irq_wake_rust(frame: *mut Frame) {
    // SAFETY: cada nucleo tiene su APIC local en la misma direccion, que el
    // identity map cubre.
    unsafe {
        let apic = APIC;
        if apic != 0 {
            core::ptr::write_volatile((apic + EOI) as *mut u32, 0);
        }
    }

    let slot = crate::percpu::slot();
    // Solo se desvia si hay un `exec` en curso: sin punto de recuperacion no
    // hay a donde volver, y desviar seria saltar a basura.
    if crate::percpu::armed() == 0 || !kernel_core::work::take_cancel(slot) {
        return;
    }

    // SAFETY: el stub paso el marco que el procesador apilo, que esta en la
    // pila de este nucleo.
    unsafe { divert(&mut *frame) };
}

/// Le cambia el destino al `iretq`: en vez de volver al codigo del agente,
/// vuelve al punto de recuperacion de `exec`.
///
/// Lo usan los dos handlers que sacan a un nucleo de donde estaba —el corte
/// pedido de afuera y el plazo vencido— y es el mismo desvio que hace el de
/// faults. Va sobre el marco corto que apila una interrupcion: el de las
/// excepciones lleva ademas todos los registros que guarda su stub, asi que los
/// tipos no son el mismo.
fn divert(m: &mut Frame) {
    m.rip = crate::percpu::return_point();
    // Si el codigo venia de anillo 3, volver ahi con una direccion del kernel
    // fallaria: hay que volver tambien de anillo. Es lo mismo que hace el
    // handler de faults, y por el mismo motivo.
    if m.cs & 3 != 0 {
        m.cs = crate::gdt::CODE as u64;
        m.ss = crate::gdt::DATA as u64;
        m.rsp = crate::percpu::kernel_stack();
    }
}

/// Vacia la cola del UART y avisa que ya atendio.
#[no_mangle]
extern "sysv64" fn irq_serial_rust() {
    // Hasta que no quede nada: si quedara un byte, el timbre volveria a sonar.
    while let Some(b) = crate::uart::read_byte() {
        // SAFETY: corre en el nucleo del cable, y el bucle solo saca bytes con
        // el timbre apagado, asi que no hay dos manos en el anillo a la vez.
        unsafe { kernel_core::serial::push(b) };
    }

    // SAFETY: `install` dejo la direccion del APIC, que el identity map cubre.
    unsafe {
        let apic = APIC;
        if apic != 0 {
            core::ptr::write_volatile((apic + EOI) as *mut u32, 0);
        }
    }
}

/// Anota que sono y avisa que ya atendio. Nada mas.
#[no_mangle]
extern "sysv64" fn irq_mailbox_rust() {
    kernel_core::channel::rang();

    // SAFETY: `install` dejo la direccion del APIC, que el identity map cubre.
    unsafe {
        let apic = APIC;
        if apic != 0 {
            core::ptr::write_volatile((apic + EOI) as *mut u32, 0);
        }
    }
}

// --- El IO-APIC -------------------------------------------------------------
//
// No se lee ni se escribe directo: tiene dos ventanillas. En la primera se pone
// que registro se quiere y en la segunda se lee o escribe su valor.

unsafe fn ioapic_write(base: u64, reg: u32, value: u32) {
    core::ptr::write_volatile(base as *mut u32, reg);
    core::ptr::write_volatile((base + 0x10) as *mut u32, value);
}

/// Programa el timbre del cable serie y lo enciende.
///
/// Devuelve el numero de timbre que quedo asignado.
///
/// # Safety
///
/// Las tablas de paginas y la IDT tienen que estar puestas.
pub unsafe fn install(hw: &Hardware) -> Result<u8, &'static str> {
    let Some(apic) = hw.interrupts.filter(|i| i.kind == "apic") else {
        return Err("la maquina no informa un APIC");
    };
    let Some(io) = hw.ioapic else {
        return Err("la maquina no informa un IO-APIC");
    };
    APIC = apic.address;

    // Prender el APIC local, por si el firmware lo dejo apagado.
    let svr = core::ptr::read_volatile((apic.address + SVR) as *const u32);
    core::ptr::write_volatile((apic.address + SVR) as *mut u32, svr | (1 << 8));

    // El numero de timbre en la tabla de excepciones.
    crate::idt::set_gate(SERIAL_VECTOR as usize, irq_serial_stub as *const () as u64)?;

    // A que numero global corresponde el cable 4 en ESTA maquina.
    let gsi = hw.gsi_of(SERIAL_CABLE);
    if gsi < io.gsi_base {
        return Err("el cable del serie no lo atiende este IO-APIC");
    }
    let entry = gsi - io.gsi_base;

    // Cada entrada de ruteo son dos registros, a partir del 0x10.
    let reg = 0x10 + entry * 2;

    // A quien: este nucleo.
    ioapic_write(io.address, reg + 1, (crate::smp::this_core() as u32) << 24);
    // Que timbre, y desenmascarado. Los ceros de arriba son los valores por
    // defecto que corresponden a un cable de PC: entrega fija, destino fisico,
    // activo en alto, por flanco.
    ioapic_write(io.address, reg, SERIAL_VECTOR as u32);

    // Y por ultimo decirle al UART que levante la mano cuando llegue un byte.
    crate::uart::enable_rx_interrupt();

    Ok(SERIAL_VECTOR)
}

/// Duerme hasta que suene algun timbre.
///
/// `sti` habilita las interrupciones y `hlt` duerme, **en ese orden y pegados**.
/// Importa: el bucle corre con el timbre apagado, asi que si un byte llego justo
/// antes, su interrupcion quedo pendiente y el `hlt` vuelve enseguida. Al revés
/// —dormir y despues habilitar— se perderia ese despertador.
pub fn sleep() {
    unsafe { core::arch::asm!("sti; hlt; cli", options(nomem, nostack)) }
}

/// Prepara a un nucleo reclamado para que lo puedan despertar.
///
/// El APIC local es **por nucleo**: que el de arranque este encendido no dice
/// nada del de este. La entrada de la tabla, en cambio, es compartida, asi que
/// ponerla de nuevo no molesta.
///
/// # Safety
///
/// Corre en el nucleo reclamado, con su IDT ya puesta.
pub unsafe fn prepare_worker() -> Result<(), &'static str> {
    // La direccion la dejo `install` corriendo en el nucleo de arranque: el
    // APIC local de cada nucleo vive en la misma direccion, y cada uno ve el
    // suyo. Lo que NO se hereda es que este encendido.
    if APIC == 0 {
        return Err("el APIC todavia no esta encendido");
    }
    let svr = core::ptr::read_volatile((APIC + SVR) as *const u32);
    core::ptr::write_volatile((APIC + SVR) as *mut u32, svr | (1 << 8));

    crate::idt::set_gate(WAKE_VECTOR as usize, irq_wake_stub as *const () as u64)
}

/// Despierta al nucleo que la maquina nombra con ese identificador.
///
/// Las dos escrituras van en este orden: la primera dice a quien, la segunda
/// dispara. Al reves se le mandaria a quien hubiera quedado de antes.
/// Le manda un NMI a otro nucleo: la interrupcion que `cli` no puede tapar.
///
/// En el registro de comando del APIC, el modo de entrega vive en los bits 8 a
/// 10, y `100` es NMI. El vector se ignora en ese modo — la entrada de la tabla
/// que se usa es siempre la 2, que la fija la arquitectura.
pub fn stop(id: u64) -> bool {
    // SAFETY: el APIC de este nucleo esta en la direccion que el identity map
    // cubre, y escribir el comando es lo unico que hace falta.
    unsafe {
        if APIC == 0 {
            return false;
        }
        core::ptr::write_volatile((APIC + ICR_HIGH) as *mut u32, (id as u32) << 24);
        core::ptr::write_volatile((APIC + ICR_LOW) as *mut u32, 4 << 8);
    }
    true
}

pub fn wake(id: u64) {
    // SAFETY: el APIC lo dejo `install`, y el identity map lo cubre.
    unsafe {
        let apic = APIC;
        if apic == 0 {
            return;
        }
        core::ptr::write_volatile((apic + ICR_HIGH) as *mut u32, (id as u32) << 24);
        core::ptr::write_volatile((apic + ICR_LOW) as *mut u32, WAKE_VECTOR as u32);
    }
}

/// Programa el timbre del buzon: un IPI que el agente se manda a este nucleo.
///
/// # Safety
///
/// `install` tiene que haber corrido antes: comparten el APIC.
pub unsafe fn install_doorbell() -> Result<kernel_core::channel::Doorbell, &'static str> {
    if APIC == 0 {
        return Err("el APIC todavia no esta encendido");
    }

    crate::idt::set_gate(MAILBOX_VECTOR as usize, irq_mailbox_stub as *const () as u64)?;

    // Dos escrituras, y en este orden: la primera dice a quien, la segunda
    // dispara la llamada. Al reves se mandaria a quien hubiera quedado antes.
    let target = (crate::smp::this_core() as u64) << 24;
    Ok(kernel_core::channel::Doorbell {
        writes: [
            (APIC + ICR_HIGH, target, 4),
            (APIC + ICR_LOW, MAILBOX_VECTOR as u64, 4),
        ],
        count: 2,
        id: MAILBOX_VECTOR as u32,
    })
}

// --- Los handlers del agente (D9) -------------------------------------------

/// Los numeros de timbre que se reservan para el agente.
///
/// **Abajo del cable a proposito.** El cable esta en 0x40 (grupo 4) y estos en
/// el grupo 3, asi que un aparato del agente que se vuelva loco no puede tapar
/// el cordon (D17, P6).
const AGENT_VECTOR: u8 = 0x31;

core::arch::global_asm!(
    r#"
.section .text
.globl AGENT_STUBS

// Un stub por ranura. Cada uno apila su numero de ranura y salta al comun: es
// la unica forma de que el codigo comun sepa a que handler llamar, porque el
// CPU no dice por que vector entro.
.macro AGENT_STUB n
agent_\n:
    push \n
    jmp agent_comun
.endm

AGENT_STUB 0
AGENT_STUB 1
AGENT_STUB 2
AGENT_STUB 3
AGENT_STUB 4
AGENT_STUB 5
AGENT_STUB 6
AGENT_STUB 7

// El prologo y el epilogo que D9 dice que pone el kernel. Salva lo que la
// convencion de llamadas permite pisar, llama, restaura, y avisa que atendio.
agent_comun:
    push rax
    push rcx
    push rdx
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push rbx

    // La ranura quedo apilada antes de todo esto.
    mov rdi, [rsp + 80]

    mov rbx, rsp
    and rsp, -16
    call irq_agent_rust
    mov rsp, rbx

    pop rbx
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rax

    // Sacar la ranura que apilo el stub.
    add rsp, 8
    iretq

.section .rodata
.balign 8
AGENT_STUBS:
    .quad agent_0, agent_1, agent_2, agent_3
    .quad agent_4, agent_5, agent_6, agent_7
"#
);

extern "C" {
    static AGENT_STUBS: [u64; kernel_core::handlers::MAX];
}

/// Llama al codigo del agente y avisa que se atendio.
#[no_mangle]
extern "sysv64" fn irq_agent_rust(slot: u64) {
    let slot = slot as usize;

    if let Some(h) = kernel_core::handlers::at(slot) {
        kernel_core::handlers::served(slot);
        // SAFETY: la direccion salio de un reclamo vigente del agente, y el
        // identity map cubre toda la memoria. Lo que haya ahi puede ser
        // cualquier cosa — de eso se trata (P2).
        unsafe {
            let f: extern "sysv64" fn() = core::mem::transmute(h.entry);
            f();
        }
    }

    // SAFETY: `install` dejo la direccion del APIC.
    unsafe {
        let apic = APIC;
        if apic != 0 {
            core::ptr::write_volatile((apic + EOI) as *mut u32, 0);
        }
    }
}

/// Rutea una interrupcion de aparato al codigo del agente.
///
/// # Safety
///
/// `install` tiene que haber corrido antes.
/// La direccion donde escriben los aparatos para disparar una interrupcion.
///
/// En x86_64 no hay un aparato en el medio: se le escribe al APIC local, y cual
/// es lo dicen los doce bits de arriba mas el numero del nucleo. Por eso aca la
/// MADT no hace falta para esto — la direccion se arma, no se lee de una tabla.
const MSI_BASE: u64 = 0xFEE0_0000;

/// Instala un handler para una interrupcion disparada por escritura (MSI).
///
/// # Safety
///
/// La entrada del handler tiene que estar en un reclamo vigente.
pub unsafe fn install_msi(
    slot: usize,
    raw: bool,
) -> Result<(u32, kernel_core::channel::Doorbell), kernel_core::handlers::Error> {
    use kernel_core::handlers::Error;

    if APIC == 0 {
        return Err(Error::NoSuchInterrupt);
    }
    // El mismo vector que usaria por cable: lo que cambia es como llega, no
    // quien lo atiende.
    let vector = AGENT_VECTOR + slot as u8;
    let target = if raw {
        kernel_core::handlers::at(slot).map(|h| h.entry).ok_or(Error::NoSuchInterrupt)?
    } else {
        AGENT_STUBS[slot]
    };
    crate::idt::set_gate(vector as usize, target).map_err(|_| Error::NoSuchInterrupt)?;

    // Y **no se toca el IOAPIC**: no hay cable que rutear. El aparato va a
    // escribir directo, que es todo el punto de MSI.
    let address = MSI_BASE | ((crate::smp::this_core() as u64) << 12);
    Ok((
        vector as u32,
        kernel_core::channel::Doorbell {
            writes: [(address, vector as u64, 4), (0, 0, 0)],
            count: 1,
            id: vector as u32,
        },
    ))
}

pub unsafe fn install_agent(
    hw: &Hardware,
    interrupt: u32,
    slot: usize,
    raw: bool,
) -> Result<kernel_core::channel::Doorbell, kernel_core::handlers::Error> {
    use kernel_core::handlers::Error;

    if APIC == 0 {
        return Err(Error::NoSuchInterrupt);
    }
    let Some(io) = hw.ioapic else {
        return Err(Error::NoSuchInterrupt);
    };
    if interrupt < io.gsi_base {
        return Err(Error::NoSuchInterrupt);
    }
    // El cable del serie no se entrega: seria quedarse sin cordon.
    if interrupt == hw.gsi_of(SERIAL_CABLE) {
        return Err(Error::IsKernels);
    }

    let vector = AGENT_VECTOR + slot as u8;

    // Con `raw` la tabla apunta directo al codigo del agente: ni prologo, ni
    // epilogo, ni EOI. Tiene que terminar en `iretq` y avisarle al APIC el
    // mismo.
    let target = if raw {
        kernel_core::handlers::at(slot).map(|h| h.entry).ok_or(Error::NoSuchInterrupt)?
    } else {
        AGENT_STUBS[slot]
    };

    crate::idt::set_gate(vector as usize, target).map_err(|_| Error::NoSuchInterrupt)?;

    let entry = interrupt - io.gsi_base;
    let reg = 0x10 + entry * 2;
    ioapic_write(io.address, reg + 1, (crate::smp::this_core() as u32) << 24);
    ioapic_write(io.address, reg, vector as u32);

    // Como hacerla sonar a proposito: un IPI a este mismo nucleo con ese
    // vector. Entra por la misma puerta que si hubiera hablado el aparato.
    let target = (crate::smp::this_core() as u64) << 24;
    Ok(kernel_core::channel::Doorbell {
        writes: [(APIC + ICR_HIGH, target, 4), (APIC + ICR_LOW, vector as u64, 4)],
        count: 2,
        id: vector as u32,
    })
}
