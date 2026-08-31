//! Tests del nucleo portable, que corren en la maquina de desarrollo.
//!
//! `kernel-core` no toca hardware: traduce, clasifica y formatea. Todo eso son
//! funciones puras que se pueden probar sin arrancar nada, y conviene, porque
//! el ciclo "compilar, bootear QEMU, leer el serie" es lento y no falla fuerte
//! — un tamano mal redondeado se lee igual de bien que uno bien.

use crate::cbor::{scan, Reader, Scan, Writer};
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

// ---------------------------------------------------------------------------
// CBOR
// ---------------------------------------------------------------------------

/// Codifica con el Writer y devuelve los bytes.
fn enc(f: impl FnOnce(&mut Writer<'_>)) -> Vec<u8> {
    let mut buf = [0u8; 512];
    let n = {
        let mut w = Writer::new(&mut buf);
        f(&mut w);
        w.finish().expect("no entro").len()
    };
    buf[..n].to_vec()
}

/// Los ejemplos canonicos del RFC 8949. Si estos dan, la codificacion es CBOR
/// de verdad y no un formato binario propio que se le parece.
#[test]
fn los_enteros_se_codifican_como_manda_el_rfc() {
    assert_eq!(enc(|w| w.uint(0)), [0x00]);
    assert_eq!(enc(|w| w.uint(23)), [0x17]);
    // 24 ya no entra en los 5 bits del encabezado: pasa a un byte aparte.
    assert_eq!(enc(|w| w.uint(24)), [0x18, 0x18]);
    assert_eq!(enc(|w| w.uint(255)), [0x18, 0xff]);
    assert_eq!(enc(|w| w.uint(256)), [0x19, 0x01, 0x00]);
    assert_eq!(enc(|w| w.uint(65535)), [0x19, 0xff, 0xff]);
    assert_eq!(enc(|w| w.uint(65536)), [0x1a, 0x00, 0x01, 0x00, 0x00]);
    assert_eq!(
        enc(|w| w.uint(u64::MAX)),
        [0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
    );
}

#[test]
fn cadenas_arreglos_y_simples_como_manda_el_rfc() {
    assert_eq!(enc(|w| w.text("")), [0x60]);
    assert_eq!(enc(|w| w.text("a")), [0x61, 0x61]);
    assert_eq!(enc(|w| w.text("IETF")), [0x64, 0x49, 0x45, 0x54, 0x46]);
    assert_eq!(enc(|w| w.bytes(&[1, 2, 3, 4])), [0x44, 1, 2, 3, 4]);
    assert_eq!(enc(|w| w.bool(false)), [0xf4]);
    assert_eq!(enc(|w| w.bool(true)), [0xf5]);
    assert_eq!(enc(|w| w.null()), [0xf6]);
    assert_eq!(
        enc(|w| {
            w.array(3);
            w.uint(1);
            w.uint(2);
            w.uint(3);
        }),
        [0x83, 0x01, 0x02, 0x03]
    );
}

/// Lo que hace posible leer del UART de a un byte sin saber cuanto viene.
#[test]
fn un_mensaje_a_medias_se_reconoce_como_incompleto() {
    let entero = enc(|w| {
        w.array(3);
        w.uint(7);
        w.text("describe");
        w.map(0);
    });

    // Cada prefijo estricto tiene que decir "falta mas", nunca "listo".
    for corte in 0..entero.len() {
        assert_eq!(
            scan(&entero[..corte]),
            Scan::Incomplete,
            "el prefijo de {corte} bytes se dio por completo"
        );
    }
    assert_eq!(scan(&entero), Scan::Complete(entero.len()));
}

/// Si vienen dos pedidos pegados, el primero tiene que medirse solo.
#[test]
fn dos_mensajes_pegados_no_se_confunden() {
    let mut flujo = enc(|w| {
        w.array(3);
        w.uint(1);
        w.text("describe");
        w.map(0);
    });
    let primero = flujo.len();
    flujo.extend_from_slice(&enc(|w| w.uint(42)));

    assert_eq!(scan(&flujo), Scan::Complete(primero));
}

#[test]
fn lo_que_no_es_cbor_se_rechaza_sin_esperar_mas() {
    // 0x1c, 0x1d y 0x1e no existen en la especificacion.
    assert_eq!(scan(&[0x1c]), Scan::Malformed);
    // 0x1f es largo indefinido: valido en CBOR, no soportado acá a proposito.
    assert_eq!(scan(&[0x9f]), Scan::Malformed);
}

/// Un mensaje hostil no puede hacer que el kernel se quede sin pila.
#[test]
fn el_anidamiento_tiene_techo() {
    // 20 arreglos de un elemento, uno adentro del otro.
    let hondo = vec![0x81u8; 20];
    assert_eq!(scan(&hondo), Scan::Malformed);
}

#[test]
fn se_lee_lo_que_se_escribio() {
    let msg = enc(|w| {
        w.array(3);
        w.uint(9);
        w.text("describe");
        w.map(1);
        w.text("what");
        w.array(1);
        w.text("memory");
    });

    let mut r = Reader::new(&msg);
    assert_eq!(r.array(), Some(3));
    assert_eq!(r.uint(), Some(9));
    assert_eq!(r.text(), Some("describe"));
    assert_eq!(r.map(), Some(1));
    assert_eq!(r.text(), Some("what"));
    assert_eq!(r.array(), Some(1));
    assert_eq!(r.text(), Some("memory"));
}

/// Pedir el tipo equivocado no puede mover el cursor: si lo moviera, el resto
/// del mensaje se leeria corrido y el kernel contestaria cualquier cosa.
#[test]
fn pedir_el_tipo_equivocado_no_pierde_el_hilo() {
    let msg = enc(|w| w.text("hola"));
    let mut r = Reader::new(&msg);

    assert_eq!(r.uint(), None);
    assert_eq!(r.array(), None);
    assert_eq!(r.text(), Some("hola"));
}

/// Saltear una clave desconocida es lo que permite agregar argumentos nuevos
/// sin romper a un kernel viejo.
#[test]
fn se_puede_saltear_un_valor_cualquiera() {
    let msg = enc(|w| {
        w.array(2);
        w.map(1);
        w.text("adentro");
        w.array(2);
        w.uint(1);
        w.uint(2);
        w.text("despues");
    });

    let mut r = Reader::new(&msg);
    assert_eq!(r.array(), Some(2));
    assert_eq!(r.skip(), Some(())); // se saltea el mapa entero
    assert_eq!(r.text(), Some("despues"));
}

/// Que no entre en el buffer no puede ser un panico ni una escritura afuera.
#[test]
fn si_no_entra_se_avisa_en_vez_de_romper() {
    let mut chico = [0u8; 4];
    let mut w = Writer::new(&mut chico);
    w.text("esto es mucho mas largo que cuatro bytes");
    assert!(w.finish().is_none());
}

#[test]
fn el_texto_invalido_en_utf8_se_rechaza() {
    // Encabezado de texto de 2 bytes, seguido de una secuencia UTF-8 rota.
    let msg = [0x62u8, 0xff, 0xfe];
    let mut r = Reader::new(&msg);
    assert_eq!(r.text(), None);
}

// ---------------------------------------------------------------------------
// De quien es cada direccion
// ---------------------------------------------------------------------------

/// Dos regiones del kernel PEGADAS, una libre en el medio del mapa, y un hueco
/// sin mapear a partir de 0x5000.
static MAPA: [Region; 4] = [
    Region { start: 0x1000, bytes: 0x1000, kind: Kind::Kernel },
    Region { start: 0x2000, bytes: 0x1000, kind: Kind::Kernel },
    Region { start: 0x3000, bytes: 0x1000, kind: Kind::Free },
    Region { start: 0x4000, bytes: 0x1000, kind: Kind::Firmware },
];

fn maquina() -> Machine {
    Machine { regions: &MAPA, tables: Tables::default(), failure: None }
}

#[test]
fn se_encuentra_la_region_de_una_direccion() {
    let m = maquina();
    assert_eq!(m.region_containing(0x1000).map(|r| r.kind), Some(Kind::Kernel));
    assert_eq!(m.region_containing(0x1fff).map(|r| r.kind), Some(Kind::Kernel));
    assert_eq!(m.region_containing(0x3500).map(|r| r.kind), Some(Kind::Free));
    // El final de una region ya no le pertenece.
    assert!(m.region_containing(0x5000).is_none());
}

#[test]
fn lo_que_esta_entero_en_memoria_del_kernel_es_nuestro() {
    let m = maquina();
    assert!(m.is_ours(0x1000, 0x1000));
    // A caballo de dos regiones del kernel pegadas: sigue siendo nuestro.
    assert!(m.is_ours(0x1800, 0x1000));
    assert!(m.is_ours(0x1000, 0x2000));
}

/// Este es el caso que motiva todo: una pila que empieza en memoria nuestra
/// pero se pasa a memoria que `mem.claim` podria entregar.
#[test]
fn lo_que_se_pasa_a_memoria_reclamable_no_es_nuestro() {
    let m = maquina();
    assert!(!m.is_ours(0x2800, 0x1000), "se metio en la region libre");
    assert!(!m.is_ours(0x3000, 0x100), "arranca en memoria libre");
    assert!(!m.is_ours(0x4000, 0x100), "arranca en memoria del firmware");
}

#[test]
fn un_hueco_sin_mapear_tampoco_es_nuestro() {
    let m = maquina();
    assert!(!m.is_ours(0x9000, 0x100));
    // Empieza bien y se cae por un hueco.
    assert!(!m.is_ours(0x4f00, 0x1000));
}

#[test]
fn sin_mapa_no_hay_nada_nuestro() {
    // Una maquina muda no puede afirmar que algo sea suyo.
    assert!(!Machine::mute("sin datos").is_ours(0x1000, 0x10));
}
