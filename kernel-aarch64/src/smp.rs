//! Arrancar los otros nucleos en aarch64 (D13).
//!
//! # PSCI: se le pide al firmware
//!
//! ARM estandarizo una interfaz para esto, PSCI, y el firmware la implementa.
//! Se le dice "prende el nucleo tal y arrancalo en esta direccion" y el se
//! encarga. Comparado con lo que hay que hacer en x86_64, es un lujo.
//!
//! La llamada se hace con una instruccion que salta al nivel de abajo, y **cual
//! de las dos es depende de la maquina**: `hvc` si hay un hipervisor, `smc` si
//! el firmware esta en el mundo seguro. No se adivina: lo dice la FADT (P4).
//!
//! # El nucleo arranca crudo
//!
//! PSCI lo entrega con la MMU apagada, las caches apagadas y nada configurado.
//! Antes de tocar cualquier cosa del kernel tiene que ponerse las mismas tablas
//! de paginas que el resto, prender la MMU y cargar la tabla de excepciones.
//!
//! El orden importa: **los atomicos no funcionan con la MMU apagada**. Con la
//! MMU apagada toda la memoria se comporta como si fuera un dispositivo, y ahi
//! las instrucciones de acceso exclusivo —que es como esta hecho un atomico— no
//! estan garantizadas. Avisar que se llego antes de prender la MMU seria
//! escribir en el aire.

use kernel_core::acpi::Psci;
use kernel_core::cores;

/// 16 KiB de pila para cada nucleo. Alcanza de sobra: van a correr codigo del
/// agente, que trae la suya.
const TAM_PILA: usize = 16 * 1024;

#[repr(C, align(16))]
struct Pilas([[u8; TAM_PILA]; cores::MAX]);

static mut PILAS: Pilas = Pilas([[0; TAM_PILA]; cores::MAX]);

// Lo que el nucleo de arranque deja anotado para que los demas se configuren
// igual que el. Se leen con la MMU apagada, asi que tienen que estar alineados.
#[no_mangle]
static mut AP_MAIR: u64 = 0;
#[no_mangle]
static mut AP_TCR: u64 = 0;
#[no_mangle]
static mut AP_TTBR0: u64 = 0;
#[no_mangle]
static mut AP_VBAR: u64 = 0;
#[no_mangle]
static mut AP_PILAS: u64 = 0;

/// El identificador de PSCI para "prender un nucleo", en su version de 64 bits.
const CPU_ON: u64 = 0xC400_0003;

core::arch::global_asm!(
    r#"
.section .text
.globl ap_entry

// Aca aterriza un nucleo recien prendido. Llega con la MMU apagada y x0 con el
// numero de ranura, que es el `context id` que le pasamos a PSCI.
ap_entry:
    mov  x19, x0                      // la ranura, a un registro que sobreviva

    // Las mismas tablas que usa el nucleo de arranque.
    adrp x1, AP_MAIR
    ldr  x1, [x1, :lo12:AP_MAIR]
    msr  mair_el1, x1

    adrp x1, AP_TCR
    ldr  x1, [x1, :lo12:AP_TCR]
    msr  tcr_el1, x1

    adrp x1, AP_TTBR0
    ldr  x1, [x1, :lo12:AP_TTBR0]
    msr  ttbr0_el1, x1

    dsb  sy
    isb
    tlbi vmalle1
    dsb  sy
    isb

    // MMU, cache de datos y cache de instrucciones.
    mrs  x1, sctlr_el1
    orr  x1, x1, #1
    orr  x1, x1, #(1 << 2)
    orr  x1, x1, #(1 << 12)
    msr  sctlr_el1, x1
    isb

    // La tabla de excepciones. Desde aca un fault se captura en vez de matar.
    adrp x1, AP_VBAR
    ldr  x1, [x1, :lo12:AP_VBAR]
    msr  vbar_el1, x1
    isb

    // Su pila: la base del arreglo mas su ranura por el tamano de cada una.
    adrp x1, AP_PILAS
    ldr  x1, [x1, :lo12:AP_PILAS]
    mov  x2, #(16 * 1024)
    madd x1, x19, x2, x1
    add  x1, x1, x2                   // la cima: crece hacia abajo
    mov  sp, x1

    mov  x0, x19
    bl   ap_main

    // `ap_main` no vuelve. Si volviera, quedarse quieto es mejor que seguir.
1:  wfe
    b    1b
"#
);

extern "C" {
    fn ap_entry();
}

/// Lo primero que corre un nucleo nuevo con todo ya configurado.
#[no_mangle]
extern "C" fn ap_main(slot: u64) -> ! {
    let mpidr: u64;
    // El identificador de este nucleo, tal como lo nombra la maquina. Los bits
    // de arriba son banderas, no parte del numero.
    unsafe { core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack)) };

    cores::arrived(slot as usize, mpidr & 0x00FF_FFFF);

    // Y a esperar trabajo. Todavia no hay forma de darselo: `exec` corre en el
    // nucleo que atiende el protocolo. Esa es la parte que sigue.
    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) }
    }
}

/// Anota la configuracion que van a copiar los nucleos nuevos.
///
/// # Safety
///
/// Solo desde el nucleo de arranque, y con la MMU ya configurada.
pub unsafe fn prepare() {
    core::arch::asm!("mrs {}, mair_el1", out(reg) AP_MAIR, options(nomem, nostack));
    core::arch::asm!("mrs {}, tcr_el1", out(reg) AP_TCR, options(nomem, nostack));
    core::arch::asm!("mrs {}, ttbr0_el1", out(reg) AP_TTBR0, options(nomem, nostack));
    core::arch::asm!("mrs {}, vbar_el1", out(reg) AP_VBAR, options(nomem, nostack));
    AP_PILAS = core::ptr::addr_of!(PILAS) as u64;
}

/// El identificador de este nucleo.
pub fn this_core() -> u64 {
    let mpidr: u64;
    unsafe { core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack)) };
    mpidr & 0x00FF_FFFF
}

/// Le pide al firmware que prenda un nucleo.
///
/// # Safety
///
/// `prepare` tiene que haberse llamado antes.
pub unsafe fn start(psci: Option<Psci>, id: u64, slot: usize) -> Result<(), cores::Error> {
    let Some(p) = psci else {
        return Err(cores::Error::NoMechanism);
    };

    let entrada = ap_entry as usize as u64;
    let estado = llamar(p.use_hvc, CPU_ON, id, entrada, slot as u64);

    // PSCI devuelve 0 en exito y un negativo en error.
    if estado != 0 {
        return Err(cores::Error::NeverArrived);
    }
    Ok(())
}

/// Una llamada PSCI.
///
/// Los registros del x4 al x17 se declaran pisados porque la convencion de
/// llamadas de ARM permite que el firmware los use: si no se dijera, el
/// compilador podria dejar algo vivo ahi y encontrarlo cambiado al volver.
unsafe fn llamar(use_hvc: bool, funcion: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let salida: i64;
    if use_hvc {
        core::arch::asm!(
            "hvc #0",
            inout("x0") funcion => salida,
            inout("x1") a1 => _, inout("x2") a2 => _, inout("x3") a3 => _,
            lateout("x4") _, lateout("x5") _, lateout("x6") _, lateout("x7") _,
            lateout("x8") _, lateout("x9") _, lateout("x10") _, lateout("x11") _,
            lateout("x12") _, lateout("x13") _, lateout("x14") _, lateout("x15") _,
            lateout("x16") _, lateout("x17") _,
        );
    } else {
        core::arch::asm!(
            "smc #0",
            inout("x0") funcion => salida,
            inout("x1") a1 => _, inout("x2") a2 => _, inout("x3") a3 => _,
            lateout("x4") _, lateout("x5") _, lateout("x6") _, lateout("x7") _,
            lateout("x8") _, lateout("x9") _, lateout("x10") _, lateout("x11") _,
            lateout("x12") _, lateout("x13") _, lateout("x14") _, lateout("x15") _,
            lateout("x16") _, lateout("x17") _,
        );
    }
    salida
}
