//! Los nucleos que el agente tiene reclamados (D13).
//!
//! # Por que el agente quiere varios
//!
//! El kernel no es plural: hay un solo agente y la maquina entera es suya. Pero
//! el agente si puede serlo — si quiere cinco cosas a la vez, reclama cinco
//! nucleos y pone codigo distinto en cada uno. No hace falta que el kernel tenga
//! procesos para que el agente tenga paralelismo (P2).
//!
//! # Como arranca un nucleo
//!
//! No se parece en nada entre arquitecturas, y por eso el como vive del lado de
//! cada una:
//!
//! - **aarch64**: se le pide al firmware con PSCI, que es una llamada. El nucleo
//!   arranca directo en la direccion que se le da.
//! - **x86_64**: se le manda una secuencia de interrupciones por el APIC, y el
//!   nucleo arranca en **modo real de 16 bits**, como en 1978. Hay que darle un
//!   trampolin en memoria baja que lo lleve hasta 64 bits.
//!
//! Lo que si es igual en las dos: el nucleo que arranca tiene que armarse su
//! pila, cargar las tablas de paginas y la tabla de excepciones **antes** de
//! tocar cualquier cosa del kernel.
//!
//! # Por que atomicos y no un `bool`
//!
//! El nucleo nuevo escribe "llegue" y el que lo arranco lo lee. Son dos CPUs
//! distintas mirando la misma memoria, y ahi el orden de las escrituras deja de
//! ser obvio: x86 tiene un modelo de memoria fuerte y perdona una barrera
//! faltante, ARM reordena y no. Es exactamente la clase de bug que D22 quiere
//! cazar, y la unica forma de no tenerlo es decir explicitamente que se
//! sincroniza.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Cuantos nucleos se pueden tener reclamados.
pub const MAX: usize = 16;

/// En que anda un nucleo reclamado.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub enum State {
    /// Se le pidio que arranque y todavia no contesto.
    Starting,
    /// Arranco, se configuro solo, y esta esperando trabajo.
    Idle,
    /// Se le pidio que arranque y nunca llego.
    Failed,
}

impl State {
    /// El identificador que viaja por el protocolo (D6).
    pub fn code(&self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Idle => "idle",
            State::Failed => "failed",
        }
    }
}

/// Un nucleo reclamado.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub struct Core {
    pub handle: u64,
    /// El identificador con el que la maquina lo nombra: APIC ID en x86_64,
    /// MPIDR en aarch64. Es el que informa `describe` (P4).
    pub id: u64,
    pub state: State,
}

/// Por que no se pudo.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub enum Error {
    /// La maquina no informa ningun nucleo con ese identificador.
    NoSuchCore,
    /// La maquina lo informa, pero dice que no esta disponible.
    NotUsable,
    /// Es el nucleo sobre el que corre el kernel. Ese no se reclama: es el que
    /// esta atendiendo el pedido.
    IsBootCore,
    /// Ya estaba reclamado.
    Taken,
    /// No entran mas nucleos en la tabla.
    TableFull,
    /// Se le pidio que arranque y no llego a tiempo.
    NeverArrived,
    /// Esta arquitectura todavia no sabe arrancar nucleos.
    NotSupported,
    /// La maquina no informa como arrancarlos.
    NoMechanism,
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::NoSuchCore => "no-such-core",
            Error::NotUsable => "core-not-usable",
            Error::IsBootCore => "is-boot-core",
            Error::Taken => "already-claimed",
            Error::TableFull => "core-table-full",
            Error::NeverArrived => "core-never-arrived",
            Error::NotSupported => "not-supported",
            Error::NoMechanism => "no-start-mechanism",
        }
    }
}

/// La ranura de cada nucleo. El que arranca escribe aca su identificador mas
/// uno, para que cero siga significando "todavia no llego".
static ARRIVAL: [AtomicU64; MAX] = [const { AtomicU64::new(0) }; MAX];

/// Cuantas ranuras estan en uso. Solo la escribe el nucleo de arranque.
static USED: AtomicUsize = AtomicUsize::new(0);

static mut TABLE: [Option<Core>; MAX] = [None; MAX];
static mut NEXT: u64 = 1;

/// Lo que un nucleo recien arrancado llama para avisar que llego.
///
/// Se llama **desde el nucleo nuevo**, ya con sus tablas de paginas y su tabla
/// de excepciones puestas: antes de eso no se puede tocar memoria compartida.
pub fn arrived(slot: usize, id: u64) {
    if slot < MAX {
        // `Release` empareja con el `Acquire` del otro lado: todo lo que este
        // nucleo escribio antes de esto queda visible para quien lea la llegada.
        ARRIVAL[slot].store(id.wrapping_add(1), Ordering::Release);
    }
}

/// Si el nucleo de esa ranura ya aviso que llego.
pub fn has_arrived(slot: usize) -> bool {
    slot < MAX && ARRIVAL[slot].load(Ordering::Acquire) != 0
}

/// Reserva una ranura para un nucleo que se va a arrancar.
///
/// Devuelve el numero de ranura, que es lo que el codigo de arranque le pasa al
/// nucleo nuevo para que sepa cual es la suya.
pub fn reserve(id: u64) -> Result<(usize, u64), Error> {
    let slot = USED.load(Ordering::Relaxed);
    if slot >= MAX {
        return Err(Error::TableFull);
    }

    let handle = unsafe {
        let h = NEXT;
        NEXT += 1;
        h
    };

    ARRIVAL[slot].store(0, Ordering::Release);
    unsafe {
        (&mut *core::ptr::addr_of_mut!(TABLE))[slot] =
            Some(Core { handle, id, state: State::Starting });
    }
    USED.store(slot + 1, Ordering::Release);
    Ok((slot, handle))
}

/// Anota como termino el arranque de esa ranura.
pub fn settle(slot: usize, state: State) {
    if slot >= MAX {
        return;
    }
    unsafe {
        if let Some(c) = &mut (&mut *core::ptr::addr_of_mut!(TABLE))[slot] {
            c.state = state;
        }
    }
}

/// Si ese identificador ya esta reclamado.
pub fn is_claimed(id: u64) -> bool {
    all().any(|c| c.id == id)
}

/// La ranura de un nucleo reclamado, buscada por su handle.
///
/// La ranura es la posicion en la tabla, y es lo que usa todo lo que trabaja
/// por nucleo: el buzon de trabajo, el bloque privado, la pila de excepcion. El
/// handle es lo que ve el agente (D14), y esto traduce de uno al otro.
pub fn slot_of(handle: u64) -> Option<usize> {
    let n = USED.load(Ordering::Acquire);
    let t = unsafe { &*core::ptr::addr_of!(TABLE) };
    t[..n.min(MAX)]
        .iter()
        .position(|c| matches!(c, Some(c) if c.handle == handle))
}

/// Con que numero nombra la maquina al nucleo de esa ranura.
pub fn id_of(slot: usize) -> Option<u64> {
    if slot >= MAX {
        return None;
    }
    let t = unsafe { &*core::ptr::addr_of!(TABLE) };
    t[slot].map(|c| c.id)
}

pub fn all() -> impl Iterator<Item = Core> {
    let n = USED.load(Ordering::Acquire);
    let t = unsafe { &*core::ptr::addr_of!(TABLE) };
    t[..n.min(MAX)].iter().filter_map(|c| *c)
}

pub fn count() -> usize {
    all().count()
}

/// Borra la tabla. Solo para los tests.
#[cfg(test)]
pub fn reset() {
    for l in &ARRIVAL {
        l.store(0, Ordering::Release);
    }
    USED.store(0, Ordering::Release);
    unsafe {
        *core::ptr::addr_of_mut!(TABLE) = [None; MAX];
        NEXT = 1;
    }
}
