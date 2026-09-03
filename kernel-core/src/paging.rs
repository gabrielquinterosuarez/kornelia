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
use crate::memory::{Caching, Kind};

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
    let mut cap = 0u64;
    for r in m.regions {
        cap = cap.max(r.end());
    }
    cap.div_ceil(GIB)
}

/// Cuantos rangos se pueden mapear a pedido, ya arrancada la maquina.
///
/// Ocho alcanza para los aparatos de una maquina. Si no alcanzara, se dice en
/// vez de mapear a medias: media ventana de registros es peor que ninguna,
/// porque el aparato contesta hasta la mitad y calla el resto.
const MAX_EXTRA: usize = 8;

/// Lo que se mapeo **despues** del arranque, porque el agente lo pidio.
///
/// El mapa que informa la maquina no cubre todo lo que hay: los BARs de PCIe
/// pueden caer mucho mas arriba de la region mas alta, y en aarch64 caen — el
/// controlador NVMe de la maquina de prueba aparece en 512 GiB y el mapa llega
/// a 257. Antes eso era un `unmapped` y el aparato quedaba inalcanzable, o sea
/// que el kernel era la razon por la que no se podia usar (P1).
static mut EXTRA: [Option<(u64, u64)>; MAX_EXTRA] = [None; MAX_EXTRA];

/// Anota un rango recien mapeado. `false` si ya no queda lugar.
pub fn note_mapped(start: u64, end: u64) -> bool {
    // SAFETY: se llama desde el nucleo del protocolo, que atiende de a uno.
    let extra = unsafe { &mut *core::ptr::addr_of_mut!(EXTRA) };
    for slot in extra.iter_mut() {
        match slot {
            // Ya estaba: mapear dos veces el mismo BAR no es un error, es el
            // agente reclamandolo, soltandolo y volviendolo a reclamar.
            Some((s, e)) if *s <= start && end <= *e => return true,
            None => {
                *slot = Some((start, end));
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Si el CPU puede tocar ese rango: porque la maquina lo informo en el mapa, o
/// porque se mapeo a pedido despues.
pub fn covers(m: &Machine, start: u64, end: u64) -> bool {
    if end <= span_gib(m).saturating_mul(GIB) {
        return true;
    }
    // SAFETY: solo lectura, y quien escribe es un solo nucleo.
    let extra = unsafe { &*core::ptr::addr_of!(EXTRA) };
    extra.iter().flatten().any(|(s, e)| *s <= start && end <= *e)
}

/// Olvida lo mapeado a pedido. Solo para los tests, que comparten estaticos.
#[cfg(test)]
pub fn forget_mapped() {
    unsafe { EXTRA = [None; MAX_EXTRA] };
}

/// Con que atributos hay que mapear la pagina numero `gib`.
///
/// Ante la duda, `Device`. Cachear RAM que en realidad era un registro rompe el
/// dispositivo de una forma dificil de diagnosticar; no cachear RAM de verdad
/// solo la hace lenta. El error caro y el barato no son simetricos.
pub fn attr_of(m: &Machine, gib: u64) -> Attr {
    let start_at = gib * GIB;
    let end = start_at + GIB;

    let mut has_memory = false;

    for r in m.regions {
        // Sin solapamiento con esta pagina, no dice nada de ella.
        if r.start >= end || r.end() <= start_at {
            continue;
        }
        // **Lo que la maquina dijo gana sobre lo que se puede deducir** (P4,
        // deuda 11). El entorno de arranque informa la cacheabilidad region por
        // region, y es mas precisa que inferirla de la clase: hay memoria
        // reservada que igual es RAM cacheable, y rangos que parecen RAM y no
        // se pueden cachear.
        //
        // Un solo pedazo no cacheable adentro alcanza para que la pagina entera
        // tenga que serlo, porque el grano del mapeo es 1 GiB. Por eso este
        // caso corta y el otro solo anota.
        match r.caching {
            Caching::Uncacheable => return Attr::Device,
            Caching::WriteBack => has_memory = true,
            Caching::Unknown => match r.kind {
                // Sin el dato, se deduce de la clase, que es lo que se hacia
                // antes de que los atributos se leyeran.
                Kind::Mmio => return Attr::Device,
                // Lo que la maquina no supo explicar no se asume RAM.
                Kind::Reserved | Kind::Broken | Kind::Other(_) | Kind::Unreported => {}
                _ => has_memory = true,
            },
        }
    }

    if has_memory {
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
    /// Si el hardware **hace cumplir** que la memoria marcada para el agente no
    /// sea ejecutable con privilegio (D27).
    ///
    /// En aarch64 siempre: viene en el modelo de permisos y no se apaga. En
    /// x86_64 depende de que el CPU tenga SMEP y de haberlo podido prender — el
    /// firmware puede dejarlo apagado, y QEMU lo hace. Sin esto el permiso se
    /// marca igual pero no separa nada, y una garantia que no se cumple es peor
    /// que no tenerla: por eso se informa en vez de suponerse.
    pub isolation: bool,
}

/// El grano fino: cuando un pedazo de 1 GiB tiene memoria del kernel adentro,
/// se parte en bloques de este tamano para poder distinguir.
///
/// Dos MiB y no cuatro KiB porque partir un GiB en paginas de 4 KiB serian
/// quinientas doce tablas. La contra es que memoria del agente que caiga en el
/// mismo bloque de 2 MiB que el kernel queda tambien fuera de su alcance — y eso
/// se informa en vez de que lo descubra chocandose.
pub const BLOCK: u64 = 2 << 20;

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
///
/// Se parte por dos motivos, y los dos son el mismo: hay que poder decir cosas
/// distintas de pedazos distintos.
///
/// - **Tiene memoria del kernel adentro**: esos bloques nunca son del agente.
/// - **Tiene memoria libre adentro**: de ahi salen los reclamos, y un reclamo
///   puede pedir ser alcanzable sin privilegio (D27). Si el pedazo entero fuera
///   un solo bloque de 1 GiB, marcarlo marcaria tambien todo lo que hay al lado.
pub fn needs_split(m: &Machine, gib: u64) -> bool {
    let (a, b) = (gib * GIB, (gib + 1) * GIB);
    m.regions
        .iter()
        .any(|r| matches!(r.kind, Kind::Kernel | Kind::Free) && r.start < b && r.end() > a)
}
