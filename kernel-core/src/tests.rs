//! Tests del nucleo portable, que corren en la maquina de desarrollo.
//!
//! `kernel-core` no toca hardware: traduce, clasifica y formatea. Todo eso son
//! funciones puras que se pueden probar sin arrancar nada, y conviene, porque
//! el ciclo "compilar, bootear QEMU, leer el serie" es lento y no falla fuerte
//! — un tamano mal redondeado se lee igual de bien que uno bien.

use crate::cbor::{scan, Reader, Scan, Writer};
use crate::fault::{report, Cause, Fault};
use crate::machine::{Machine, Tables};
use crate::memory::{Kind, Region};
use crate::paging::{attr_of, span_gib, Attr, Mapping, GIB};
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

    const REGISTERS: &'static [&'static str] = &["r0", "r1"];

    unsafe fn install_fault_handlers(&mut self) -> Result<(), &'static str> {
        Err("la plataforma de prueba no tiene excepciones")
    }

    fn trigger_breakpoint(&mut self) {}

    fn last_fault(&self) -> Option<Fault> {
        None
    }

    unsafe fn exec(&mut self, _entry: u64, _region: (u64, u64)) -> crate::fault::Outcome {
        crate::fault::Outcome { faulted: false, regs: &[], fault: None }
    }

    unsafe fn install_serial_interrupt(
        &mut self,
        _hw: &crate::acpi::Hardware,
    ) -> Result<u8, &'static str> {
        Err("la plataforma de prueba no tiene timbre")
    }

    unsafe fn install_doorbell(
        &mut self,
        _hw: &crate::acpi::Hardware,
    ) -> Result<crate::channel::Doorbell, &'static str> {
        Err("la plataforma de prueba no tiene timbre")
    }

    unsafe fn install_irq(
        &mut self,
        _hw: &crate::acpi::Hardware,
        _interrupt: u32,
        _slot: usize,
        _raw: bool,
    ) -> Result<crate::channel::Doorbell, crate::handlers::Error> {
        Err(crate::handlers::Error::NoSuchInterrupt)
    }

    fn sleep(&mut self) {}

    fn uart_address(&self) -> Option<u64> {
        None
    }

    fn this_core(&self) -> u64 {
        0
    }

    unsafe fn start_core(
        &mut self,
        _hw: &crate::acpi::Hardware,
        _id: u64,
        _slot: usize,
    ) -> Result<(), crate::cores::Error> {
        Err(crate::cores::Error::NotSupported)
    }

    unsafe fn install_page_tables(&mut self, _m: &Machine) -> Result<Mapping, &'static str> {
        // Una plataforma de mentira no tiene MMU que configurar. Lo que si se
        // testea es el PLAN de mapeo, que es la parte portable.
        Err("la plataforma de prueba no pagina")
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

// ---------------------------------------------------------------------------
// El plan de mapeo (D12)
// ---------------------------------------------------------------------------

fn con(regiones: &'static [Region]) -> Machine {
    Machine { regions: regiones, tables: Tables::default(), failure: None }
}

#[test]
fn se_cubre_hasta_la_region_mas_alta_redondeando_para_arriba() {
    // Un solo byte pasado el GiB obliga a mapear el GiB siguiente entero.
    static UNO: [Region; 1] = [Region { start: 0, bytes: GIB + 1, kind: Kind::Free }];
    assert_eq!(span_gib(&con(&UNO)), 2);

    // Justo en el limite no hace falta uno mas.
    static JUSTO: [Region; 1] = [Region { start: 0, bytes: GIB, kind: Kind::Free }];
    assert_eq!(span_gib(&con(&JUSTO)), 1);

    // Se cubre hasta arriba de todo, no hasta donde llega la RAM: ahi viven los
    // BARs de PCIe.
    static ALTO: [Region; 2] = [
        Region { start: 0, bytes: GIB, kind: Kind::Free },
        Region { start: 100 * GIB, bytes: GIB, kind: Kind::Mmio },
    ];
    assert_eq!(span_gib(&con(&ALTO)), 101);
}

#[test]
fn una_pagina_con_ram_es_cacheable() {
    static RAM: [Region; 1] = [Region { start: 0, bytes: GIB, kind: Kind::Free }];
    assert_eq!(attr_of(&con(&RAM), 0), Attr::Memory);
}

/// El error caro y el barato no son simetricos: cachear un registro rompe el
/// dispositivo, no cachear RAM solo la hace lenta.
#[test]
fn un_solo_registro_vuelve_toda_la_pagina_no_cacheable() {
    static MIXTA: [Region; 2] = [
        Region { start: 0, bytes: GIB - 4096, kind: Kind::Free },
        // Una sola pagina de MMIO al final del GiB.
        Region { start: GIB - 4096, bytes: 4096, kind: Kind::Mmio },
    ];
    assert_eq!(attr_of(&con(&MIXTA), 0), Attr::Device);
}

#[test]
fn un_hueco_sin_nada_se_trata_como_dispositivo() {
    static LEJOS: [Region; 1] = [Region { start: 0, bytes: GIB, kind: Kind::Free }];
    // La pagina 5 no la menciona nadie: puede haber un dispositivo que este
    // kernel todavia no sabe que existe.
    assert_eq!(attr_of(&con(&LEJOS), 5), Attr::Device);
}

#[test]
fn lo_que_la_maquina_no_supo_explicar_no_se_asume_ram() {
    static RARAS: [Region; 3] = [
        Region { start: 0, bytes: 4096, kind: Kind::Reserved },
        Region { start: 4096, bytes: 4096, kind: Kind::Broken },
        Region { start: 8192, bytes: 4096, kind: Kind::Other(77) },
    ];
    assert_eq!(attr_of(&con(&RARAS), 0), Attr::Device);
}

#[test]
fn el_firmware_y_las_tablas_de_acpi_son_memoria() {
    static FW: [Region; 2] = [
        Region { start: 0, bytes: 4096, kind: Kind::Firmware },
        Region { start: 4096, bytes: 4096, kind: Kind::AcpiTables },
    ];
    assert_eq!(attr_of(&con(&FW), 0), Attr::Memory);
}

/// Una region que arranca en una pagina y termina en la siguiente tiene que
/// contar para las dos.
#[test]
fn una_region_a_caballo_afecta_a_las_dos_paginas() {
    static CABALLO: [Region; 1] = [Region {
        start: GIB - 4096,
        bytes: 8192,
        kind: Kind::Mmio,
    }];
    let m = con(&CABALLO);
    assert_eq!(attr_of(&m, 0), Attr::Device);
    assert_eq!(attr_of(&m, 1), Attr::Device);
}

// ---------------------------------------------------------------------------
// Faults (P5, D7)
// ---------------------------------------------------------------------------

/// Un breakpoint es un alto pedido: se sigue en la instruccion de al lado.
/// Cualquier otra cosa volveria a fallar en la misma instruccion, para siempre.
#[test]
fn solo_el_breakpoint_se_puede_retomar() {
    assert!(Cause::Breakpoint.resumable());
    for c in [
        Cause::PageFault,
        Cause::InvalidOpcode,
        Cause::DivideByZero,
        Cause::Protection,
        Cause::Alignment,
        Cause::Double,
        Cause::InstructionFetch,
        Cause::Unknown,
    ] {
        assert!(!c.resumable(), "{} no deberia poder retomarse", c.code());
    }
}

#[test]
fn cada_causa_tiene_su_codigo_y_no_se_repiten() {
    let todas = [
        Cause::Breakpoint,
        Cause::DivideByZero,
        Cause::InvalidOpcode,
        Cause::PageFault,
        Cause::InstructionFetch,
        Cause::Alignment,
        Cause::Protection,
        Cause::Double,
        Cause::Unknown,
    ];
    let mut vistos = std::collections::HashSet::new();
    for c in todas {
        assert!(!c.code().is_empty());
        assert!(vistos.insert(c.code()), "codigo repetido: {}", c.code());
    }
}

fn texto_del_fault(f: &Fault, nombres: &[&str]) -> String {
    let mut s = String::new();
    report(f, nombres, &mut s);
    s
}

#[test]
fn el_reporte_dice_causa_pc_y_registros() {
    static VALORES: [u64; 2] = [0xdead, 0xbeef];
    let f = Fault {
        cause: Cause::PageFault,
        raw: 14,
        detail: 0x2,
        pc: 0x1234,
        address: Some(0xcafe),
        regs: &VALORES,
    };
    let t = texto_del_fault(&f, &["uno", "dos"]);

    assert!(t.contains("page-fault"), "{t}");
    // El numero crudo viaja aunque la causa ya este traducida (P4).
    assert!(t.contains("crudo 14"), "{t}");
    assert!(t.contains("0x0000000000001234"), "{t}");
    assert!(t.contains("0x000000000000cafe"), "{t}");
    assert!(t.contains("uno=000000000000dead"), "{t}");
    assert!(t.contains("dos=000000000000beef"), "{t}");
}

/// Sin dirección tocada no se inventa una: un `int3` no tiene ninguna, y poner
/// un cero se leería como "toco la direccion cero".
#[test]
fn sin_direccion_no_se_informa_ninguna() {
    let f = Fault { cause: Cause::Breakpoint, raw: 3, detail: 0, pc: 0x99, address: None, regs: &[] };
    let t = texto_del_fault(&f, &[]);
    assert!(t.contains("breakpoint"), "{t}");
    assert!(!t.contains("direccion tocada"), "{t}");
}

/// El reporte lo arma quien tiene los nombres, y este modulo no conoce ninguno
/// (D3). Si vinieran de más o de menos, no puede quedar leyendo fuera de rango.
#[test]
fn nombres_y_valores_descoordinados_no_desbordan() {
    static TRES: [u64; 3] = [1, 2, 3];
    let f = Fault { cause: Cause::Unknown, raw: 0, detail: 0, pc: 0, address: None, regs: &TRES };

    // Más nombres que valores.
    let t = texto_del_fault(&f, &["a", "b", "c", "d", "e"]);
    assert!(t.contains("c=") && !t.contains("d="), "{t}");

    // Y menos.
    let t = texto_del_fault(&f, &["a"]);
    assert!(t.contains("a=") && !t.contains("b="), "{t}");
}

// ---------------------------------------------------------------------------
// Reclamos (D14)
// ---------------------------------------------------------------------------

use crate::claims::{self, Error, Request};

/// La tabla de reclamos es un estatico compartido, y los tests corren en
/// paralelo. Sin serializarlos se pisarian entre ellos y fallarian por turnos.
static CANDADO: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn con_tabla_limpia<T>(f: impl FnOnce() -> T) -> T {
    let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
    claims::reset();
    f()
}

/// 0x0000-0x1000 kernel, 0x1000-0x5000 libre, 0x5000-0x6000 mmio.
static MAQ: [Region; 3] = [
    Region { start: 0x0000, bytes: 0x1000, kind: Kind::Kernel },
    Region { start: 0x1000, bytes: 0x4000, kind: Kind::Free },
    Region { start: 0x5000, bytes: 0x1000, kind: Kind::Mmio },
];

fn maq() -> Machine {
    Machine { regions: &MAQ, tables: Tables::default(), failure: None }
}

fn pedir(bytes: u64) -> Request {
    Request { bytes, ..Default::default() }
}

#[test]
fn solo_se_reparte_memoria_libre() {
    con_tabla_limpia(|| {
        let c = claims::claim(&maq(), pedir(0x100)).unwrap();
        assert_eq!(c.kind, Kind::Free);
        // No salio de la region del kernel, que empieza en 0.
        assert!(c.start >= 0x1000, "{:#x}", c.start);
        assert!(c.end() <= 0x5000);
    });
}

#[test]
fn dos_reclamos_no_se_pisan() {
    con_tabla_limpia(|| {
        let m = maq();
        let a = claims::claim(&m, pedir(0x1000)).unwrap();
        let b = claims::claim(&m, pedir(0x1000)).unwrap();
        assert!(
            a.end() <= b.start || b.end() <= a.start,
            "se pisan: {:#x}..{:#x} y {:#x}..{:#x}",
            a.start, a.end(), b.start, b.end()
        );
    });
}

#[test]
fn se_respeta_la_alineacion() {
    con_tabla_limpia(|| {
        let m = maq();
        // Primero algo chico para desalinear el hueco siguiente.
        claims::claim(&m, pedir(1)).unwrap();
        let c = claims::claim(&m, Request { bytes: 0x100, align: 0x1000, ..Default::default() })
            .unwrap();
        assert_eq!(c.start % 0x1000, 0, "{:#x}", c.start);
    });
}

#[test]
fn una_alineacion_que_no_es_potencia_de_dos_se_rechaza() {
    con_tabla_limpia(|| {
        let r = Request { bytes: 16, align: 3, ..Default::default() };
        assert_eq!(claims::claim(&maq(), r), Err(Error::BadAlign));
    });
}

/// Existe porque hay dispositivos que solo hacen DMA por debajo de los 4 GiB.
#[test]
fn se_respeta_el_tope() {
    con_tabla_limpia(|| {
        let r = Request { bytes: 0x100, below: Some(0x2000), ..Default::default() };
        let c = claims::claim(&maq(), r).unwrap();
        assert!(c.end() <= 0x2000, "{:#x}", c.end());

        // Y si no entra debajo del tope, se dice que no hay lugar.
        let r = Request { bytes: 0x100, below: Some(0x1000), ..Default::default() };
        assert_eq!(claims::claim(&maq(), r), Err(Error::NoRoom));
    });
}

#[test]
fn no_se_entrega_lo_que_no_entra() {
    con_tabla_limpia(|| {
        assert_eq!(claims::claim(&maq(), pedir(0x100000)), Err(Error::NoRoom));
        assert_eq!(claims::claim(&maq(), pedir(0)), Err(Error::Empty));
    });
}

/// El MMIO se reclama por direccion exacta: el BAR de un dispositivo esta donde
/// esta, no donde haya lugar.
#[test]
fn el_mmio_se_reclama_por_direccion_exacta() {
    con_tabla_limpia(|| {
        let r = Request { bytes: 0x1000, at: Some(0x5000), ..Default::default() };
        let c = claims::claim(&maq(), r).unwrap();
        assert_eq!(c.start, 0x5000);
        assert_eq!(c.kind, Kind::Mmio);
    });
}

/// Entregar la memoria donde vive la pila del kernel no seria libertad, seria
/// incoherencia: el kernel es lo que esta prestando el servicio.
#[test]
fn la_memoria_del_kernel_no_se_entrega() {
    con_tabla_limpia(|| {
        let r = Request { bytes: 0x100, at: Some(0x0), ..Default::default() };
        assert_eq!(claims::claim(&maq(), r), Err(Error::IsKernel));
    });
}

#[test]
fn no_se_entrega_lo_que_la_maquina_no_informo() {
    con_tabla_limpia(|| {
        let r = Request { bytes: 0x100, at: Some(0x99000), ..Default::default() };
        assert_eq!(claims::claim(&maq(), r), Err(Error::Unmapped));

        // Ni un rango que empieza bien y se sale de la region.
        let r = Request { bytes: 0x2000, at: Some(0x5000), ..Default::default() };
        assert_eq!(claims::claim(&maq(), r), Err(Error::Unmapped));
    });
}

#[test]
fn lo_ya_reclamado_no_se_reclama_de_nuevo() {
    con_tabla_limpia(|| {
        let m = maq();
        let c = claims::claim(&m, pedir(0x100)).unwrap();
        let r = Request { bytes: 0x10, at: Some(c.start), ..Default::default() };
        assert_eq!(claims::claim(&m, r), Err(Error::Taken));
    });
}

/// Un handle liberado no se reusa: si se reusara, un pedido que llega tarde con
/// un handle viejo tocaria lo que otro reclamo despues en el mismo lugar.
#[test]
fn los_handles_no_se_reusan() {
    con_tabla_limpia(|| {
        let m = maq();
        let a = claims::claim(&m, pedir(0x100)).unwrap();
        assert!(claims::release(a.handle));
        let b = claims::claim(&m, pedir(0x100)).unwrap();
        assert_ne!(a.handle, b.handle);
        // Y el viejo ya no existe.
        assert!(claims::get(a.handle).is_none());
        assert!(!claims::release(a.handle));
    });
}

#[test]
fn no_se_puede_leer_fuera_del_reclamo() {
    con_tabla_limpia(|| {
        let c = claims::claim(&maq(), pedir(0x100)).unwrap();
        assert_eq!(claims::range_of(c.handle, 0, 0x100), Ok(c.start));
        assert_eq!(claims::range_of(c.handle, 0xFF, 2), Err(Error::OutOfBounds));
        assert_eq!(claims::range_of(c.handle, 0, 0x101), Err(Error::OutOfBounds));
        // Ni con un desplazamiento que desborda al sumar.
        assert_eq!(claims::range_of(c.handle, u64::MAX, 2), Err(Error::OutOfBounds));
        assert_eq!(claims::range_of(9999, 0, 1), Err(Error::NoSuchHandle));
    });
}

/// D14: el estado de la maquina no es una sesion. Lo reclamado se puede listar,
/// que es como el agente lo recupera al reconectar.
#[test]
fn lo_reclamado_se_puede_listar() {
    con_tabla_limpia(|| {
        let m = maq();
        let a = claims::claim(&m, pedir(0x100)).unwrap();
        let b = claims::claim(&m, pedir(0x100)).unwrap();
        assert_eq!(claims::count(), 2);

        let handles: Vec<u64> = claims::all().map(|c| c.handle).collect();
        assert!(handles.contains(&a.handle) && handles.contains(&b.handle));

        claims::release(a.handle);
        assert_eq!(claims::count(), 1);
    });
}

// ---------------------------------------------------------------------------
// Las tablas de ACPI
// ---------------------------------------------------------------------------

use crate::acpi;
use crate::tables::Acpi as Rsdp;

/// Arma una tabla de ACPI con la firma pedida y el checksum bien puesto.
fn tabla_acpi(firma: &[u8; 4], cuerpo: &[u8]) -> Vec<u8> {
    let mut t = vec![0u8; 36];
    t[..4].copy_from_slice(firma);
    t.extend_from_slice(cuerpo);
    let largo = t.len() as u32;
    t[4..8].copy_from_slice(&largo.to_le_bytes());

    // ACPI manda que la tabla entera sume 0 modulo 256.
    let suma = t.iter().fold(0u8, |a, x| a.wrapping_add(*x));
    t[9] = 0u8.wrapping_sub(suma);
    t
}

/// Una entrada de la MADT: tipo, largo, y payload.
fn entrada(tipo: u8, payload: &[u8]) -> Vec<u8> {
    let mut e = vec![tipo, (payload.len() + 2) as u8];
    e.extend_from_slice(payload);
    e
}

/// Arma un XSDT que apunta a las tablas dadas, y devuelve todo junto para que
/// no se muevan de lugar mientras se lee.
fn maquina_acpi(tablas: &[Vec<u8>]) -> (Vec<u8>, Vec<Box<[u8]>>) {
    // Las tablas van al heap y se quedan quietas ahi; el XSDT guarda punteros.
    let fijas: Vec<Box<[u8]>> = tablas.iter().map(|t| t.clone().into_boxed_slice()).collect();
    let mut punteros = Vec::new();
    for t in &fijas {
        punteros.extend_from_slice(&(t.as_ptr() as u64).to_le_bytes());
    }
    (tabla_acpi(b"XSDT", &punteros), fijas)
}

fn leer(xsdt: &[u8]) -> acpi::Hardware {
    let rsdp = Rsdp { revision: 2, rsdt: 0, xsdt: Some(xsdt.as_ptr() as u64) };
    unsafe { acpi::read(&rsdp) }
}

#[test]
fn se_leen_los_nucleos_de_x86() {
    // Tres APICs locales: dos habilitados y uno que no.
    let mut cuerpo = 0xFEE0_0000u32.to_le_bytes().to_vec(); // direccion del APIC
    cuerpo.extend_from_slice(&0u32.to_le_bytes()); // banderas
    cuerpo.extend(entrada(0, &[0, 0, 1, 0, 0, 0])); // uid 0, apic 0, habilitado
    cuerpo.extend(entrada(0, &[1, 7, 1, 0, 0, 0])); // uid 1, apic 7, habilitado
    cuerpo.extend(entrada(0, &[2, 9, 0, 0, 0, 0])); // uid 2, apic 9, NO

    let madt = tabla_acpi(b"APIC", &cuerpo);
    let (xsdt, _fijas) = maquina_acpi(&[madt]);
    let hw = leer(&xsdt);

    assert_eq!(hw.cpus.len(), 3, "se perdio algun nucleo");
    assert_eq!(hw.usable_cpus(), 2, "un nucleo deshabilitado no es usable");
    assert_eq!(hw.cpus[1].id, 7);
    assert_eq!(hw.cpus[1].uid, 1);
    assert!(!hw.cpus[2].enabled);

    let i = hw.interrupts.expect("no encontro el controlador");
    assert_eq!(i.kind, "apic");
    assert_eq!(i.address, 0xFEE0_0000);
}

/// El mismo formato, contenido distinto: un ARM describe GICs donde un x86
/// describe APICs, y los dos salen normalizados al mismo vocabulario (D24).
#[test]
fn se_leen_los_nucleos_de_arm() {
    let mut cuerpo = vec![0u8; 8];
    // GICC: el identificador que sirve para arrancarlo es el MPIDR, en el
    // offset 68 de la entrada.
    let mut gicc = vec![0u8; 74];
    gicc[6..10].copy_from_slice(&5u32.to_le_bytes()); // uid, en el offset 8
    gicc[10..14].copy_from_slice(&1u32.to_le_bytes()); // banderas: habilitado
    gicc[66..74].copy_from_slice(&0x8000_0003u64.to_le_bytes()); // mpidr
    cuerpo.extend(entrada(11, &gicc));

    // GICD: el distribuidor, version 3.
    let mut gicd = vec![0u8; 22];
    gicd[6..14].copy_from_slice(&0x0800_0000u64.to_le_bytes()); // direccion
    gicd[18] = 3; // version
    cuerpo.extend(entrada(12, &gicd));

    let (xsdt, _fijas) = maquina_acpi(&[tabla_acpi(b"APIC", &cuerpo)]);
    let hw = leer(&xsdt);

    assert_eq!(hw.cpus.len(), 1);
    assert_eq!(hw.cpus[0].id, 0x8000_0003, "el id tiene que ser el MPIDR");
    assert_eq!(hw.cpus[0].uid, 5);
    assert!(hw.cpus[0].enabled);

    let i = hw.interrupts.expect("no encontro el GIC");
    assert_eq!(i.kind, "gic");
    assert_eq!(i.address, 0x0800_0000);
    assert_eq!(i.version, 3);
}

#[test]
fn se_lee_donde_esta_pcie() {
    let mut cuerpo = vec![0u8; 8]; // reservado
    cuerpo.extend_from_slice(&0xE000_0000u64.to_le_bytes()); // base
    cuerpo.extend_from_slice(&0u16.to_le_bytes()); // segmento
    cuerpo.push(0); // primer bus
    cuerpo.push(255); // ultimo bus
    cuerpo.extend_from_slice(&0u32.to_le_bytes());

    let (xsdt, _fijas) = maquina_acpi(&[tabla_acpi(b"MCFG", &cuerpo)]);
    let x = leer(&xsdt).pcie.expect("no encontro PCIe");
    assert_eq!(x.base, 0xE000_0000);
    assert_eq!(x.bus_end, 255);
}

/// Informar que existe una tabla que este kernel todavia no lee es mas util que
/// callarla (P4).
#[test]
fn se_informan_las_tablas_que_no_se_interpretan() {
    let (xsdt, _fijas) = maquina_acpi(&[
        tabla_acpi(b"FACP", &[0u8; 4]),
        tabla_acpi(b"DSDT", &[0u8; 4]),
    ]);
    let hw = leer(&xsdt);
    assert_eq!(hw.signatures.len(), 2);
    assert_eq!(&hw.signatures[0], b"FACP");
    assert_eq!(&hw.signatures[1], b"DSDT");
    assert!(hw.cpus.is_empty());
}

/// Es la razon de ser del checksum: recorriendo memoria cruda, dar con cuatro
/// bytes que parecen una firma es mas facil de lo que parece.
#[test]
fn una_tabla_con_el_checksum_roto_se_ignora() {
    let mut madt = tabla_acpi(b"APIC", &[0u8; 8]);
    madt[9] = madt[9].wrapping_add(1);
    let (xsdt, _fijas) = maquina_acpi(&[madt]);
    let hw = leer(&xsdt);
    assert!(hw.signatures.is_empty(), "acepto una tabla que no cerraba");
}

/// Una entrada de largo cero haria girar el recorrido para siempre.
#[test]
fn una_entrada_de_largo_imposible_no_cuelga() {
    let mut cuerpo = vec![0u8; 8];
    cuerpo.push(0); // tipo
    cuerpo.push(0); // largo CERO
    cuerpo.extend_from_slice(&[0u8; 8]);

    let (xsdt, _fijas) = maquina_acpi(&[tabla_acpi(b"APIC", &cuerpo)]);
    // Que vuelva ya es la prueba.
    let hw = leer(&xsdt);
    assert!(hw.cpus.is_empty());
}

#[test]
fn sin_raiz_no_se_inventa_nada() {
    let rsdp = Rsdp { revision: 2, rsdt: 0, xsdt: Some(0) };
    let hw = unsafe { acpi::read(&rsdp) };
    assert!(hw.cpus.is_empty() && hw.signatures.is_empty());
    assert!(hw.interrupts.is_none() && hw.pcie.is_none());
}

// ---------------------------------------------------------------------------
// Nucleos (D13)
// ---------------------------------------------------------------------------

use crate::cores;

fn con_nucleos_limpios<T>(f: impl FnOnce() -> T) -> T {
    let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
    cores::reset();
    f()
}

#[test]
fn un_nucleo_no_esta_vivo_hasta_que_avisa() {
    con_nucleos_limpios(|| {
        let (slot, _) = cores::reserve(7).unwrap();
        // Reservar no es arrancar: son dos CPUs y una no puede afirmar por la
        // otra.
        assert!(!cores::has_arrived(slot));
        cores::arrived(slot, 7);
        assert!(cores::has_arrived(slot));
    });
}

/// El identificador cero es legitimo, asi que "llego" no se puede representar
/// guardando el identificador a secas.
#[test]
fn el_nucleo_cero_tambien_puede_avisar() {
    con_nucleos_limpios(|| {
        let (slot, _) = cores::reserve(0).unwrap();
        assert!(!cores::has_arrived(slot));
        cores::arrived(slot, 0);
        assert!(cores::has_arrived(slot), "el nucleo 0 no pudo avisar");
    });
}

#[test]
fn cada_nucleo_tiene_su_ranura_y_su_handle() {
    con_nucleos_limpios(|| {
        let (s1, h1) = cores::reserve(1).unwrap();
        let (s2, h2) = cores::reserve(2).unwrap();
        assert_ne!(s1, s2);
        assert_ne!(h1, h2);

        cores::arrived(s2, 2);
        assert!(cores::has_arrived(s2));
        assert!(!cores::has_arrived(s1), "aviso por la ranura equivocada");
    });
}

#[test]
fn lo_reclamado_se_puede_listar_y_no_se_repite() {
    con_nucleos_limpios(|| {
        cores::reserve(3).unwrap();
        assert!(cores::is_claimed(3));
        assert!(!cores::is_claimed(4));
        assert_eq!(cores::count(), 1);
    });
}

#[test]
fn la_tabla_de_nucleos_tiene_techo() {
    con_nucleos_limpios(|| {
        for i in 0..cores::MAX {
            cores::reserve(i as u64).unwrap();
        }
        assert_eq!(cores::reserve(999), Err(cores::Error::TableFull));
    });
}

#[test]
fn el_estado_de_un_nucleo_se_puede_corregir() {
    con_nucleos_limpios(|| {
        let (slot, _) = cores::reserve(5).unwrap();
        assert_eq!(cores::all().next().unwrap().state, cores::State::Starting);
        cores::settle(slot, cores::State::Failed);
        assert_eq!(cores::all().next().unwrap().state, cores::State::Failed);
    });
}

/// El controlador que recibe las interrupciones de los aparatos, y las
/// interrupciones viejas de PC que esta maquina movio de numero.
#[test]
fn se_lee_el_ioapic_y_los_numeros_movidos() {
    let mut cuerpo = vec![0u8; 8];
    // Tipo 1: el IO-APIC. Direccion en el offset 4 de la entrada, primer
    // numero global en el 8.
    let mut ioapic = vec![0u8; 10];
    ioapic[2..6].copy_from_slice(&0xFEC0_0000u32.to_le_bytes());
    ioapic[6..10].copy_from_slice(&0u32.to_le_bytes());
    cuerpo.extend(entrada(1, &ioapic));

    // Tipo 2: la interrupcion 4 (el serie de la PC) esta en la 20.
    let mut over = vec![0u8; 8];
    over[1] = 4; // source, en el offset 3 de la entrada
    over[2..6].copy_from_slice(&20u32.to_le_bytes());
    cuerpo.extend(entrada(2, &over));

    let (xsdt, _fijas) = maquina_acpi(&[tabla_acpi(b"APIC", &cuerpo)]);
    let hw = leer(&xsdt);

    let io = hw.ioapic.expect("no encontro el IO-APIC");
    assert_eq!(io.address, 0xFEC0_0000);
    assert_eq!(io.gsi_base, 0);

    // La 4 se movio a la 20; las que no estan en la tabla no cambiaron.
    assert_eq!(hw.gsi_of(4), 20);
    assert_eq!(hw.gsi_of(1), 1, "una interrupcion sin override no cambia");
}

/// La maquina diciendo donde tiene su consola, en vez de que la supongamos.
#[test]
fn se_lee_donde_esta_el_puerto_serie() {
    let mut cuerpo = vec![0u8; 22]; // hasta el offset 58 de la tabla
    // La direccion vive adentro de una estructura generica que arranca en el
    // offset 40 de la tabla; la direccion misma en el 44, o sea el 8 del cuerpo.
    cuerpo[8..16].copy_from_slice(&0x0900_0000u64.to_le_bytes());
    // El numero de interrupcion, en el 54 de la tabla = 18 del cuerpo.
    cuerpo[18..22].copy_from_slice(&33u32.to_le_bytes());

    let (xsdt, _fijas) = maquina_acpi(&[tabla_acpi(b"SPCR", &cuerpo)]);
    let sp = leer(&xsdt).serial.expect("no encontro el puerto serie");
    assert_eq!(sp.address, 0x0900_0000);
    assert_eq!(sp.gsi, 33);
}

/// Una SPCR mas corta de lo que el campo necesita no se lee a medias.
#[test]
fn una_spcr_truncada_no_inventa_nada() {
    let (xsdt, _fijas) = maquina_acpi(&[tabla_acpi(b"SPCR", &[0u8; 4])]);
    assert!(leer(&xsdt).serial.is_none());
}

// ---------------------------------------------------------------------------
// El segundo canal (D5, D17)
// ---------------------------------------------------------------------------

use crate::channel;

/// Arma un buzon como lo armaria el agente. Devuelve los bytes, que hay que
/// mantener vivos mientras se use.
fn buzon(capacidad: u32) -> Vec<u8> {
    let mut b = vec![0u8; channel::size_for(capacidad) as usize];
    b[0..4].copy_from_slice(&channel::EXPECTED_MAGIC.to_le_bytes());
    b[4..8].copy_from_slice(&channel::EXPECTED_VERSION.to_le_bytes());
    b[8..12].copy_from_slice(&capacidad.to_le_bytes());
    b
}

fn con_buzon_limpio<T>(f: impl FnOnce() -> T) -> T {
    let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
    channel::reset();
    f()
}

#[test]
fn se_adopta_un_buzon_bien_armado() {
    con_buzon_limpio(|| {
        let b = buzon(64);
        let m = unsafe { channel::adopt(7, b.as_ptr() as u64, b.len() as u64) }.unwrap();
        assert_eq!(m.capacity(), 64);
        assert_eq!(m.handle, 7);
        assert!(channel::current().is_some());

        assert!(channel::forget(7));
        assert!(channel::current().is_none());
        assert!(!channel::forget(7), "olvidarlo dos veces no puede decir que si");
    });
}

/// Sin la firma, ahi no hay un buzon: hay memoria con lo que hubiera antes.
#[test]
fn sin_firma_no_se_adopta() {
    con_buzon_limpio(|| {
        let mut b = buzon(64);
        b[0] = 0;
        assert_eq!(
            unsafe { channel::adopt(1, b.as_ptr() as u64, b.len() as u64) },
            Err(channel::Error::NoMagic)
        );
        assert!(channel::current().is_none());
    });
}

#[test]
fn una_capacidad_imposible_se_rechaza() {
    con_buzon_limpio(|| {
        // Cero.
        let b = buzon(0);
        assert_eq!(
            unsafe { channel::adopt(1, b.as_ptr() as u64, b.len() as u64) },
            Err(channel::Error::BadCapacity)
        );
        // Y una que no es potencia de dos: dar la vuelta seria una division.
        let mut b = buzon(64);
        b[8..12].copy_from_slice(&100u32.to_le_bytes());
        assert_eq!(
            unsafe { channel::adopt(1, b.as_ptr() as u64, b.len() as u64) },
            Err(channel::Error::BadCapacity)
        );
    });
}

/// Si el buzon no entra en lo que el agente reclamo, el kernel escribiria fuera
/// de lo que le entregaron.
#[test]
fn un_buzon_que_no_entra_se_rechaza() {
    con_buzon_limpio(|| {
        let b = buzon(64);
        let corto = b.len() as u64 - 1;
        assert_eq!(
            unsafe { channel::adopt(1, b.as_ptr() as u64, corto) },
            Err(channel::Error::TooSmall)
        );
        // Y ni el encabezado solo.
        assert_eq!(
            unsafe { channel::adopt(1, b.as_ptr() as u64, 4) },
            Err(channel::Error::TooSmall)
        );
    });
}

#[test]
fn lo_que_deja_el_agente_se_lee_en_orden() {
    con_buzon_limpio(|| {
        let mut b = buzon(8);
        let rings = channel::size_for(0) as usize; // el encabezado
        b[rings..rings + 3].copy_from_slice(&[0xAA, 0xBB, 0xCC]);
        // El agente avanza su indice despues de escribir.
        b[16..20].copy_from_slice(&3u32.to_le_bytes());

        let m = unsafe { channel::adopt(1, b.as_ptr() as u64, b.len() as u64) }.unwrap();
        unsafe {
            assert_eq!(m.pop(), Some(0xAA));
            assert_eq!(m.pop(), Some(0xBB));
            assert_eq!(m.pop(), Some(0xCC));
            assert_eq!(m.pop(), None, "leyo mas de lo que el agente escribio");
        }
    });
}

#[test]
fn las_respuestas_se_dejan_para_el_agente() {
    con_buzon_limpio(|| {
        let b = buzon(4);
        let m = unsafe { channel::adopt(1, b.as_ptr() as u64, b.len() as u64) }.unwrap();
        unsafe {
            for x in [1u8, 2, 3, 4] {
                assert!(m.push(x), "no entro un byte que si tenia lugar");
            }
            // Lleno: el agente todavia no leyo nada.
            assert!(!m.push(5), "escribio de mas y piso lo que el agente no leyo");
        }

        // El anillo de respuestas arranca despues del de pedidos.
        let base = channel::size_for(0) as usize + 4;
        assert_eq!(&b[base..base + 4], &[1, 2, 3, 4]);
        // Y el indice quedo avanzado para que el agente sepa cuanto hay.
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 4);
    });
}

/// El anillo da la vuelta: es un anillo, no una cinta.
#[test]
fn el_anillo_da_la_vuelta() {
    con_buzon_limpio(|| {
        let mut b = buzon(4);
        let m = unsafe { channel::adopt(1, b.as_ptr() as u64, b.len() as u64) }.unwrap();
        unsafe {
            for x in [1u8, 2, 3, 4] {
                assert!(m.push(x));
            }
        }
        // El agente dice que leyo dos.
        b[28..32].copy_from_slice(&2u32.to_le_bytes());
        unsafe {
            assert!(m.push(9), "no reuso el lugar que el agente libero");
        }
        // El 9 fue al lugar 0, que es donde estaba el 1.
        let base = channel::size_for(0) as usize + 4;
        assert_eq!(b[base], 9);
    });
}

// ---------------------------------------------------------------------------
// Handlers del agente (D9)
// ---------------------------------------------------------------------------

use crate::handlers;

fn con_handlers_limpios<T>(f: impl FnOnce() -> T) -> T {
    let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
    handlers::reset();
    f()
}

#[test]
fn se_instala_un_handler_y_se_encuentra_por_su_interrupcion() {
    con_handlers_limpios(|| {
        let slot = handlers::reserve(34, 0x1000, false).unwrap();
        assert_eq!(handlers::slot_of(34), Some(slot));
        assert_eq!(handlers::slot_of(35), None);
        let h = handlers::at(slot).unwrap();
        assert_eq!(h.entry, 0x1000);
        assert!(!h.raw);
        assert_eq!(h.count, 0);
    });
}

/// Dos handlers para la misma interrupcion serian dos codigos peleandose por un
/// evento: el segundo pediría ganar sin decirlo.
#[test]
fn no_se_instalan_dos_para_la_misma_interrupcion() {
    con_handlers_limpios(|| {
        handlers::reserve(34, 0x1000, false).unwrap();
        assert_eq!(handlers::reserve(34, 0x2000, false), Err(handlers::Error::Taken));
    });
}

#[test]
fn la_tabla_de_handlers_tiene_techo() {
    con_handlers_limpios(|| {
        for i in 0..handlers::MAX {
            handlers::reserve(100 + i as u32, 0x1000, false).unwrap();
        }
        assert_eq!(
            handlers::reserve(999, 0x1000, false),
            Err(handlers::Error::TableFull)
        );
    });
}

/// Si la arquitectura no pudo instalarlo, la ranura se suelta: dejarla tomada
/// haria que el proximo intento diga "ya instalado" por nada.
#[test]
fn una_ranura_que_no_se_uso_se_suelta() {
    con_handlers_limpios(|| {
        let slot = handlers::reserve(34, 0x1000, false).unwrap();
        handlers::release_slot(slot);
        assert!(handlers::at(slot).is_none());
        assert_eq!(handlers::slot_of(34), None);
        // Y se puede volver a pedir.
        assert!(handlers::reserve(34, 0x2000, false).is_ok());
    });
}

/// El agente lee esta cuenta para saber si su aparato esta hablando, sin tener
/// que instrumentar su propio codigo.
#[test]
fn se_cuenta_cada_vez_que_se_atiende() {
    con_handlers_limpios(|| {
        let slot = handlers::reserve(34, 0x1000, false).unwrap();
        handlers::served(slot);
        handlers::served(slot);
        assert_eq!(handlers::at(slot).unwrap().count, 2);

        // Una ranura que no existe no rompe nada.
        handlers::served(handlers::MAX + 5);
    });
}
