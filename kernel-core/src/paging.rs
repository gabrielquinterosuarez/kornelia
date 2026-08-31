//! El plan de mapeo (D12).
//!
//! Aca se decide **que** hay que mapear y con que atributos. Armar las tablas es
//! harina de otro costal y vive en cada arquitectura: el formato es lo menos
//! portable que hay — x86_64 usa cuatro o cinco niveles, aarch64 tiene TTBR0 y
//! TTBR1, RISC-V tiene Sv39/48/57 — pero la decision de que va cacheable y que
//! no sale del mapa de memoria, que es igual en todas.
//!
//! # Por que tablas propias
//!
//! Hasta ahora se corria sobre las que dejo armadas el firmware. Viven en
//! memoria que `ExitBootServices` convirtio en libre, asi que `mem.claim` se
//! las podria entregar al agente. Y a diferencia de la pila, esto no falla
//! donde se escribe: falla en la proxima traduccion de direccion, en cualquier
//! parte.
//!
//! # Por que identity map
//!
//! Virtual igual a fisica. Los dispositivos hablan en direcciones fisicas, asi
//! que cualquier otra cosa obligaria al agente a llevar dos sistemas de
//! coordenadas mientras escribe un driver. El que quiera tablas propias las
//! arma y las carga desde su codigo en `exec` (P2).

use crate::machine::Machine;
use crate::memory::Kind;

/// Una pagina de 1 GiB. El grano del identity map: con paginas asi, mapear
/// toda la RAM cuesta unas pocas entradas y la presion sobre el TLB es casi
/// nula.
pub const GIB: u64 = 1 << 30;

/// Como hay que mapear una pagina.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub enum Attr {
    /// RAM comun: cacheable.
    Memory,
    /// Registros de dispositivo: **no cacheable**. Si la CPU cachea una
    /// escritura a un registro, el dispositivo no se entera nunca.
    Device,
}

/// Cuantas paginas de 1 GiB hacen falta para cubrir todo lo que la maquina
/// informo.
///
/// Se cubre hasta la region mas alta y no solo hasta donde llega la RAM: ahi
/// arriba viven los BARs de PCIe, y el agente va a querer escribirles.
pub fn span_gib(m: &Machine) -> u64 {
    let mut tope = 0u64;
    for r in m.regions {
        tope = tope.max(r.end());
    }
    tope.div_ceil(GIB)
}

/// Con que atributos hay que mapear la pagina numero `gib`.
///
/// Ante la duda, `Device`. Cachear RAM que en realidad era un registro rompe el
/// dispositivo de una forma dificil de diagnosticar; no cachear RAM de verdad
/// solo la hace lenta. El error caro y el barato no son simetricos.
pub fn attr_of(m: &Machine, gib: u64) -> Attr {
    let inicio = gib * GIB;
    let fin = inicio + GIB;

    let mut hay_memoria = false;

    for r in m.regions {
        // Sin solapamiento con esta pagina, no dice nada de ella.
        if r.start >= fin || r.end() <= inicio {
            continue;
        }
        match r.kind {
            // Un solo registro adentro alcanza para que la pagina entera tenga
            // que ser no cacheable: el grano del mapeo es 1 GiB.
            Kind::Mmio => return Attr::Device,
            // Lo que la maquina no supo explicar no se asume RAM.
            Kind::Reserved | Kind::Broken | Kind::Other(_) => {}
            _ => hay_memoria = true,
        }
    }

    if hay_memoria {
        Attr::Memory
    } else {
        // Ni memoria conocida ni nada: un hueco. Puede haber un dispositivo que
        // este kernel todavia no sabe que existe.
        Attr::Device
    }
}

/// Lo que quedo mapeado, para poder contarlo por el cordon.
#[derive(Clone, Copy)]
pub struct Mapping {
    /// Cuantas paginas de 1 GiB se mapearon.
    pub gib: u64,
    /// De esas, cuantas quedaron sin cachear.
    pub device_gib: u64,
    /// Direccion fisica de la tabla raiz, ya releida del registro. Se informa
    /// para poder comprobar que cayo en memoria del kernel.
    pub root: u64,
}

/// El grano fino: cuando un pedazo de 1 GiB tiene memoria del kernel adentro,
/// se parte en bloques de este tamano para poder distinguir.
///
/// Dos MiB y no cuatro KiB porque partir un GiB en paginas de 4 KiB serian
/// quinientas doce tablas. La contra es que memoria del agente que caiga en el
/// mismo bloque de 2 MiB que el kernel queda tambien fuera de su alcance — y eso
/// se informa en vez de que lo descubra chocandose.
pub const BLOQUE: u64 = 2 << 20;

/// Si ese rango pisa memoria del kernel.
///
/// Es lo que decide si un pedazo se le puede dejar alcanzar al agente. La
/// pregunta se hace sobre el mapa real y no sobre donde uno cree que esta el
/// kernel: la respuesta la da la maquina (P4).
pub fn touches_kernel(m: &Machine, start: u64, end: u64) -> bool {
    m.regions
        .iter()
        .any(|r| r.kind == Kind::Kernel && r.start < end && r.end() > start)
}

/// Si esa pagina de 1 GiB necesita partirse en bloques mas chicos.
pub fn needs_split(m: &Machine, gib: u64) -> bool {
    touches_kernel(m, gib * GIB, (gib + 1) * GIB)
}
