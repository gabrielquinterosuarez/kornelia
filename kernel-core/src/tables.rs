//! Los encabezados de ACPI y del device tree.
//!
//! Aca no se interpreta ninguna tabla todavia: solo se lee el encabezado para
//! **verificar que el puntero apunta a lo que el firmware dijo**, y para sacar
//! los pocos datos que hacen falta para decidir como seguir.
//!
//! Va en `kernel-core` y no en `boot-uefi` a proposito: ACPI y el device tree
//! son formatos de la maquina, no de UEFI. Quien los *encuentra* depende del
//! entorno de arranque; quien los *lee*, no (D24).
//!
//! # Cuidado con la memoria
//!
//! Estas funciones leen direcciones fisicas crudas. Hoy funciona porque despues
//! de `ExitBootServices` seguimos con las tablas de paginas que dejo el
//! firmware, que identity-mapean la memoria baja. Cuando el kernel arme las
//! suyas (D12) esto sigue valiendo, pero mientras tanto es una herencia y no
//! una garantia nuestra.

/// "RSD PTR " en little-endian.
const RSDP_SIGNATURE: u64 = 0x2052_5450_2044_5352;

/// El numero magico del device tree aplanado. El formato es big-endian en toda
/// arquitectura, asi que este valor se compara despues de dar vuelta los bytes.
const DTB_MAGIC: u32 = 0xd00d_feed;

/// Lo que dice el encabezado del RSDP de ACPI.
#[derive(Clone, Copy)]
pub struct Acpi {
    /// 0 es ACPI 1.0 (solo RSDT de 32 bits); 2 o mas trae XSDT de 64 bits.
    pub revision: u8,
    /// Raiz de 32 bits. Siempre presente.
    pub rsdt: u32,
    /// Raiz de 64 bits. Solo en revision 2 o mas.
    pub xsdt: Option<u64>,
}

/// Lo que dice el encabezado del device tree aplanado.
#[derive(Clone, Copy)]
pub struct DeviceTree {
    /// Tamano total del blob.
    pub bytes: u32,
    /// Version del formato.
    pub version: u32,
}

/// Lee y verifica el RSDP en `addr`.
///
/// Devuelve `None` si la firma no coincide o si el checksum no cierra: en ese
/// caso el puntero no apuntaba a un RSDP, y es mejor saberlo aca que al seguir
/// una raiz inventada.
///
/// # Safety
///
/// `addr` tiene que ser una direccion fisica legible de al menos 36 bytes.
pub unsafe fn read_acpi(addr: u64) -> Option<Acpi> {
    let p = addr as *const u8;

    if (p as *const u64).read_unaligned() != RSDP_SIGNATURE {
        return None;
    }

    // ACPI manda que los primeros 20 bytes sumen 0 modulo 256. Es la unica
    // defensa contra un puntero que casualmente empiece con la firma.
    let mut suma: u8 = 0;
    for i in 0..20 {
        suma = suma.wrapping_add(p.add(i).read());
    }
    if suma != 0 {
        return None;
    }

    let revision = p.add(15).read();
    let rsdt = (p.add(16) as *const u32).read_unaligned();

    // El XSDT y el resto del encabezado extendido solo existen de la revision 2
    // en adelante. Leerlos en una maquina ACPI 1.0 seria leer lo que haya al
    // lado.
    let xsdt = if revision >= 2 {
        Some((p.add(24) as *const u64).read_unaligned())
    } else {
        None
    };

    Some(Acpi { revision, rsdt, xsdt })
}

/// Lee y verifica el encabezado del device tree en `addr`.
///
/// # Safety
///
/// `addr` tiene que ser una direccion fisica legible de al menos 28 bytes.
pub unsafe fn read_device_tree(addr: u64) -> Option<DeviceTree> {
    let p = addr as *const u8;

    // Todo el formato es big-endian, incluso en una maquina little-endian.
    let magic = u32::from_be((p as *const u32).read_unaligned());
    if magic != DTB_MAGIC {
        return None;
    }

    let bytes = u32::from_be((p.add(4) as *const u32).read_unaligned());
    let version = u32::from_be((p.add(20) as *const u32).read_unaligned());

    Some(DeviceTree { bytes, version })
}

/// Lo que la maquina dice de si misma, venga en el dialecto que venga (P4).
///
/// Hay dos formatos y no se parecen: ACPI son tablas con firma y checksum, el
/// device tree es un arbol con nombres de texto. **Cual usar no es una eleccion
/// del kernel**: es cual dejo el firmware. Se prefiere ACPI cuando estan los
/// dos, porque es el que este kernel lee mas completo.
///
/// Vive aca, y no en cada lugar que lo necesita, porque esa decision se toma en
/// dos momentos muy separados —al armar el mapa de memoria y al describir el
/// hardware— y tenerla escrita dos veces es tenerla escrita mal una vez.
///
/// # Safety
///
/// Las direcciones tienen que ser las que dio el firmware, y su memoria estar
/// mapeada.
pub unsafe fn describe(t: &crate::machine::Tables) -> crate::acpi::Hardware {
    if let Some(addr) = t.acpi {
        if let Some(rsdp) = read_acpi(addr) {
            return crate::acpi::read(&rsdp);
        }
    }
    if let Some(blob) = t.device_tree {
        return crate::fdt::read(blob);
    }
    crate::acpi::Hardware::blank()
}
