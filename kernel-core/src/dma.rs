//! Que aparato puede tocar que memoria (D8).
//!
//! # Por que hay una tabla y no solo una escritura al IOMMU
//!
//! `dma.allow` programa el silicio, y con eso alcanzaria para que ande. Pero al
//! soltar la memoria hay que **sacar** ese permiso, y para eso hay que acordarse
//! de a quien se le habia dado: memoria devuelta que sigue alcanzable por un
//! aparato es exactamente el agujero silencioso que el IOMMU viene a cerrar. Un
//! reclamo nuevo caeria ahi mismo y el aparato de antes seguiria escribiendole.
//!
//! Es el mismo criterio que el permiso sin privilegio de D27: se anota al darlo
//! y se saca al soltar.

/// Cuantos permisos se pueden tener a la vez. Cada uno es un par
/// (aparato, reclamo); un aparato con tres reclamos ocupa tres.
pub const MAX: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub struct Grant {
    /// Con que numero lo nombra el bus: en PCIe, bus, dispositivo y funcion.
    pub device: u32,
    pub handle: u64,
    pub start: u64,
    pub bytes: u64,
}

static mut TABLE: [Option<Grant>; MAX] = [None; MAX];

/// Por que no se pudo.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub enum Error {
    /// Ya estaba declarado. No es un fallo del agente, pero decirlo es mejor
    /// que programar el silicio dos veces.
    Already,
    /// No entran mas permisos.
    TableFull,
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::Already => "already allowed",
            Error::TableFull => "grant table full",
        }
    }
}

/// Anota un permiso. Lo llama el verbo **despues** de que el silicio lo acepto:
/// una tabla que diga lo que el hardware no hace seria peor que no tenerla.
pub fn record(g: Grant) -> Result<(), Error> {
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLE) };
    if t.iter().flatten().any(|x| x.device == g.device && x.handle == g.handle) {
        return Err(Error::Already);
    }
    for slot in t.iter_mut() {
        if slot.is_none() {
            *slot = Some(g);
            return Ok(());
        }
    }
    Err(Error::TableFull)
}

/// Saca de la tabla todo lo que se le habia permitido a ese reclamo.
///
/// Devuelve cuantos habia, para que quien llama sepa a cuantos aparatos hay que
/// quitarles el acceso en el silicio.
pub fn forget(handle: u64, mut each: impl FnMut(Grant)) -> usize {
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLE) };
    let mut n = 0;
    for slot in t.iter_mut() {
        if let Some(g) = *slot {
            if g.handle == handle {
                each(g);
                *slot = None;
                n += 1;
            }
        }
    }
    n
}

pub fn all() -> impl Iterator<Item = Grant> {
    let t = unsafe { &*core::ptr::addr_of!(TABLE) };
    t.iter().filter_map(|g| *g)
}

pub fn count() -> usize {
    all().count()
}

/// Borra la tabla. Solo para los tests.
#[cfg(test)]
pub fn reset() {
    unsafe { *core::ptr::addr_of_mut!(TABLE) = [None; MAX] };
}
