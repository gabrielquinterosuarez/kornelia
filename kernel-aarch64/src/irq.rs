//! El timbre del cable serie en aarch64.
//!
//! # Las dos mitades del GIC
//!
//! Igual que el APIC de x86, el controlador de ARM viene en dos partes:
//!
//! - El **distribuidor** (GICD), uno por maquina: recibe los cables de los
//!   aparatos, decide a que nucleo va cada uno y con que prioridad.
//! - La **interfaz de nucleo** (GICC), una por nucleo: es donde el nucleo
//!   pregunta "que timbre sono" y donde dice "ya atendi".
//!
//! Las dos direcciones salen de la MADT, que ya leemos.
//!
//! # Por que aca no hace falta un handler y en x86 si
//!
//! Esta es una diferencia de verdad entre las dos arquitecturas, y sale de como
//! duerme cada una.
//!
//! En x86, `hlt` **solo despierta si las interrupciones estan habilitadas**. Asi
//! que hay que habilitarlas, y entonces el timbre suena de verdad: hace falta
//! codigo que lo atienda.
//!
//! En ARM, `wfi` despierta con una interrupcion pendiente **aunque este
//! enmascarada**. Eso permite dejarla enmascarada siempre: el nucleo duerme, la
//! interrupcion queda pendiente y lo despierta, y el bucle mismo la reconoce y
//! vacia la cola. Nadie corre en medio de nadie.
//!
//! Y eso importa por una razon que no es estetica: si el que atiende el timbre
//! vaciara la cola justo despues de que el bucle miro y antes de que se duerma,
//! el bucle se dormiria con un byte ya guardado y sin nadie que lo despierte.
//! Enmascarando, ese hueco no existe.

use kernel_core::acpi::Hardware;

// --- Distribuidor -----------------------------------------------------------
/// Encender el distribuidor.
const GICD_CTLR: u64 = 0x000;
/// Habilitar una interrupcion: un bit por interrupcion.
const GICD_ISENABLER: u64 = 0x100;
/// Prioridad: un byte por interrupcion. **Mas chico es mas prioritario.**
const GICD_IPRIORITYR: u64 = 0x400;
/// A que nucleos va: un byte por interrupcion, un bit por nucleo.
const GICD_ITARGETSR: u64 = 0x800;

// --- Interfaz de nucleo -----------------------------------------------------
/// Encender la interfaz.
const GICC_CTLR: u64 = 0x000;
/// Umbral de prioridad: solo pasan las mas prioritarias que este numero.
const GICC_PMR: u64 = 0x004;
/// Leerlo es preguntar "que timbre sono", y a la vez reconocerlo.
const GICC_IAR: u64 = 0x00C;
/// Escribirlo es decir "ya atendi".
const GICC_EOIR: u64 = 0x010;

/// La prioridad del cable serie: la mas alta que hay.
///
/// Es a proposito y es la contracara del timbre que va a tener el agente. Por
/// mas que el agente inunde de llamadas, el cordon umbilical pasa primero — y
/// eso lo hace cumplir el silicio del GIC, no una decision del kernel (D17, P6).
const PRIORIDAD_SERIE: u8 = 0x00;

/// El numero de timbre del buzon del agente.
///
/// Del 0 al 15 son las que un nucleo se manda a otro. La 8 esta libre.
const SGI_BUZON: u32 = 8;

/// La prioridad del buzon: **mas baja que la del cable** (numero mas grande).
/// Por mas que el agente inunde de llamadas, el cordon pasa primero (D17, P6).
const PRIORIDAD_BUZON: u8 = 0x80;

/// Donde se dispara una interrupcion de nucleo a nucleo.
const GICD_SGIR: u64 = 0xF00;

/// El numero de interrupcion que no significa nada: el GIC lo devuelve cuando
/// no habia ninguna de verdad.
const ESPURIA: u32 = 1023;

static mut GICD: u64 = 0;
static mut GICC: u64 = 0;
static mut CABLE: u32 = 0;

unsafe fn leer(base: u64, reg: u64) -> u32 {
    core::ptr::read_volatile((base + reg) as *const u32)
}
unsafe fn escribir(base: u64, reg: u64, v: u32) {
    core::ptr::write_volatile((base + reg) as *mut u32, v);
}
unsafe fn escribir_byte(base: u64, reg: u64, v: u8) {
    core::ptr::write_volatile((base + reg) as *mut u8, v);
}

/// Programa el timbre del cable serie y lo enciende.
///
/// # Safety
///
/// Las tablas de paginas tienen que estar puestas.
pub unsafe fn install(hw: &Hardware) -> Result<u8, &'static str> {
    let Some(gic) = hw.interrupts.filter(|i| i.kind == "gic") else {
        return Err("la maquina no informa un GIC");
    };
    if gic.cpu_interface == 0 {
        return Err("la maquina no informa la interfaz de nucleo del GIC");
    }
    let Some(serie) = hw.serial else {
        return Err("la maquina no informa donde esta el puerto serie");
    };
    if serie.gsi == 0 {
        return Err("la maquina no informa por que interrupcion avisa el serie");
    }

    GICD = gic.address;
    GICC = gic.cpu_interface;
    CABLE = serie.gsi;

    // Encender el distribuidor y la interfaz de este nucleo.
    escribir(GICD, GICD_CTLR, 1);
    escribir(GICC, GICC_PMR, 0xF0); // que pase todo salvo lo menos prioritario
    escribir(GICC, GICC_CTLR, 1);

    let n = serie.gsi;

    // Prioridad, y a que nucleo va. El byte de destino es una mascara de
    // nucleos: el bit 0 es el primero.
    escribir_byte(GICD, GICD_IPRIORITYR + n as u64, PRIORIDAD_SERIE);
    escribir_byte(GICD, GICD_ITARGETSR + n as u64, 1);

    // Habilitarla: un bit por interrupcion, de a 32 por registro.
    escribir(GICD, GICD_ISENABLER + (n as u64 / 32) * 4, 1 << (n % 32));

    // Y decirle al UART que levante la mano cuando llegue un byte.
    crate::uart::enable_rx_interrupt();

    Ok(n as u8)
}

/// Duerme hasta que suene algun timbre, y despues vacia lo que llego.
///
/// Las interrupciones quedan enmascaradas todo el tiempo: `wfi` despierta con
/// una pendiente igual, y asi nadie corre en medio del bucle.
pub fn sleep() {
    unsafe {
        core::arch::asm!("wfi", options(nomem, nostack));

        if GICC == 0 {
            return;
        }

        // Preguntar que sono. Leer el registro ya es reconocer la interrupcion.
        loop {
            let cual = leer(GICC, GICC_IAR);
            let id = cual & 0x3FF;
            if id == ESPURIA {
                break;
            }
            if id == SGI_BUZON {
                // Solo despierta. El trabajo lo hace el bucle.
                kernel_core::channel::rang();
            } else if id == CABLE {
                // Vaciar la cola del UART: si quedara un byte, el timbre
                // volveria a sonar de inmediato.
                crate::uart::clear_rx_interrupt();
                while let Some(b) = crate::uart::read_byte() {
                    kernel_core::serial::push(b);
                }
            } else if let Some(slot) = kernel_core::handlers::slot_of(id) {
                // Un aparato del agente. El prologo y el epilogo que D9 dice
                // que pone el kernel son, aca, este mismo camino: los registros
                // ya estan a salvo porque esto es una llamada normal.
                if let Some(h) = kernel_core::handlers::at(slot) {
                    kernel_core::handlers::served(slot);
                    let f: extern "C" fn() = core::mem::transmute(h.entry);
                    f();
                }
            }
            // "Ya atendi", con el mismo numero que vino.
            escribir(GICC, GICC_EOIR, cual);
        }
    }
}

/// Programa el timbre del buzon: una interrupcion de nucleo a nucleo.
///
/// # Safety
///
/// `install` tiene que haber corrido antes: comparten el GIC.
pub unsafe fn install_doorbell() -> Result<kernel_core::channel::Doorbell, &'static str> {
    if GICD == 0 {
        return Err("el GIC todavia no esta encendido");
    }

    escribir_byte(GICD, GICD_IPRIORITYR + SGI_BUZON as u64, PRIORIDAD_BUZON);
    escribir(GICD, GICD_ISENABLER, 1 << SGI_BUZON);

    // Una sola escritura: los bits de arriba dicen a que nucleos, los de abajo
    // que numero de timbre. El bit 16 es el primer nucleo.
    let valor = (1u64 << 16) | SGI_BUZON as u64;
    Ok(kernel_core::channel::Doorbell {
        writes: [(GICD + GICD_SGIR, valor, 4), (0, 0, 0)],
        count: 1,
        id: SGI_BUZON,
    })
}

// --- Los handlers del agente (D9) -------------------------------------------

/// Marcar una interrupcion como pendiente sin que el aparato hable.
const GICD_ISPENDR: u64 = 0x200;

/// La prioridad de los aparatos del agente: **mas baja que el cable**. Un
/// aparato que se vuelva loco no puede tapar el cordon (D17, P6).
const PRIORIDAD_AGENTE: u8 = 0xA0;

/// Rutea una interrupcion de aparato al codigo del agente.
///
/// # Safety
///
/// `install` tiene que haber corrido antes.
pub unsafe fn install_agente(
    hw: &Hardware,
    interrupt: u32,
    _slot: usize,
    raw: bool,
) -> Result<kernel_core::channel::Doorbell, kernel_core::handlers::Error> {
    use kernel_core::handlers::Error;

    if GICD == 0 {
        return Err(Error::NoSuchInterrupt);
    }
    // En aarch64 el GIC entrega el numero y el reparto lo hace el kernel en
    // software: no hay un camino mas crudo que este. Decirlo es mejor que
    // aceptar el pedido y darle otra cosa (P4).
    if raw {
        return Err(Error::NoRawPath);
    }
    // Ni el cable ni el timbre del buzon se entregan.
    if hw.serial.map(|s| s.gsi) == Some(interrupt) || interrupt == SGI_BUZON {
        return Err(Error::IsKernels);
    }
    // Del 0 al 31 son las de nucleo a nucleo y las privadas de cada nucleo.
    if interrupt < 32 || interrupt >= 1020 {
        return Err(Error::NoSuchInterrupt);
    }

    escribir_byte(GICD, GICD_IPRIORITYR + interrupt as u64, PRIORIDAD_AGENTE);
    escribir_byte(GICD, GICD_ITARGETSR + interrupt as u64, 1);
    escribir(GICD, GICD_ISENABLER + (interrupt as u64 / 32) * 4, 1 << (interrupt % 32));

    // Como hacerla sonar a proposito: marcarla pendiente en el distribuidor.
    // El GIC no distingue eso de que el aparato haya hablado.
    let reg = GICD + GICD_ISPENDR + (interrupt as u64 / 32) * 4;
    Ok(kernel_core::channel::Doorbell {
        writes: [(reg, 1u64 << (interrupt % 32), 4), (0, 0, 0)],
        count: 1,
        id: interrupt,
    })
}
