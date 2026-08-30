//! El mapa de memoria, normalizado.
//!
//! Este es el vocabulario que hablan todos los entornos de arranque (D24): UEFI
//! hoy, device tree o ROM de arranque manana. El nucleo no sabe de cual vino.
//!
//! Por eso aca no hay ni una palabra de UEFI. Los tipos de memoria de UEFI son
//! quince numeros con nombres suyos; `Clase` es lo que de eso le importa a quien
//! va a reclamar memoria.

/// Una region contigua de memoria fisica, tal como la maquina se describe (P4).
#[derive(Clone, Copy)]
pub struct Region {
    /// Direccion fisica donde empieza.
    pub inicio: u64,
    /// Cuantos bytes ocupa. En bytes y no en paginas a proposito: el tamano de
    /// pagina es una convencion del entorno de arranque, no de la maquina.
    pub bytes: u64,
    /// Que es esta region para quien quiera reclamarla.
    pub clase: Clase,
}

impl Region {
    /// Region nula, para poder tener arreglos estaticos sin asignador.
    pub const fn vacia() -> Self {
        Self { inicio: 0, bytes: 0, clase: Clase::Reservada }
    }

    /// Primera direccion que ya NO pertenece a la region.
    pub fn fin(&self) -> u64 {
        self.inicio.saturating_add(self.bytes)
    }
}

/// Que se puede hacer con una region.
///
/// No es la taxonomia del firmware: es la que necesita `mem.claim`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Clase {
    /// RAM utilizable. Es lo unico que `mem.claim` puede entregar como memoria.
    Libre,
    /// El kernel y sus datos. Reclamarla es suicidarse.
    Kernel,
    /// Codigo o datos que el firmware sigue necesitando despues de que le
    /// soltamos la maquina. No se toca nunca.
    Firmware,
    /// Tablas que describen la maquina (ACPI). Se pueden leer y, una vez
    /// leidas, reciclar.
    TablasAcpi,
    /// Memoria del firmware que sobrevive a la suspension. No se toca.
    FirmwareNvs,
    /// Registros de dispositivo, no RAM. Se mapea no-cacheable (D12).
    Mmio,
    /// Reservada sin decir para que.
    Reservada,
    /// La maquina la reporta como fisicamente defectuosa.
    Rota,
    /// Persiste sin alimentacion, pero se direcciona como RAM.
    Persistente,
    /// Un tipo que este kernel todavia no conoce. Se informa el numero crudo en
    /// vez de inventarle un significado (P4).
    Otra(u32),
}

impl Clase {
    /// Nombre corto para el cordon umbilical. ASCII puro.
    pub fn nombre(&self) -> &'static str {
        match self {
            Clase::Libre => "libre",
            Clase::Kernel => "kernel",
            Clase::Firmware => "firmware",
            Clase::TablasAcpi => "acpi",
            Clase::FirmwareNvs => "acpi-nvs",
            Clase::Mmio => "mmio",
            Clase::Reservada => "reservada",
            Clase::Rota => "rota",
            Clase::Persistente => "persistente",
            Clase::Otra(_) => "otra",
        }
    }
}

/// Lo que el entorno de arranque logro averiguar de la maquina.
///
/// Es el resultado de la unica ventana que hay para preguntarle al firmware
/// (D25). Despues de eso, esto es todo lo que se sabe.
#[derive(Clone, Copy)]
pub struct Maquina {
    /// El mapa de memoria fisica. Vacio si el arranque no lo pudo obtener.
    pub regiones: &'static [Region],
    /// Si algo salio mal al describir la maquina, que fue. Un fallo de arranque
    /// es un dato, no una muerte (P5): el cordon umbilical sigue vivo y hay que
    /// poder contar que paso.
    pub fallo: Option<&'static str>,
}

impl Maquina {
    /// Maquina sobre la que no se pudo averiguar nada.
    pub const fn muda(motivo: &'static str) -> Self {
        Self { regiones: &[], fallo: Some(motivo) }
    }

    /// Total de bytes de RAM utilizable.
    pub fn bytes_libres(&self) -> u64 {
        let mut total = 0u64;
        for r in self.regiones {
            if r.clase == Clase::Libre {
                total = total.saturating_add(r.bytes);
            }
        }
        total
    }
}
