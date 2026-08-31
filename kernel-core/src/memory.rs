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

/// Lo que el entorno de arranque logro averiguar de la maquina.
///
/// Es el resultado de la unica ventana que hay para preguntarle al firmware
/// (D25). Despues de eso, esto es todo lo que se sabe.
#[derive(Clone, Copy)]
pub struct Machine {
    /// El mapa de memoria fisica. Vacio si el arranque no lo pudo obtener.
    pub regions: &'static [Region],
    /// Si algo salio mal al describir la maquina, que fue. Un fallo de arranque
    /// es un dato, no una muerte (P5): el cordon umbilical sigue vivo y hay que
    /// poder contar que paso.
    pub failure: Option<&'static str>,
}

impl Machine {
    /// Maquina sobre la que no se pudo averiguar nada.
    pub const fn mute(reason: &'static str) -> Self {
        Self { regions: &[], failure: Some(reason) }
    }

    /// Total de bytes de RAM utilizable.
    pub fn free_bytes(&self) -> u64 {
        let mut total = 0u64;
        for r in self.regions {
            if r.kind == Kind::Free {
                total = total.saturating_add(r.bytes);
            }
        }
        total
    }
}
