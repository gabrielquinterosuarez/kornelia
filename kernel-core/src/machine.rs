//! Lo que se sabe de la maquina.
//!
//! Es el resultado de la unica ventana que hay para preguntarle al firmware
//! (D25). Despues de que esa ventana se cierra, esto es todo lo que se sabe, y
//! el nucleo no sabe si vino de UEFI, de un device tree o de una ROM (D24).

use crate::memory::{Kind, Region};

#[derive(Clone, Copy)]
pub struct Machine {
    /// El mapa de memoria fisica. Vacio si el arranque no lo pudo obtener.
    pub regions: &'static [Region],
    /// Donde la maquina guarda su propia descripcion.
    pub tables: Tables,
    /// Si algo salio mal al describir la maquina, que fue. Un fallo de arranque
    /// es un dato, no una muerte (P5): el cordon umbilical sigue vivo y hay que
    /// poder contar que paso.
    pub failure: Option<&'static str>,
}

/// Donde la maquina dejo escrito lo que es.
///
/// El mapa de memoria dice que RAM hay, pero no cuantos nucleos, ni donde esta
/// el controlador de interrupciones, ni que hay colgado del bus. Eso vive en
/// estas tablas, y hay **dos dialectos** para lo mismo:
///
/// - **ACPI**: tablas que arma el firmware. Es lo que usan x86 y los servidores
///   ARM.
/// - **Device tree**: un arbol de nodos con propiedades, compilado a un blob.
///   Es lo de ARM y RISC-V embebido.
///
/// Una maquina puede ofrecer los dos, uno, o ninguno. Aca solo se anota **donde
/// estan**: interpretarlos es harina de otro costal.
#[derive(Clone, Copy, Default)]
pub struct Tables {
    /// Direccion fisica del RSDP, la raiz de las tablas de ACPI.
    pub acpi: Option<u64>,
    /// Direccion fisica del device tree aplanado (DTB).
    pub device_tree: Option<u64>,
    /// Direccion fisica del punto de entrada de SMBIOS: fabricante, modelo,
    /// numero de serie. Todavia no se interpreta.
    pub smbios: Option<u64>,
}

impl Machine {
    /// Maquina sobre la que no se pudo averiguar nada.
    pub const fn mute(reason: &'static str) -> Self {
        Self {
            regions: &[],
            tables: Tables { acpi: None, device_tree: None, smbios: None },
            failure: Some(reason),
        }
    }

    /// La region que contiene esa direccion fisica, si alguna la contiene.
    pub fn region_containing(&self, addr: u64) -> Option<&Region> {
        self.regions.iter().find(|r| addr >= r.start && addr < r.end())
    }

    /// Comprueba que un rango entero caiga en memoria que nunca se va a
    /// entregar.
    ///
    /// Existe para no dar por sentado donde quedo la pila (ver `stack`). Que el
    /// razonamiento cierre en el papel no alcanza: el que reparte las clases es
    /// el firmware, y hay firmwares raros.
    pub fn is_ours(&self, start: u64, len: u64) -> bool {
        let end = start.saturating_add(len);
        let mut addr = start;
        while addr < end {
            match self.region_containing(addr) {
                Some(r) if r.kind == Kind::Kernel => addr = r.end(),
                // Ni una region ajena, ni un hueco sin mapear.
                _ => return false,
            }
        }
        true
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
