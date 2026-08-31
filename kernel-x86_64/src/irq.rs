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
const VECTOR_SERIE: u8 = 0x40;

/// El cable del puerto serie en la PC original.
const CABLE_SERIE: u8 = 4;

/// El numero de timbre del buzon del agente.
///
/// **Mas bajo que el del cable a proposito.** En x86 la prioridad sale del
/// numero: el CPU atiende primero los de numero mas alto, en grupos de 16. Con
/// el cable en 0x40 (grupo 4) y el buzon en 0x30 (grupo 3), por mas que el
/// agente inunde de llamadas el cordon pasa primero (D17, P6).
const VECTOR_BUZON: u8 = 0x30;

/// Registro de control de interrupciones del APIC local: la parte de abajo
/// dispara el envio, la de arriba dice a quien. Son los mismos que usa `smp`
/// para arrancar los otros nucleos — un IPI es un IPI.
const ICR_BAJO: u64 = 0x300;
const ICR_ALTO: u64 = 0x310;

/// Donde el APIC local dice "ya atendi".
const EOI: u64 = 0xB0;
/// Registro de interrupcion espuria: su bit 8 prende el APIC local.
const SVR: u64 = 0xF0;

/// La direccion del APIC local, para poder decir "ya atendi" desde el handler.
static mut APIC: u64 = 0;

core::arch::global_asm!(
    r#"
.section .text
.globl irq_serie_stub

// Lo que corre cuando suena el timbre del cable serie.
//
// Salva los registros que la convencion de llamadas permite pisar: esto
// interrumpe codigo cualquiera, que no tiene idea de que va a pasar.
irq_serie_stub:
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
    call irq_serie_rust
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

.globl irq_buzon_stub

// Lo que corre cuando el agente toca el timbre del buzon. Solo despierta: el
// trabajo lo hace el bucle cuando termina lo que estaba haciendo.
irq_buzon_stub:
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
    call irq_buzon_rust
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
    fn irq_serie_stub();
    fn irq_buzon_stub();
}

/// Vacia la cola del UART y avisa que ya atendio.
#[no_mangle]
extern "sysv64" fn irq_serie_rust() {
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
extern "sysv64" fn irq_buzon_rust() {
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

unsafe fn ioapic_escribir(base: u64, reg: u32, val: u32) {
    core::ptr::write_volatile(base as *mut u32, reg);
    core::ptr::write_volatile((base + 0x10) as *mut u32, val);
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
    crate::idt::set_gate(VECTOR_SERIE as usize, irq_serie_stub as *const () as u64)?;

    // A que numero global corresponde el cable 4 en ESTA maquina.
    let gsi = hw.gsi_of(CABLE_SERIE);
    if gsi < io.gsi_base {
        return Err("el cable del serie no lo atiende este IO-APIC");
    }
    let entrada = gsi - io.gsi_base;

    // Cada entrada de ruteo son dos registros, a partir del 0x10.
    let reg = 0x10 + entrada * 2;

    // A quien: este nucleo.
    ioapic_escribir(io.address, reg + 1, (crate::smp::this_core() as u32) << 24);
    // Que timbre, y desenmascarado. Los ceros de arriba son los valores por
    // defecto que corresponden a un cable de PC: entrega fija, destino fisico,
    // activo en alto, por flanco.
    ioapic_escribir(io.address, reg, VECTOR_SERIE as u32);

    // Y por ultimo decirle al UART que levante la mano cuando llegue un byte.
    crate::uart::enable_rx_interrupt();

    Ok(VECTOR_SERIE)
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

/// Programa el timbre del buzon: un IPI que el agente se manda a este nucleo.
///
/// # Safety
///
/// `install` tiene que haber corrido antes: comparten el APIC.
pub unsafe fn install_doorbell() -> Result<kernel_core::channel::Doorbell, &'static str> {
    if APIC == 0 {
        return Err("el APIC todavia no esta encendido");
    }

    crate::idt::set_gate(VECTOR_BUZON as usize, irq_buzon_stub as *const () as u64)?;

    // Dos escrituras, y en este orden: la primera dice a quien, la segunda
    // dispara la llamada. Al reves se mandaria a quien hubiera quedado antes.
    let destino = (crate::smp::this_core() as u64) << 24;
    Ok(kernel_core::channel::Doorbell {
        writes: [
            (APIC + ICR_ALTO, destino, 4),
            (APIC + ICR_BAJO, VECTOR_BUZON as u64, 4),
        ],
        count: 2,
        id: VECTOR_BUZON as u32,
    })
}

// --- Los handlers del agente (D9) -------------------------------------------

/// Los numeros de timbre que se reservan para el agente.
///
/// **Abajo del cable a proposito.** El cable esta en 0x40 (grupo 4) y estos en
/// el grupo 3, asi que un aparato del agente que se vuelva loco no puede tapar
/// el cordon (D17, P6).
const VECTOR_AGENTE: u8 = 0x31;

core::arch::global_asm!(
    r#"
.section .text
.globl AGENTE_STUBS

// Un stub por ranura. Cada uno apila su numero de ranura y salta al comun: es
// la unica forma de que el codigo comun sepa a que handler llamar, porque el
// CPU no dice por que vector entro.
.macro STUB_AGENTE n
agente_\n:
    push \n
    jmp agente_comun
.endm

STUB_AGENTE 0
STUB_AGENTE 1
STUB_AGENTE 2
STUB_AGENTE 3
STUB_AGENTE 4
STUB_AGENTE 5
STUB_AGENTE 6
STUB_AGENTE 7

// El prologo y el epilogo que D9 dice que pone el kernel. Salva lo que la
// convencion de llamadas permite pisar, llama, restaura, y avisa que atendio.
agente_comun:
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
    call irq_agente_rust
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
AGENTE_STUBS:
    .quad agente_0, agente_1, agente_2, agente_3
    .quad agente_4, agente_5, agente_6, agente_7
"#
);

extern "C" {
    static AGENTE_STUBS: [u64; kernel_core::handlers::MAX];
}

/// Llama al codigo del agente y avisa que se atendio.
#[no_mangle]
extern "sysv64" fn irq_agente_rust(slot: u64) {
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
pub unsafe fn install_agente(
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
    if interrupt == hw.gsi_of(CABLE_SERIE) {
        return Err(Error::IsKernels);
    }

    let vector = VECTOR_AGENTE + slot as u8;

    // Con `raw` la tabla apunta directo al codigo del agente: ni prologo, ni
    // epilogo, ni EOI. Tiene que terminar en `iretq` y avisarle al APIC el
    // mismo.
    let destino = if raw {
        kernel_core::handlers::at(slot).map(|h| h.entry).ok_or(Error::NoSuchInterrupt)?
    } else {
        AGENTE_STUBS[slot]
    };

    crate::idt::set_gate(vector as usize, destino).map_err(|_| Error::NoSuchInterrupt)?;

    let entrada = interrupt - io.gsi_base;
    let reg = 0x10 + entrada * 2;
    ioapic_escribir(io.address, reg + 1, (crate::smp::this_core() as u32) << 24);
    ioapic_escribir(io.address, reg, vector as u32);

    // Como hacerla sonar a proposito: un IPI a este mismo nucleo con ese
    // vector. Entra por la misma puerta que si hubiera hablado el aparato.
    let destino = (crate::smp::this_core() as u64) << 24;
    Ok(kernel_core::channel::Doorbell {
        writes: [(APIC + ICR_ALTO, destino, 4), (APIC + ICR_BAJO, vector as u64, 4)],
        count: 2,
        id: vector as u32,
    })
}
