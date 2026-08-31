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
"#
);

extern "sysv64" {
    fn irq_serie_stub();
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
