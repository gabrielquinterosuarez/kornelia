//! El mapa de memoria, normalizado.
//!
//! Este es el vocabulario que hablan todos los entornos de arranque (D24): UEFI
//! hoy, device tree o ROM de arranque manana. El nucleo no sabe de cual vino.
//!
//! Por eso aca no hay ni una palabra de UEFI. Los tipos de memoria de UEFI son
//! quince numeros con nombres suyos; `Kind` es lo que de eso le importa a quien
//! va a reclamar memoria.

/// Si esta region se puede cachear, **segun la maquina** y no segun nosotros.
///
/// El entorno de arranque informa esto region por region, y es mas preciso que
/// deducirlo de la clase: hay memoria reservada que igual es RAM cacheable, y
/// hay rangos que parecen RAM y no se pueden cachear. Deducirlo de la clase
/// anda casi siempre, y "casi siempre" en cacheabilidad significa un
/// dispositivo que no se entera de una escritura.
///
/// Se normaliza en vez de guardar los bits crudos del firmware: `kernel-core`
/// no sabe como arranco la maquina (D24), y `EFI_MEMORY_WB` es una palabra de
/// UEFI.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub enum Caching {
    /// La maquina dice que se puede cachear.
    WriteBack,
    /// La maquina dice que **no**. Son registros, o memoria que se comparte con
    /// algo que no pasa por la cache.
    Uncacheable,
    /// La maquina no lo dijo. Se cae en deducirlo de la clase, que es lo que se
    /// hacia siempre.
    Unknown,
}

/// Una region contigua de memoria fisica, tal como la maquina se describe (P4).
#[derive(Clone, Copy)]
pub struct Region {
    /// Direccion fisica donde empieza.
    pub start: u64,
    /// Cuantos bytes ocupa. En bytes y no en paginas a proposito: el tamano de
    /// pagina es una convencion del entorno de arranque, no de la maquina.
    pub bytes: u64,
    /// Que es esta region para quien quiera reclamarla.
    pub kind: Kind,
    /// Si se puede cachear, segun lo que informo la maquina.
    pub caching: Caching,
}

impl Region {
    /// Region nula, para poder tener arreglos estaticos sin asignador.
    pub const fn empty() -> Self {
        Self { start: 0, bytes: 0, kind: Kind::Reserved, caching: Caching::Unknown }
    }

    /// Una region de la que solo se sabe donde esta y que es.
    ///
    /// Existe para los tests y para quien no tenga el dato de cacheabilidad: no
    /// decirlo es distinto de decir que no se puede cachear.
    pub const fn new(start: u64, bytes: u64, kind: Kind) -> Self {
        Self { start, bytes, kind, caching: Caching::Unknown }
    }

    /// Primera direccion que ya NO pertenece a la region.
    pub fn end(&self) -> u64 {
        self.start.saturating_add(self.bytes)
    }
}

/// Que se puede hacer con una region.
///
/// No es la taxonomia del firmware: es la que necesita `mem.claim`.
// Debug solo al testear: en el kernel de verdad no hay a quien mostrarselo, y
// el formateo derivado es codigo que se lleva puesto bytes al pedo.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub enum Kind {
    /// RAM utilizable. Es lo unico que `mem.claim` puede entregar como memoria.
    Free,
    /// El kernel y sus datos. Reclamarla es suicidarse.
    Kernel,
    /// Codigo o datos que el firmware sigue necesitando despues de que le
    /// soltamos la maquina. No se toca nunca.
    Firmware,
    /// Tablas que describen la maquina (ACPI). Se pueden leer y, una vez
    /// leidas, reciclar.
    AcpiTables,
    /// Memoria del firmware que sobrevive a la suspension. No se toca.
    FirmwareNvs,
    /// Registros de dispositivo, no RAM. Se mapea no-cacheable (D12).
    Mmio,
    /// Reservada sin decir para que.
    Reserved,
    /// La maquina la reporta como fisicamente defectuosa.
    Broken,
    /// Persiste sin alimentacion, pero se direcciona como RAM.
    Persistent,
    /// Un tipo que este kernel todavia no conoce. Se informa el numero crudo en
    /// vez de inventarle un significado (P4).
    Other(u32),
    /// **La maquina no dijo nada de este rango.** Es un hueco del mapa que el
    /// identity map igual alcanza, y ahi suelen vivir los BARs que asigno el
    /// firmware sin listarlos.
    ///
    /// Se entrega, porque el kernel no es quien decide que aparatos existen
    /// (P1) — y negarlo no protegia nada: el agente ya le escribia desde su
    /// codigo en `exec`. Pero se entrega **con este nombre** y no como `Mmio`:
    /// decir "esto son registros de un dispositivo" seria inventar lo que la
    /// maquina no dijo, y el agente tiene derecho a saber que lo que se lleva
    /// nadie se lo confirmo (P4).
    Unreported,
}

impl Kind {
    /// El identificador que viaja por el protocolo (D6), y tambien el que sale
    /// por el cable.
    ///
    /// Hubo un `name()` aparte, en espanol, para que un humano leyera el serie.
    /// Dejo de tener sentido cuando lo que el kernel dice paso a ser ingles
    /// (regla 6): decia lo mismo con otras palabras y no lo usaba nadie.
    pub fn code(&self) -> &'static str {
        match self {
            Kind::Free => "free",
            Kind::Kernel => "kernel",
            Kind::Firmware => "firmware",
            Kind::AcpiTables => "acpi",
            Kind::FirmwareNvs => "acpi-nvs",
            Kind::Mmio => "mmio",
            Kind::Reserved => "reserved",
            Kind::Broken => "broken",
            Kind::Persistent => "persistent",
            Kind::Other(_) => "other",
            Kind::Unreported => "unreported",
        }
    }
}
