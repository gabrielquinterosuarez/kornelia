//! El segundo canal: el buzon que arma el agente (D5, D17).
//!
//! # Por que existe
//!
//! El cable serie es el cordon umbilical, no el transporte: anda a 115200 bits
//! por segundo, que para subir codigo maquina es una eternidad. D5 dice que el
//! agente escribe el transporte rapido —el driver de red— y **se lo entrega** al
//! kernel. Este modulo es el punto de entrega.
//!
//! Y D17 dice que el cable **nunca se abandona**: el segundo canal se *agrega*.
//! El kernel escucha por los dos y contesta por donde le llegó el pedido. Un
//! cordon que se puede cortar no era un cordon.
//!
//! # Que lleva adentro
//!
//! Bytes, en las dos direcciones. Exactamente lo mismo que el cable: por arriba
//! va el mismo CBOR y el kernel no distingue de donde vino. Es a proposito —
//! hacer que el segundo canal tenga otra forma seria dos protocolos en vez de
//! uno.
//!
//! El acuerdo es este, en memoria que el agente reclamó con `mem.claim`:
//!
//! ```text
//!  offset  campo       quien escribe    para que
//!    0     magic       el agente        que el kernel pueda verificarlo
//!    4     version     el agente        para poder cambiarlo despues
//!    8     capacidad   el agente        cuantos bytes tiene cada anillo
//!   12     (reservado)
//!   16     ped_cabeza  el agente        hasta donde escribio pedidos
//!   20     ped_cola    el kernel        hasta donde los leyo
//!   24     res_cabeza  el kernel        hasta donde escribio respuestas
//!   28     res_cola    el agente        hasta donde las leyo
//!   32     pedidos     el agente        `capacidad` bytes
//!   32+c   respuestas  el kernel        `capacidad` bytes
//! ```
//!
//! # Por que los indices se leen con orden de memoria explicito
//!
//! El agente puede estar escribiendo desde otro nucleo. Cuando avanza
//! `ped_cabeza`, lo que importa es que **los bytes que escribio antes ya se
//! vean**: si el kernel viera el indice nuevo y los bytes viejos, leeria basura.
//! Esa garantia no es gratis — x86 la da casi siempre y ARM no — y se pide
//! explicitamente con `Acquire` y `Release`.

use core::sync::atomic::{AtomicU32, Ordering};

/// "KORN" en little-endian. Que el agente lo escriba es la senal de que el
/// buzon esta armado y no es memoria con basura.
const MAGIC: u32 = 0x4E52_4F4B;
/// La version del acuerdo. Si algun dia cambia la forma, esto lo distingue.
const VERSION: u32 = 1;

/// El encabezado, antes de los dos anillos.
const ENCABEZADO: u64 = 32;

// Offsets de cada campo.
const OFF_MAGIC: u64 = 0;
const OFF_VERSION: u64 = 4;
const OFF_CAPACIDAD: u64 = 8;
const OFF_PED_CABEZA: u64 = 16;
const OFF_PED_COLA: u64 = 20;
const OFF_RES_CABEZA: u64 = 24;
const OFF_RES_COLA: u64 = 28;

/// Un buzon ya verificado.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub struct Mailbox {
    /// Donde empieza, en fisicas.
    base: u64,
    /// Cuantos bytes tiene cada anillo.
    capacidad: u32,
    /// El handle del que salio, para poder informarlo.
    pub handle: u64,
}

/// Por que no se pudo adoptar.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub enum Error {
    /// El primer campo no dice "KORN": ahi no hay un buzon armado.
    NoMagic,
    /// El acuerdo es de otra version.
    BadVersion,
    /// La capacidad es cero, o no es potencia de dos.
    BadCapacity,
    /// El buzon no entra en la memoria que se reclamo.
    TooSmall,
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::NoMagic => "no-mailbox-magic",
            Error::BadVersion => "bad-mailbox-version",
            Error::BadCapacity => "bad-mailbox-capacity",
            Error::TooSmall => "region-too-small",
        }
    }
}

/// Un solo buzon: hay un solo agente (D13).
static mut ADOPTADO: Option<Mailbox> = None;

/// Un entero del encabezado, como atomico.
///
/// # Safety
///
/// `addr` tiene que estar mapeada y alineada a 4.
unsafe fn campo(addr: u64) -> &'static AtomicU32 {
    &*(addr as *const AtomicU32)
}

/// Mira si en `start` hay un buzon armado, y lo adopta.
///
/// # Safety
///
/// `start..start+bytes` tiene que estar mapeada.
pub unsafe fn adopt(handle: u64, start: u64, bytes: u64) -> Result<Mailbox, Error> {
    if bytes < ENCABEZADO {
        return Err(Error::TooSmall);
    }
    if campo(start + OFF_MAGIC).load(Ordering::Acquire) != MAGIC {
        return Err(Error::NoMagic);
    }
    if campo(start + OFF_VERSION).load(Ordering::Relaxed) != VERSION {
        return Err(Error::BadVersion);
    }

    let capacidad = campo(start + OFF_CAPACIDAD).load(Ordering::Relaxed);
    // Potencia de dos para que dar la vuelta sea una mascara y no una division.
    if capacidad == 0 || !capacidad.is_power_of_two() {
        return Err(Error::BadCapacity);
    }
    // Los dos anillos tienen que entrar en lo que el agente reclamo. Si no
    // entraran, el kernel escribiria fuera de lo que le entregaron.
    let necesario = ENCABEZADO + 2 * capacidad as u64;
    if necesario > bytes {
        return Err(Error::TooSmall);
    }

    let m = Mailbox { base: start, capacidad, handle };
    ADOPTADO = Some(m);
    Ok(m)
}

/// El buzon adoptado, si hay.
pub fn current() -> Option<Mailbox> {
    unsafe { ADOPTADO }
}

/// Deja de escuchar por el buzon.
pub fn forget(handle: u64) -> bool {
    unsafe {
        if ADOPTADO.map(|m| m.handle) == Some(handle) {
            ADOPTADO = None;
            return true;
        }
    }
    false
}

impl Mailbox {
    pub fn capacity(&self) -> u32 {
        self.capacidad
    }

    /// Saca el byte mas viejo que dejo el agente, si hay.
    ///
    /// # Safety
    ///
    /// El buzon tiene que seguir mapeado.
    pub unsafe fn pop(&self) -> Option<u8> {
        let cabeza = campo(self.base + OFF_PED_CABEZA);
        let cola = campo(self.base + OFF_PED_COLA);

        // `Acquire`: si se ve el indice nuevo, se ven tambien los bytes que el
        // agente escribio antes de avanzarlo.
        let c = cabeza.load(Ordering::Acquire);
        let t = cola.load(Ordering::Relaxed);
        if t == c {
            return None;
        }

        let pos = t & (self.capacidad - 1);
        let b = core::ptr::read_volatile((self.base + ENCABEZADO + pos as u64) as *const u8);

        // `Release`: el byte ya se leyo antes de decirle al agente que ese lugar
        // esta libre. Al reves podria sobreescribirlo antes de que lo leamos.
        cola.store(t.wrapping_add(1), Ordering::Release);
        Some(b)
    }

    /// Deja un byte de respuesta. `false` si no habia lugar.
    ///
    /// # Safety
    ///
    /// El buzon tiene que seguir mapeado.
    pub unsafe fn push(&self, b: u8) -> bool {
        let cabeza = campo(self.base + OFF_RES_CABEZA);
        let cola = campo(self.base + OFF_RES_COLA);

        let c = cabeza.load(Ordering::Relaxed);
        let t = cola.load(Ordering::Acquire);
        if c.wrapping_sub(t) >= self.capacidad {
            return false;
        }

        let pos = c & (self.capacidad - 1);
        core::ptr::write_volatile(
            (self.base + ENCABEZADO + self.capacidad as u64 + pos as u64) as *mut u8,
            b,
        );

        // `Release`: el byte esta escrito antes de que el agente vea el indice.
        cabeza.store(c.wrapping_add(1), Ordering::Release);
        true
    }
}

/// El tamano minimo de region para un buzon de esa capacidad.
///
/// Lo publica `describe` para que el agente no tenga que deducirlo.
pub const fn size_for(capacity: u32) -> u64 {
    ENCABEZADO + 2 * capacity as u64
}

/// Los offsets del acuerdo, para que `describe` los publique en vez de que el
/// agente los tenga horneados.
pub const LAYOUT: &[(&str, u64)] = &[
    ("magic", OFF_MAGIC),
    ("version", OFF_VERSION),
    ("capacity", OFF_CAPACIDAD),
    ("request_head", OFF_PED_CABEZA),
    ("request_tail", OFF_PED_COLA),
    ("response_head", OFF_RES_CABEZA),
    ("response_tail", OFF_RES_COLA),
    ("rings", ENCABEZADO),
];

/// El valor que el agente tiene que escribir en `magic`.
pub const EXPECTED_MAGIC: u32 = MAGIC;
/// La version del acuerdo que este kernel entiende.
pub const EXPECTED_VERSION: u32 = VERSION;

/// Olvida el buzon. Solo para los tests.
#[cfg(test)]
pub fn reset() {
    unsafe { ADOPTADO = None }
}
