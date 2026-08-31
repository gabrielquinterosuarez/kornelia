//! El mapa de memoria, normalizado.
//!
//! Este es el vocabulario que hablan todos los entornos de arranque (D24): UEFI
//! hoy, device tree o ROM de arranque manana. El nucleo no sabe de cual vino.
//!
//! Por eso aca no hay ni una palabra de UEFI. Los tipos de memoria de UEFI son
//! quince numeros con nombres suyos; `Kind` es lo que de eso le importa a quien
//! va a reclamar memoria.

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
}

impl Region {
    /// Region nula, para poder tener arreglos estaticos sin asignador.
    pub const fn empty() -> Self {
        Self { start: 0, bytes: 0, kind: Kind::Reserved }
    }

    /// Primera direccion que ya NO pertenece a la region.
    pub fn end(&self) -> u64 {
        self.start.saturating_add(self.bytes)
    }
}

/// Que se puede hacer con una region.
///
/// No es la taxonomia del firmware: es la que necesita `mem.claim`.
#[derive(Clone, Copy, PartialEq, Eq)]
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
}

impl Kind {
    /// El identificador que viaja por el protocolo (D6).
    ///
    /// Va aparte de `name()` a proposito: `name()` es para que un humano lea el
    /// serie y puede cambiar de redaccion cuando quiera; esto es interfaz, y si
    /// cambia se rompe el cliente del otro lado.
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
        }
    }

    /// Nombre corto para el cordon umbilical. ASCII puro.
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Free => "libre",
            Kind::Kernel => "kernel",
            Kind::Firmware => "firmware",
            Kind::AcpiTables => "acpi",
            Kind::FirmwareNvs => "acpi-nvs",
            Kind::Mmio => "mmio",
            Kind::Reserved => "reservada",
            Kind::Broken => "rota",
            Kind::Persistent => "persistente",
            Kind::Other(_) => "otra",
        }
    }
}
