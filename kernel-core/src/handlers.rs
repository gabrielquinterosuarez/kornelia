//! Los handlers de interrupcion que instala el agente (D9).
//!
//! # Por que los escribe el agente
//!
//! Una interrupcion se atiende en microsegundos; el agente, que es externo,
//! contesta en segundos. Nunca puede estar en ese lazo (P3). Asi que no es que
//! el kernel le pase el evento al agente: el agente **escribe el codigo que
//! corre sin el**, y el kernel solo lo pone en la tabla.
//!
//! El patron previsto es que ese handler escriba en un ring buffer y el agente
//! lea cuando vuelva. La maquina junta eventos sola durante horas.
//!
//! # Como se nombra una interrupcion (D3)
//!
//! Aca hubo que elegir, porque las dos arquitecturas cuentan distinto:
//!
//! - En x86_64 hay **dos** numeros: el cable del aparato y la ranura de la tabla
//!   de interrupciones adonde el IO-APIC lo manda. Son cosas distintas.
//! - En aarch64 hay **uno**: el GIC entrega el numero de la interrupcion y el
//!   reparto se hace en software.
//!
//! El protocolo usa el numero con el que **la maquina identifica la fuente** —el
//! que informan las tablas de ACPI y que `describe` ya publica— y cada
//! arquitectura se arregla con lo que necesite por dentro. Poner "vector" en el
//! protocolo hubiera sido hornear el modelo de x86.
//!
//! # Donde corren
//!
//! En el nucleo que atiende el protocolo, y **con privilegio** — el hardware no
//! sabe entregar una interrupcion a un nivel sin privilegio, asi que D27 no los
//! cubre y eso esta anotado en D27 mismo.

/// Cuantos handlers puede tener instalados el agente a la vez.
///
/// El numero es chico a proposito: cada uno necesita una ranura reservada en la
/// tabla de interrupciones de x86, y reservarlas de mas seria quitarle lugar a
/// lo que venga.
pub const MAX: usize = 8;

/// Un handler instalado.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub struct Handler {
    /// El numero con el que la maquina identifica esta interrupcion.
    pub interrupt: u32,
    /// Donde esta el codigo del agente.
    pub entry: u64,
    /// Si el kernel no pone nada alrededor.
    pub raw: bool,
    /// Cuantas veces se atendio. El agente lo lee con `describe` para saber si
    /// su aparato esta hablando, sin tener que instrumentar su propio codigo.
    pub count: u64,
    /// Las escrituras que hacen sonar esta interrupcion **a proposito**.
    ///
    /// Sirve para que el agente pueda probar su propio handler sin depender de
    /// que el aparato se digne a hablar. Se publica con la misma forma que el
    /// timbre del buzon: una lista de escrituras, sin nombrar el controlador.
    pub trigger: crate::channel::Doorbell,
}

/// Por que no se pudo instalar.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub enum Error {
    /// Ya hay un handler para esa interrupcion.
    Taken,
    /// No quedan ranuras.
    TableFull,
    /// La maquina no usa ese numero de interrupcion.
    NoSuchInterrupt,
    /// Es una interrupcion del kernel: el cable serie o el timbre del buzon.
    /// Entregarla seria quedarse sin cordon.
    IsKernels,
    /// Esta arquitectura no tiene un camino mas crudo que el que ya usa.
    NoRawPath,
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::Taken => "already-installed",
            Error::TableFull => "handler-table-full",
            Error::NoSuchInterrupt => "no-such-interrupt",
            Error::IsKernels => "is-kernel-interrupt",
            Error::NoRawPath => "no-raw-path-on-this-machine",
        }
    }
}

static mut TABLA: [Option<Handler>; MAX] = [None; MAX];

/// Reserva una ranura para esa interrupcion.
///
/// Devuelve el numero de ranura, que es lo que la arquitectura usa para saber
/// cual handler llamar.
pub fn reserve(interrupt: u32, entry: u64, raw: bool) -> Result<usize, Error> {
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLA) };

    if t.iter().flatten().any(|h| h.interrupt == interrupt) {
        return Err(Error::Taken);
    }
    let slot = t.iter().position(|h| h.is_none()).ok_or(Error::TableFull)?;
    t[slot] = Some(Handler {
        interrupt,
        entry,
        raw,
        count: 0,
        trigger: crate::channel::Doorbell::vacio(),
    });
    Ok(slot)
}

/// Suelta una ranura, si la instalacion no salio.
pub fn release_slot(slot: usize) {
    if slot < MAX {
        unsafe { (*core::ptr::addr_of_mut!(TABLA))[slot] = None }
    }
}

/// Anota como se hace sonar esa interrupcion a proposito.
pub fn set_trigger(slot: usize, t: crate::channel::Doorbell) {
    if slot >= MAX {
        return;
    }
    unsafe {
        if let Some(h) = &mut (*core::ptr::addr_of_mut!(TABLA))[slot] {
            h.trigger = t;
        }
    }
}

/// El handler de esa ranura.
pub fn at(slot: usize) -> Option<Handler> {
    if slot >= MAX {
        return None;
    }
    unsafe { (*core::ptr::addr_of!(TABLA))[slot] }
}

/// La ranura que atiende esa interrupcion.
pub fn slot_of(interrupt: u32) -> Option<usize> {
    let t = unsafe { &*core::ptr::addr_of!(TABLA) };
    t.iter().position(|h| h.map(|x| x.interrupt) == Some(interrupt))
}

/// Anota que se atendio una vez.
///
/// Lo llama la arquitectura desde el camino de la interrupcion.
pub fn served(slot: usize) {
    if slot >= MAX {
        return;
    }
    unsafe {
        if let Some(h) = &mut (*core::ptr::addr_of_mut!(TABLA))[slot] {
            h.count = h.count.wrapping_add(1);
        }
    }
}

pub fn all() -> impl Iterator<Item = Handler> {
    let t = unsafe { &*core::ptr::addr_of!(TABLA) };
    t.iter().filter_map(|h| *h)
}

pub fn count() -> usize {
    all().count()
}

/// Borra la tabla. Solo para los tests.
#[cfg(test)]
pub fn reset() {
    unsafe { *core::ptr::addr_of_mut!(TABLA) = [None; MAX] }
}
