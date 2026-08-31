//! Lo que el agente tiene reclamado.
//!
//! # Los handles son de la maquina (D14)
//!
//! No de la conexion. Sobreviven a que el agente se desconecte y siguen valiendo
//! cuando vuelve: el estado de la maquina no es una sesion. Por eso la tabla es
//! un estatico y no algo que se limpie cuando se corta el cable — y por eso
//! `describe` informa lo reclamado, para que al reconectar el agente recupere lo
//! que tenia.
//!
//! # Que se puede reclamar y que no
//!
//! Cualquier region de la maquina **menos la del kernel**. No es una politica
//! sobre lo que el agente deberia hacer (P6): es que el kernel es lo que le esta
//! prestando el servicio, y entregarle la memoria donde vive su propia pila no
//! seria libertad sino incoherencia.
//!
//! Todo lo demas se entrega, incluida la memoria del firmware y el MMIO, y
//! siempre se informa de que clase es. El agente decide con el dato a la vista,
//! que es distinto de que el kernel decida por el.

use crate::machine::Machine;
use crate::memory::Kind;

/// Cuantas cosas se pueden tener reclamadas a la vez.
const MAX: usize = 128;

/// Un pedazo de la maquina, reclamado.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub struct Claim {
    /// El numero con el que el agente lo nombra. Nunca se reusa.
    pub handle: u64,
    pub start: u64,
    pub bytes: u64,
    /// De que clase era la region de donde salio. Se informa para que el agente
    /// sepa que se llevo.
    pub kind: Kind,
    /// Si es alcanzable desde el nivel sin privilegio (D27).
    ///
    /// Es una propiedad de la memoria, no de la corrida: se pide al reclamarla.
    /// Y **tiene contracara**: una memoria marcada asi deja de ser ejecutable
    /// con privilegio, asi que sirve para `exec supervised` y no para `raw`.
    pub user: bool,
}

impl Claim {
    /// Primera direccion que ya no le pertenece.
    pub fn end(&self) -> u64 {
        self.start.saturating_add(self.bytes)
    }
}

/// Por que no se pudo.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub enum Error {
    /// Se pidieron cero bytes.
    Empty,
    /// La alineacion pedida no es potencia de dos.
    BadAlign,
    /// No hay un hueco libre asi.
    NoRoom,
    /// El rango pedido se sale de lo que la maquina informo.
    Unmapped,
    /// El rango pedido cae en memoria del kernel.
    IsKernel,
    /// El rango pedido pisa algo ya reclamado.
    Taken,
    /// No entran mas reclamos en la tabla.
    TableFull,
    /// Ese handle no existe.
    NoSuchHandle,
    /// El rango pedido se sale del reclamo.
    OutOfBounds,
    /// Se pidio alcanzable sin privilegio y no se pudo marcar.
    CannotGrant,
}

impl Error {
    /// El identificador que viaja por el protocolo (D6).
    pub fn code(&self) -> &'static str {
        match self {
            Error::Empty => "empty-request",
            Error::BadAlign => "align-not-power-of-two",
            Error::NoRoom => "no-room",
            Error::Unmapped => "unmapped",
            Error::IsKernel => "is-kernel-memory",
            Error::Taken => "already-claimed",
            Error::TableFull => "claim-table-full",
            Error::NoSuchHandle => "no-such-handle",
            Error::OutOfBounds => "out-of-bounds",
            Error::CannotGrant => "cannot-grant-user-access",
        }
    }
}

/// Lo que se pide.
#[derive(Clone, Copy)]
pub struct Request {
    pub bytes: u64,
    /// Una direccion fisica exacta. Es como se reclama MMIO: el BAR de un
    /// dispositivo esta donde esta, no donde haya lugar.
    pub at: Option<u64>,
    /// Potencia de dos. Muchos dispositivos exigen buffers alineados.
    pub align: u64,
    /// Tope duro. Existe porque hay dispositivos que solo hacen DMA por debajo
    /// de los 4 GiB.
    pub below: Option<u64>,
    /// Que sea alcanzable desde el nivel sin privilegio (D27).
    ///
    /// Pedirlo **redondea el tamano y la alineacion al bloque** de la tabla de
    /// paginas, porque el permiso no se puede decir mas fino que eso. El kernel
    /// informa lo que quedo de verdad.
    pub user: bool,
}

impl Default for Request {
    fn default() -> Self {
        Self { bytes: 0, at: None, align: 1, below: None, user: false }
    }
}

static mut TABLA: [Option<Claim>; MAX] = [None; MAX];

/// El proximo handle. Solo sube: un handle liberado **no se reusa**, para que
/// un pedido que llega tarde con un handle viejo de un error y no toque lo que
/// otro reclamo despues en el mismo lugar.
static mut PROXIMO: u64 = 1;

/// Todos los reclamos vigentes.
pub fn all() -> impl Iterator<Item = Claim> {
    let t = unsafe { &*core::ptr::addr_of!(TABLA) };
    t.iter().filter_map(|c| *c)
}

pub fn count() -> usize {
    all().count()
}

pub fn get(handle: u64) -> Option<Claim> {
    all().find(|c| c.handle == handle)
}

/// Devuelve lo reclamado. `true` si existia.
pub fn release(handle: u64) -> bool {
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLA) };
    for hueco in t.iter_mut() {
        if hueco.map(|c| c.handle) == Some(handle) {
            *hueco = None;
            return true;
        }
    }
    false
}

/// Reclama memoria.
pub fn claim(m: &Machine, r: Request) -> Result<Claim, Error> {
    if r.bytes == 0 {
        return Err(Error::Empty);
    }
    if r.align == 0 || !r.align.is_power_of_two() {
        return Err(Error::BadAlign);
    }

    // El permiso no se puede decir mas fino que un bloque de la tabla, asi que
    // pedirlo redondea. Se hace aca y no en quien llama para que la regla viva
    // en un solo lugar.
    let r = if r.user {
        Request {
            bytes: r.bytes.next_multiple_of(crate::paging::BLOQUE),
            align: r.align.max(crate::paging::BLOQUE),
            ..r
        }
    } else {
        r
    };

    match r.at {
        Some(dir) => reclamar_exacto(m, dir, r.bytes),
        None => buscar_hueco(m, r),
    }
}

/// Reclama un rango exacto. Es el camino del MMIO.
fn reclamar_exacto(m: &Machine, start: u64, bytes: u64) -> Result<Claim, Error> {
    let end = start.checked_add(bytes).ok_or(Error::Unmapped)?;

    // Tiene que estar entero adentro de lo que la maquina informo, y de una sola
    // clase: un rango a caballo de dos regiones distintas no se puede describir
    // con una sola etiqueta sin mentir.
    let region = m.region_containing(start).ok_or(Error::Unmapped)?;
    if end > region.end() {
        return Err(Error::Unmapped);
    }
    if region.kind == Kind::Kernel {
        return Err(Error::IsKernel);
    }
    if pisa_algo(start, end) {
        return Err(Error::Taken);
    }
    anotar(start, bytes, region.kind)
}

/// Busca el primer hueco libre que sirva.
fn buscar_hueco(m: &Machine, r: Request) -> Result<Claim, Error> {
    for region in m.regions {
        // Solo RAM utilizable: lo demas se puede pedir, pero por direccion
        // exacta y sabiendo lo que se pide.
        if region.kind != Kind::Free {
            continue;
        }

        let mut candidato = alinear(region.start, r.align);

        loop {
            let Some(fin) = candidato.checked_add(r.bytes) else { break };
            if fin > region.end() {
                break;
            }
            if let Some(tope) = r.below {
                if fin > tope {
                    break;
                }
            }

            match primero_que_pisa(candidato, fin) {
                // Libre: es este.
                None => return anotar(candidato, r.bytes, region.kind),
                // Ocupado: se salta hasta despues de lo que estorba. Avanzar de
                // a poco recorreria byte por byte una maquina con gigabytes.
                Some(c) => {
                    let siguiente = alinear(c.end(), r.align);
                    if siguiente <= candidato {
                        break; // no avanza: se corta en vez de girar en falso
                    }
                    candidato = siguiente;
                }
            }
        }
    }
    Err(Error::NoRoom)
}

/// Anota que ese reclamo quedo alcanzable sin privilegio.
pub fn mark_user(handle: u64) {
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLA) };
    for c in t.iter_mut().flatten() {
        if c.handle == handle {
            c.user = true;
        }
    }
}

fn anotar(start: u64, bytes: u64, kind: Kind) -> Result<Claim, Error> {
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLA) };
    let hueco = t.iter_mut().find(|c| c.is_none()).ok_or(Error::TableFull)?;

    let handle = unsafe {
        let h = PROXIMO;
        PROXIMO += 1;
        h
    };

    let c = Claim { handle, start, bytes, kind, user: false };
    *hueco = Some(c);
    Ok(c)
}

fn primero_que_pisa(start: u64, end: u64) -> Option<Claim> {
    all().find(|c| start < c.end() && end > c.start)
}

fn pisa_algo(start: u64, end: u64) -> bool {
    primero_que_pisa(start, end).is_some()
}

fn alinear(v: u64, a: u64) -> u64 {
    // `a` ya se comprobo potencia de dos.
    (v + a - 1) & !(a - 1)
}

/// Comprueba que `off..off+len` caiga dentro del reclamo, y devuelve la
/// direccion fisica donde empieza.
pub fn range_of(handle: u64, off: u64, len: u64) -> Result<u64, Error> {
    let c = get(handle).ok_or(Error::NoSuchHandle)?;
    let fin = off.checked_add(len).ok_or(Error::OutOfBounds)?;
    if fin > c.bytes {
        return Err(Error::OutOfBounds);
    }
    Ok(c.start + off)
}

/// Borra la tabla. Solo para los tests: la maquina de verdad no olvida (D14).
#[cfg(test)]
pub fn reset() {
    unsafe {
        *core::ptr::addr_of_mut!(TABLA) = [None; MAX];
        PROXIMO = 1;
    }
}
