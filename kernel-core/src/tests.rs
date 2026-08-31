//! Tests del nucleo portable, que corren en la maquina de desarrollo.
//!
//! `kernel-core` no toca hardware: traduce, clasifica y formatea. Todo eso son
//! funciones puras que se pueden probar sin arrancar nada, y conviene, porque
//! el ciclo "compilar, bootear QEMU, leer el serie" es lento y no falla fuerte
//! — un tamano mal redondeado se lee igual de bien que uno bien.

use crate::machine::{Machine, Tables};
use crate::memory::{Kind, Region};
use crate::platform::{Platform, Umbilical};
use crate::tables::{read_acpi, read_device_tree};

// ---------------------------------------------------------------------------
// Una plataforma de mentira, para poder mirar lo que sale por el cordon
// ---------------------------------------------------------------------------

struct Fake {
    out: String,
}

impl Fake {
    fn new() -> Self {
        Self { out: String::new() }
    }
}

impl Platform for Fake {
    const ARCH: &'static str = "prueba";

    fn uart_write_byte(&mut self, b: u8) {
        self.out.push(b as char);
    }

    fn uart_read_byte(&mut self) -> Option<u8> {
        None
    }

    fn park(&mut self) -> ! {
        panic!("los tests no estacionan el nucleo");
    }

    fn machine(&self) -> Machine {
        Machine::mute("plataforma de prueba")
    }
}

/// Devuelve lo que `Umbilical::size` emite para ese tamano.
fn formato(bytes: u64) -> String {
    let mut f = Fake::new();
    {
        let mut u = Umbilical::new(&mut f);
        u.size(bytes);
    }
    f.out.trim().to_string()
}

// ---------------------------------------------------------------------------
// El formateo de tamanos
// ---------------------------------------------------------------------------

#[test]
fn usa_la_unidad_mas_grande_que_sea_exacta() {
    assert_eq!(formato(4096), "4 KiB");
    assert_eq!(formato(1024 * 1024), "1 MiB");
    assert_eq!(formato(1024 * 1024 * 1024), "1 GiB");
    assert_eq!(formato(12 * 1024 * 1024 * 1024), "12 GiB");
}

/// El invariante que sostiene P4: el kernel no miente sobre la maquina.
///
/// Un "512 MiB" que en realidad eran 512 MiB menos una pagina seria una mentira
/// silenciosa, del peor tipo: se lee igual de bien que la verdad.
#[test]
fn nunca_redondea() {
    let casi_512_mib = 512 * 1024 * 1024 - 4096;
    let s = formato(casi_512_mib);
    assert!(!s.contains("MiB"), "redondeo a MiB: {s}");
    assert_eq!(s, "524284 KiB");

    let casi_1_gib = 1024 * 1024 * 1024 - 1024 * 1024;
    let s = formato(casi_1_gib);
    assert!(!s.contains("GiB"), "redondeo a GiB: {s}");
    assert_eq!(s, "1023 MiB");
}

#[test]
fn lo_que_no_es_multiplo_de_kib_sale_en_bytes() {
    assert_eq!(formato(1), "1 B");
    assert_eq!(formato(1500), "1500 B");
}

#[test]
fn el_cero_no_rompe_nada() {
    assert_eq!(formato(0), "0 KiB");
}

// ---------------------------------------------------------------------------
// El mapa de memoria
// ---------------------------------------------------------------------------

#[test]
fn solo_se_cuenta_como_libre_lo_que_es_libre() {
    static REGIONES: [Region; 4] = [
        Region { start: 0, bytes: 4096, kind: Kind::Free },
        Region { start: 4096, bytes: 8192, kind: Kind::Firmware },
        Region { start: 12288, bytes: 4096, kind: Kind::Free },
        Region { start: 16384, bytes: 1 << 30, kind: Kind::Mmio },
    ];
    let m = Machine { regions: &REGIONES, tables: Tables::default(), failure: None };

    // 8 KiB: las dos regiones libres. Ni el firmware ni el MMIO cuentan, por
    // mas que el MMIO sea 1 GiB de espacio direccionable.
    assert_eq!(m.free_bytes(), 8192);
}

#[test]
fn una_maquina_muda_no_tiene_nada() {
    let m = Machine::mute("se rompio algo");
    assert_eq!(m.free_bytes(), 0);
    assert!(m.regions.is_empty());
    assert_eq!(m.failure, Some("se rompio algo"));
}

#[test]
fn el_fin_de_una_region_no_desborda() {
    let r = Region { start: u64::MAX - 10, bytes: 1000, kind: Kind::Free };
    assert_eq!(r.end(), u64::MAX);
}

#[test]
fn un_tipo_desconocido_conserva_su_numero() {
    // P4: no se le inventa significado, se informa crudo.
    let k = Kind::Other(9999);
    assert_eq!(k.name(), "otra");
    match k {
        Kind::Other(n) => assert_eq!(n, 9999),
        _ => panic!("cambio de variante"),
    }
}

// ---------------------------------------------------------------------------
// Los encabezados de ACPI y del device tree
// ---------------------------------------------------------------------------

/// Arma un RSDP sintetico con el checksum bien puesto.
fn rsdp(revision: u8) -> [u8; 36] {
    let mut b = [0u8; 36];
    b[..8].copy_from_slice(b"RSD PTR ");
    b[15] = revision;
    b[16..20].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    b[24..32].copy_from_slice(&0x1234_5678_9abc_def0u64.to_le_bytes());

    // ACPI manda que los primeros 20 bytes sumen 0 modulo 256.
    let suma = b[..20].iter().fold(0u8, |a, x| a.wrapping_add(*x));
    b[8] = 0u8.wrapping_sub(suma);
    b
}

#[test]
fn acepta_un_rsdp_bien_formado() {
    let b = rsdp(2);
    let a = unsafe { read_acpi(b.as_ptr() as u64) }.expect("deberia aceptarlo");
    assert_eq!(a.revision, 2);
    assert_eq!(a.rsdt, 0xdead_beef);
    assert_eq!(a.xsdt, Some(0x1234_5678_9abc_def0));
}

#[test]
fn en_acpi_1_0_no_se_lee_el_xsdt() {
    // La revision 0 no tiene encabezado extendido: leerlo seria leer lo que
    // haya al lado y pasarlo por una direccion.
    let a = unsafe { read_acpi(rsdp(0).as_ptr() as u64) }.unwrap();
    assert_eq!(a.xsdt, None);
    assert_eq!(a.rsdt, 0xdead_beef);
}

#[test]
fn rechaza_una_firma_ajena() {
    let mut b = rsdp(2);
    b[0] = b'X';
    assert!(unsafe { read_acpi(b.as_ptr() as u64) }.is_none());
}

/// Esta es la razon de ser del checksum: un puntero que casualmente empiece
/// con la firma correcta pero no sea un RSDP.
#[test]
fn rechaza_un_checksum_que_no_cierra() {
    let mut b = rsdp(2);
    b[9] = b[9].wrapping_add(1);
    assert!(unsafe { read_acpi(b.as_ptr() as u64) }.is_none());
}

/// El device tree es big-endian en toda arquitectura, incluso en una
/// little-endian. Si se leyera con el orden nativo, el magico no daria nunca.
fn dtb() -> [u8; 28] {
    let mut b = [0u8; 28];
    b[0..4].copy_from_slice(&0xd00d_feedu32.to_be_bytes());
    b[4..8].copy_from_slice(&1024u32.to_be_bytes());
    b[20..24].copy_from_slice(&17u32.to_be_bytes());
    b
}

#[test]
fn acepta_un_device_tree_bien_formado() {
    let b = dtb();
    let d = unsafe { read_device_tree(b.as_ptr() as u64) }.expect("deberia aceptarlo");
    assert_eq!(d.bytes, 1024);
    assert_eq!(d.version, 17);
}

#[test]
fn rechaza_un_magico_que_no_es() {
    let mut b = dtb();
    b[3] = 0x00;
    assert!(unsafe { read_device_tree(b.as_ptr() as u64) }.is_none());
}

/// Un magico escrito en little-endian tiene que ser rechazado: es justo el bug
/// que aparece si alguien "arregla" el `from_be`.
#[test]
fn rechaza_el_magico_al_reves() {
    let mut b = dtb();
    b[0..4].copy_from_slice(&0xd00d_feedu32.to_le_bytes());
    assert!(unsafe { read_device_tree(b.as_ptr() as u64) }.is_none());
}
