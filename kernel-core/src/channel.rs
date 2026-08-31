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
const HEADER: u64 = 32;

// Offsets de cada campo.
const OFF_MAGIC: u64 = 0;
const OFF_VERSION: u64 = 4;
const OFF_CAPACITY: u64 = 8;
const OFF_REQ_HEAD: u64 = 16;
const OFF_REQ_TAIL: u64 = 20;
const OFF_RESP_HEAD: u64 = 24;
const OFF_RESP_TAIL: u64 = 28;

/// Un buzon ya verificado.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub struct Mailbox {
    /// Donde empieza, en fisicas.
    base: u64,
    /// Cuantos bytes tiene cada anillo.
    capacity: u32,
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
static mut ADOPTED: Option<Mailbox> = None;

/// Un entero del encabezado, como atomico.
///
/// # Safety
///
/// `addr` tiene que estar mapeada y alineada a 4.
unsafe fn field(addr: u64) -> &'static AtomicU32 {
    &*(addr as *const AtomicU32)
}

/// Mira si en `start` hay un buzon armado, y lo adopta.
///
/// # Safety
///
/// `start..start+bytes` tiene que estar mapeada.
pub unsafe fn adopt(handle: u64, start: u64, bytes: u64) -> Result<Mailbox, Error> {
    if bytes < HEADER {
        return Err(Error::TooSmall);
    }
    if field(start + OFF_MAGIC).load(Ordering::Acquire) != MAGIC {
        return Err(Error::NoMagic);
    }
    if field(start + OFF_VERSION).load(Ordering::Relaxed) != VERSION {
        return Err(Error::BadVersion);
    }

    let capacity = field(start + OFF_CAPACITY).load(Ordering::Relaxed);
    // Potencia de dos para que dar la vuelta sea una mascara y no una division.
    if capacity == 0 || !capacity.is_power_of_two() {
        return Err(Error::BadCapacity);
    }
    // Los dos anillos tienen que entrar en lo que el agente reclamo. Si no
    // entraran, el kernel escribiria fuera de lo que le entregaron.
    let needed = HEADER + 2 * capacity as u64;
    if needed > bytes {
        return Err(Error::TooSmall);
    }

    let m = Mailbox { base: start, capacity, handle };
    ADOPTED = Some(m);
    Ok(m)
}

/// El buzon adoptado, si hay.
pub fn current() -> Option<Mailbox> {
    unsafe { ADOPTED }
}

/// Deja de escuchar por el buzon.
pub fn forget(handle: u64) -> bool {
    unsafe {
        if ADOPTED.map(|m| m.handle) == Some(handle) {
            ADOPTED = None;
            return true;
        }
    }
    false
}

impl Mailbox {
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Saca el byte mas viejo que dejo el agente, si hay.
    ///
    /// # Safety
    ///
    /// El buzon tiene que seguir mapeado.
    pub unsafe fn pop(&self) -> Option<u8> {
        let head = field(self.base + OFF_REQ_HEAD);
        let tail = field(self.base + OFF_REQ_TAIL);

        // `Acquire`: si se ve el indice nuevo, se ven tambien los bytes que el
        // agente escribio antes de avanzarlo.
        let c = head.load(Ordering::Acquire);
        let t = tail.load(Ordering::Relaxed);
        if t == c {
            return None;
        }

        let pos = t & (self.capacity - 1);
        let b = core::ptr::read_volatile((self.base + HEADER + pos as u64) as *const u8);

        // `Release`: el byte ya se leyo antes de decirle al agente que ese lugar
        // esta libre. Al reves podria sobreescribirlo antes de que lo leamos.
        tail.store(t.wrapping_add(1), Ordering::Release);
        Some(b)
    }

    /// Deja un byte de respuesta. `false` si no habia lugar.
    ///
    /// # Safety
    ///
    /// El buzon tiene que seguir mapeado.
    pub unsafe fn push(&self, b: u8) -> bool {
        let head = field(self.base + OFF_RESP_HEAD);
        let tail = field(self.base + OFF_RESP_TAIL);

        let c = head.load(Ordering::Relaxed);
        let t = tail.load(Ordering::Acquire);
        if c.wrapping_sub(t) >= self.capacity {
            return false;
        }

        let pos = c & (self.capacity - 1);
        core::ptr::write_volatile(
            (self.base + HEADER + self.capacity as u64 + pos as u64) as *mut u8,
            b,
        );

        // `Release`: el byte esta escrito antes de que el agente vea el indice.
        head.store(c.wrapping_add(1), Ordering::Release);
        true
    }
}

/// El tamano minimo de region para un buzon de esa capacidad.
///
/// Lo publica `describe` para que el agente no tenga que deducirlo.
pub const fn size_for(capacity: u32) -> u64 {
    HEADER + 2 * capacity as u64
}

/// Los offsets del acuerdo, para que `describe` los publique en vez de que el
/// agente los tenga horneados.
pub const LAYOUT: &[(&str, u64)] = &[
    ("magic", OFF_MAGIC),
    ("version", OFF_VERSION),
    ("capacity", OFF_CAPACITY),
    ("request_head", OFF_REQ_HEAD),
    ("request_tail", OFF_REQ_TAIL),
    ("response_head", OFF_RESP_HEAD),
    ("response_tail", OFF_RESP_TAIL),
    ("rings", HEADER),
];

/// El valor que el agente tiene que escribir en `magic`.
pub const EXPECTED_MAGIC: u32 = MAGIC;
/// La version del acuerdo que este kernel entiende.
pub const EXPECTED_VERSION: u32 = VERSION;

/// Olvida el buzon. Solo para los tests.
#[cfg(test)]
pub fn reset() {
    unsafe { ADOPTED = None }
}

// ---------------------------------------------------------------------------
// El timbre
// ---------------------------------------------------------------------------

/// Como tocarle el timbre al kernel.
///
/// Se describe como **una lista de escrituras a hacer en orden**, y no como
/// "escribile al APIC" o "escribile al GIC", a proposito: asi el agente no
/// necesita saber que controlador de interrupciones tiene la maquina. Hace las
/// escrituras que le dijeron y listo (P4).
///
/// En aarch64 alcanza una; en x86_64 son dos, porque el registro que dispara la
/// llamada esta separado del que dice a quien.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub struct Doorbell {
    /// (direccion, valor, cuantos bytes escribir).
    pub writes: [(u64, u64, u8); 2],
    pub count: usize,
    /// El numero de timbre, para poder informarlo.
    pub id: u32,
}

impl Doorbell {
    pub const fn blank() -> Self {
        Self { writes: [(0, 0, 0); 2], count: 0, id: 0 }
    }
}

static mut BELL: Option<Doorbell> = None;

/// Anota como se toca el timbre, para que `describe` lo publique.
pub fn set_doorbell(d: Doorbell) {
    unsafe { BELL = Some(d) }
}

/// Como se toca el timbre, si hay.
pub fn doorbell() -> Option<Doorbell> {
    unsafe { BELL }
}

/// Cuantas veces sono el timbre.
///
/// Se cuenta para poder **comprobar que el mecanismo anda**, que de otra forma
/// no seria observable: el unico canal por el que un cliente puede mirar es el
/// cable, y usarlo despierta al nucleo igual. Si este numero sube, la
/// interrupcion del agente llego y el kernel la atendio.
static RINGS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Lo llama quien atiende el timbre. Nada mas: el trabajo lo hace el bucle.
pub fn rang() {
    RINGS.fetch_add(1, Ordering::Relaxed);
}

/// Cuantas veces sono.
pub fn rings() -> u64 {
    RINGS.load(Ordering::Relaxed)
}
