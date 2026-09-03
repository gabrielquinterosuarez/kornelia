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
    /// Estaba corriendo codigo que no volvio y no se dejo cortar. No se le manda
    /// mas trabajo: el nucleo esta ahi pero no es usable.
    Lost,
}

impl State {
    /// El identificador que viaja por el protocolo (D6).
    pub fn code(&self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Idle => "idle",
            State::Failed => "failed",
            State::Lost => "lost",
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
    /// La maquina **si** sabe arrancar nucleos, pero el agente reclamo la
    /// pagina baja por la que tienen que pasar.
    ///
    /// Va aparte de `NoMechanism` a proposito: son dos situaciones que el
    /// agente resuelve distinto. Una dice "esta maquina no puede" y la otra
    /// "solta ese reclamo". Decirle la primera cuando es la segunda lo manda a
    /// buscar el problema donde no esta.
    TrampolineTaken,
    /// Tiene trabajo en curso. No se suelta con codigo de alguien adentro.
    Working,
    /// Se le pidio que corte y no contesto. Queda perdido: su codigo enmascaro
    /// las interrupciones, y desde afuera no hay como sacarlo de ahi.
    DidNotStop,
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
            Error::TrampolineTaken => "trampoline-page-taken",
            Error::Working => "core-working",
            Error::DidNotStop => "core-did-not-stop",
        }
    }
}

/// La ranura de cada nucleo. El que arranca escribe aca su identificador mas
/// uno, para que cero siga significando "todavia no llego".
static ARRIVAL: [AtomicU64; MAX] = [const { AtomicU64::new(0) }; MAX];

/// Cuantas ranuras estan en uso. Solo la escribe el nucleo de arranque.
static USED: AtomicUsize = AtomicUsize::new(0);

static mut TABLE: [Option<Core>; MAX] = [None; MAX];

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

/// Reserva una ranura para un nucleo.
///
/// Devuelve el numero de ranura, que es lo que el codigo de arranque le pasa al
/// nucleo nuevo para que sepa cual es la suya.
///
/// **La ranura es del nucleo fisico para siempre**, una vez que arranco: su
/// bucle duerme esperando trabajo en el buzon de *esa* ranura, asi que un nucleo
/// que se solto y se vuelve a reclamar tiene que recibir la misma. Lo que si es
/// nuevo es el handle — esos no se reusan nunca (D14), para que un pedido que
/// llega tarde con uno viejo de un error en vez de tocar el reclamo de otro.
pub fn reserve(id: u64) -> Result<(usize, u64), Error> {
    let n = USED.load(Ordering::Acquire);
    let t = unsafe { &*core::ptr::addr_of!(TABLE) };

    // Si ese nucleo ya arranco alguna vez, su ranura es esa y no otra.
    let mut slot = (0..n.min(MAX)).find(|&i| ARRIVAL[i].load(Ordering::Acquire) == id.wrapping_add(1));

    if slot.is_none() {
        // Una libre: primero las que quedaron vacias al soltarlas, y si no, la
        // siguiente sin usar.
        slot = (0..n.min(MAX)).find(|&i| t[i].is_none() && ARRIVAL[i].load(Ordering::Acquire) == 0);
    }
    let slot = match slot {
        Some(s) => s,
        None if n < MAX => n,
        None => return Err(Error::TableFull),
    };

    let handle = crate::handles::next();

    // El estado inicial depende de si hay que arrancarlo o ya esta andando: un
    // nucleo que vuelve de un `release` no se reinicia, sigue donde estaba.
    let state = if has_arrived(slot) { State::Idle } else {
        ARRIVAL[slot].store(0, Ordering::Release);
        State::Starting
    };
    unsafe {
        (&mut *core::ptr::addr_of_mut!(TABLE))[slot] = Some(Core { handle, id, state });
    }
    if slot >= n {
        USED.store(slot + 1, Ordering::Release);
    }
    Ok((slot, handle))
}

/// Devuelve un nucleo reclamado. `true` si el handle era de uno.
///
/// **No lo apaga ni lo reinicia**: el nucleo sigue vivo, durmiendo en su buzon,
/// y por eso se lo puede volver a reclamar sin arrancarlo de nuevo. Lo que se
/// devuelve es el derecho a mandarle trabajo.
///
/// No se suelta uno que tenga trabajo **en curso**: mientras su codigo corre, el
/// nucleo tiene dueno, y entregarselo a otro reclamo seria darle un nucleo con
/// el codigo de alguien mas adentro.
pub fn release(handle: u64) -> Result<usize, Error> {
    let Some(slot) = slot_of(handle) else {
        return Err(Error::NoSuchCore);
    };
    if crate::work::is_busy(slot) {
        return Err(Error::Working);
    }
    unsafe {
        (&mut *core::ptr::addr_of_mut!(TABLE))[slot] = None;
    }
    Ok(slot)
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
    }
}
