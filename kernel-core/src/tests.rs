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

    unsafe fn set_user_access(
        &mut self,
        _start: u64,
        _bytes: u64,
        _user: bool,
    ) -> Result<(), &'static str> {
        Err("la plataforma de prueba no pagina")
    }

    fn set_interrupts(&mut self, _on: bool) {}

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
fn format_of(bytes: u64) -> String {
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
fn uses_the_largest_unit_that_is_exact() {
    assert_eq!(format_of(4096), "4 KiB");
    assert_eq!(format_of(1024 * 1024), "1 MiB");
    assert_eq!(format_of(1024 * 1024 * 1024), "1 GiB");
    assert_eq!(format_of(12 * 1024 * 1024 * 1024), "12 GiB");
}

/// El invariante que sostiene P4: el kernel no miente sobre la maquina.
///
/// Un "512 MiB" que en realidad eran 512 MiB menos una pagina seria una mentira
/// silenciosa, del peor tipo: se lee igual de bien que la verdad.
#[test]
fn never_rounds() {
    let almost_512_mib = 512 * 1024 * 1024 - 4096;
    let s = format_of(almost_512_mib);
    assert!(!s.contains("MiB"), "redondeo a MiB: {s}");
    assert_eq!(s, "524284 KiB");

    let almost_1_gib = 1024 * 1024 * 1024 - 1024 * 1024;
    let s = format_of(almost_1_gib);
    assert!(!s.contains("GiB"), "redondeo a GiB: {s}");
    assert_eq!(s, "1023 MiB");
}

#[test]
fn what_is_not_a_multiple_of_kib_comes_out_in_bytes() {
    assert_eq!(format_of(1), "1 B");
    assert_eq!(format_of(1500), "1500 B");
}

#[test]
fn zero_breaks_nothing() {
    assert_eq!(format_of(0), "0 KiB");
}

// ---------------------------------------------------------------------------
// El mapa de memoria
// ---------------------------------------------------------------------------

#[test]
fn only_what_is_free_counts_as_free() {
    static REGIONS: [Region; 4] = [
        Region { start: 0, bytes: 4096, kind: Kind::Free },
        Region { start: 4096, bytes: 8192, kind: Kind::Firmware },
        Region { start: 12288, bytes: 4096, kind: Kind::Free },
        Region { start: 16384, bytes: 1 << 30, kind: Kind::Mmio },
    ];
    let m = Machine { regions: &REGIONS, tables: Tables::default(), failure: None };

    // 8 KiB: las dos regiones libres. Ni el firmware ni el MMIO cuentan, por
    // mas que el MMIO sea 1 GiB de espacio direccionable.
    assert_eq!(m.free_bytes(), 8192);
}

#[test]
fn a_mute_machine_has_nothing() {
    let m = Machine::mute("se rompio algo");
    assert_eq!(m.free_bytes(), 0);
    assert!(m.regions.is_empty());
    assert_eq!(m.failure, Some("se rompio algo"));
}

#[test]
fn the_end_of_a_region_does_not_overflow() {
    let r = Region { start: u64::MAX - 10, bytes: 1000, kind: Kind::Free };
    assert_eq!(r.end(), u64::MAX);
}

#[test]
fn an_unknown_kind_keeps_its_number() {
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
fn accepts_a_well_formed_rsdp() {
    let b = rsdp(2);
    let a = unsafe { read_acpi(b.as_ptr() as u64) }.expect("deberia aceptarlo");
    assert_eq!(a.revision, 2);
    assert_eq!(a.rsdt, 0xdead_beef);
    assert_eq!(a.xsdt, Some(0x1234_5678_9abc_def0));
}

#[test]
fn in_acpi_1_0_the_xsdt_is_not_read() {
    // La revision 0 no tiene encabezado extendido: leerlo seria leer lo que
    // haya al lado y pasarlo por una direccion.
    let a = unsafe { read_acpi(rsdp(0).as_ptr() as u64) }.unwrap();
    assert_eq!(a.xsdt, None);
    assert_eq!(a.rsdt, 0xdead_beef);
}

#[test]
fn rejects_a_foreign_signature() {
    let mut b = rsdp(2);
    b[0] = b'X';
    assert!(unsafe { read_acpi(b.as_ptr() as u64) }.is_none());
}

/// Esta es la razon de ser del checksum: un puntero que casualmente empiece
/// con la firma correcta pero no sea un RSDP.
#[test]
fn rejects_a_checksum_that_does_not_add_up() {
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
fn accepts_a_well_formed_device_tree() {
    let b = dtb();
    let d = unsafe { read_device_tree(b.as_ptr() as u64) }.expect("deberia aceptarlo");
    assert_eq!(d.bytes, 1024);
    assert_eq!(d.version, 17);
}

#[test]
fn rejects_a_magic_that_is_not() {
    let mut b = dtb();
    b[3] = 0x00;
    assert!(unsafe { read_device_tree(b.as_ptr() as u64) }.is_none());
}

/// Un magico escrito en little-endian tiene que ser rechazado: es justo el bug
/// que aparece si alguien "arregla" el `from_be`.
#[test]
fn rejects_the_magic_backwards() {
    let mut b = dtb();
    b[0..4].copy_from_slice(&0xd00d_feedu32.to_le_bytes());
    assert!(unsafe { read_device_tree(b.as_ptr() as u64) }.is_none());
}

// ---------------------------------------------------------------------------
// CBOR
// ---------------------------------------------------------------------------

/// Codifica con el Writer y devuelve los bytes.
fn encode(f: impl FnOnce(&mut Writer<'_>)) -> Vec<u8> {
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
fn integers_are_encoded_as_the_rfc_says() {
    assert_eq!(encode(|w| w.uint(0)), [0x00]);
    assert_eq!(encode(|w| w.uint(23)), [0x17]);
    // 24 ya no entra en los 5 bits del encabezado: pasa a un byte aparte.
    assert_eq!(encode(|w| w.uint(24)), [0x18, 0x18]);
    assert_eq!(encode(|w| w.uint(255)), [0x18, 0xff]);
    assert_eq!(encode(|w| w.uint(256)), [0x19, 0x01, 0x00]);
    assert_eq!(encode(|w| w.uint(65535)), [0x19, 0xff, 0xff]);
    assert_eq!(encode(|w| w.uint(65536)), [0x1a, 0x00, 0x01, 0x00, 0x00]);
    assert_eq!(
        encode(|w| w.uint(u64::MAX)),
        [0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
    );
}

#[test]
fn strings_arrays_and_simples_as_the_rfc_says() {
    assert_eq!(encode(|w| w.text("")), [0x60]);
    assert_eq!(encode(|w| w.text("a")), [0x61, 0x61]);
    assert_eq!(encode(|w| w.text("IETF")), [0x64, 0x49, 0x45, 0x54, 0x46]);
    assert_eq!(encode(|w| w.bytes(&[1, 2, 3, 4])), [0x44, 1, 2, 3, 4]);
    assert_eq!(encode(|w| w.bool(false)), [0xf4]);
    assert_eq!(encode(|w| w.bool(true)), [0xf5]);
    assert_eq!(encode(|w| w.null()), [0xf6]);
    assert_eq!(
        encode(|w| {
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
fn a_half_message_is_recognized_as_incomplete() {
    let encoded = encode(|w| {
        w.array(3);
        w.uint(7);
        w.text("describe");
        w.map(0);
    });

    // Cada prefijo estricto tiene que decir "falta mas", nunca "listo".
    for corte in 0..encoded.len() {
        assert_eq!(
            scan(&encoded[..corte]),
            Scan::Incomplete,
            "el prefijo de {corte} bytes se dio por completo"
        );
    }
    assert_eq!(scan(&encoded), Scan::Complete(encoded.len()));
}

/// Si vienen dos pedidos pegados, el primero tiene que medirse solo.
#[test]
fn two_glued_messages_are_not_confused() {
    let mut flow = encode(|w| {
        w.array(3);
        w.uint(1);
        w.text("describe");
        w.map(0);
    });
    let first = flow.len();
    flow.extend_from_slice(&encode(|w| w.uint(42)));

    assert_eq!(scan(&flow), Scan::Complete(first));
}

#[test]
fn what_is_not_cbor_is_rejected_without_waiting_for_more() {
    // 0x1c, 0x1d y 0x1e no existen en la especificacion.
    assert_eq!(scan(&[0x1c]), Scan::Malformed);
    // 0x1f es largo indefinido: valido en CBOR, no soportado acá a proposito.
    assert_eq!(scan(&[0x9f]), Scan::Malformed);
}

/// Un mensaje hostil no puede hacer que el kernel se quede sin pila.
#[test]
fn nesting_has_a_ceiling() {
    // 20 arreglos de un elemento, uno adentro del otro.
    let deep = vec![0x81u8; 20];
    assert_eq!(scan(&deep), Scan::Malformed);
}

#[test]
fn what_was_written_is_read_back() {
    let msg = encode(|w| {
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
fn asking_for_the_wrong_type_does_not_lose_the_thread() {
    let msg = encode(|w| w.text("hola"));
    let mut r = Reader::new(&msg);

    assert_eq!(r.uint(), None);
    assert_eq!(r.array(), None);
    assert_eq!(r.text(), Some("hola"));
}

/// Saltear una clave desconocida es lo que permite agregar argumentos nuevos
/// sin romper a un kernel viejo.
#[test]
fn any_value_can_be_skipped() {
    let msg = encode(|w| {
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
fn if_it_does_not_fit_it_says_so_instead_of_breaking() {
    let mut small = [0u8; 4];
    let mut w = Writer::new(&mut small);
    w.text("esto es mucho mas largo que cuatro bytes");
    assert!(w.finish().is_none());
}

#[test]
fn invalid_utf8_text_is_rejected() {
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
static MAP: [Region; 4] = [
    Region { start: 0x1000, bytes: 0x1000, kind: Kind::Kernel },
    Region { start: 0x2000, bytes: 0x1000, kind: Kind::Kernel },
    Region { start: 0x3000, bytes: 0x1000, kind: Kind::Free },
    Region { start: 0x4000, bytes: 0x1000, kind: Kind::Firmware },
];

fn machine() -> Machine {
    Machine { regions: &MAP, tables: Tables::default(), failure: None }
}

#[test]
fn the_region_of_an_address_is_found() {
    let m = machine();
    assert_eq!(m.region_containing(0x1000).map(|r| r.kind), Some(Kind::Kernel));
    assert_eq!(m.region_containing(0x1fff).map(|r| r.kind), Some(Kind::Kernel));
    assert_eq!(m.region_containing(0x3500).map(|r| r.kind), Some(Kind::Free));
    // El final de una region ya no le pertenece.
    assert!(m.region_containing(0x5000).is_none());
}

#[test]
fn what_is_wholly_in_kernel_memory_is_ours() {
    let m = machine();
    assert!(m.is_ours(0x1000, 0x1000));
    // A caballo de dos regiones del kernel pegadas: sigue siendo nuestro.
    assert!(m.is_ours(0x1800, 0x1000));
    assert!(m.is_ours(0x1000, 0x2000));
}

/// Este es el caso que motiva todo: una pila que empieza en memoria nuestra
/// pero se pasa a memoria que `mem.claim` podria entregar.
#[test]
fn what_crosses_into_claimable_memory_is_not_ours() {
    let m = machine();
    assert!(!m.is_ours(0x2800, 0x1000), "se metio en la region libre");
    assert!(!m.is_ours(0x3000, 0x100), "arranca en memoria libre");
    assert!(!m.is_ours(0x4000, 0x100), "arranca en memoria del firmware");
}

#[test]
fn an_unmapped_gap_is_not_ours_either() {
    let m = machine();
    assert!(!m.is_ours(0x9000, 0x100));
    // Empieza bien y se cae por un hueco.
    assert!(!m.is_ours(0x4f00, 0x1000));
}

#[test]
fn without_a_map_nothing_is_ours() {
    // Una maquina muda no puede afirmar que algo sea suyo.
    assert!(!Machine::mute("sin datos").is_ours(0x1000, 0x10));
}

// ---------------------------------------------------------------------------
// El plan de mapeo (D12)
// ---------------------------------------------------------------------------

fn from_regions(regions: &'static [Region]) -> Machine {
    Machine { regions: regions, tables: Tables::default(), failure: None }
}

#[test]
fn coverage_reaches_the_highest_region_rounding_up() {
    // Un solo byte pasado el GiB obliga a mapear el GiB siguiente entero.
    static OVER_A_GIB: [Region; 1] = [Region { start: 0, bytes: GIB + 1, kind: Kind::Free }];
    assert_eq!(span_gib(&from_regions(&OVER_A_GIB)), 2);

    // Justo en el limite no hace falta uno mas.
    static EXACTLY_A_GIB: [Region; 1] = [Region { start: 0, bytes: GIB, kind: Kind::Free }];
    assert_eq!(span_gib(&from_regions(&EXACTLY_A_GIB)), 1);

    // Se cubre hasta arriba de todo, no hasta donde llega la RAM: ahi viven los
    // BARs de PCIe.
    static HIGH_REGION: [Region; 2] = [
        Region { start: 0, bytes: GIB, kind: Kind::Free },
        Region { start: 100 * GIB, bytes: GIB, kind: Kind::Mmio },
    ];
    assert_eq!(span_gib(&from_regions(&HIGH_REGION)), 101);
}

#[test]
fn a_page_with_ram_is_cacheable() {
    static RAM: [Region; 1] = [Region { start: 0, bytes: GIB, kind: Kind::Free }];
    assert_eq!(attr_of(&from_regions(&RAM), 0), Attr::Memory);
}

/// El error caro y el barato no son simetricos: cachear un registro rompe el
/// dispositivo, no cachear RAM solo la hace lenta.
#[test]
fn a_single_register_makes_the_whole_page_uncacheable() {
    static MIXED: [Region; 2] = [
        Region { start: 0, bytes: GIB - 4096, kind: Kind::Free },
        // Una sola pagina de MMIO al final del GiB.
        Region { start: GIB - 4096, bytes: 4096, kind: Kind::Mmio },
    ];
    assert_eq!(attr_of(&from_regions(&MIXED), 0), Attr::Device);
}

#[test]
fn an_empty_gap_is_treated_as_a_device() {
    static FAR_OFF: [Region; 1] = [Region { start: 0, bytes: GIB, kind: Kind::Free }];
    // La pagina 5 no la menciona nadie: puede haber un dispositivo que este
    // kernel todavia no sabe que existe.
    assert_eq!(attr_of(&from_regions(&FAR_OFF), 5), Attr::Device);
}

#[test]
fn what_the_machine_could_not_explain_is_not_assumed_ram() {
    static UNUSUAL: [Region; 3] = [
        Region { start: 0, bytes: 4096, kind: Kind::Reserved },
        Region { start: 4096, bytes: 4096, kind: Kind::Broken },
        Region { start: 8192, bytes: 4096, kind: Kind::Other(77) },
    ];
    assert_eq!(attr_of(&from_regions(&UNUSUAL), 0), Attr::Device);
}

#[test]
fn firmware_and_acpi_tables_are_memory() {
    static FW: [Region; 2] = [
        Region { start: 0, bytes: 4096, kind: Kind::Firmware },
        Region { start: 4096, bytes: 4096, kind: Kind::AcpiTables },
    ];
    assert_eq!(attr_of(&from_regions(&FW), 0), Attr::Memory);
}

/// Una region que arranca en una pagina y termina en la siguiente tiene que
/// contar para las dos.
#[test]
fn a_straddling_region_affects_both_pages() {
    static STRADDLING: [Region; 1] = [Region {
        start: GIB - 4096,
        bytes: 8192,
        kind: Kind::Mmio,
    }];
    let m = from_regions(&STRADDLING);
    assert_eq!(attr_of(&m, 0), Attr::Device);
    assert_eq!(attr_of(&m, 1), Attr::Device);
}

// ---------------------------------------------------------------------------
// Faults (P5, D7)
// ---------------------------------------------------------------------------

/// Un breakpoint es un alto pedido: se sigue en la instruccion de al lado.
/// Cualquier otra cosa volveria a fallar en la misma instruccion, para siempre.
#[test]
fn only_the_breakpoint_can_be_resumed() {
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
fn each_cause_has_its_code_and_they_do_not_repeat() {
    let all_of = [
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
    let mut seen = std::collections::HashSet::new();
    for c in all_of {
        assert!(!c.code().is_empty());
        assert!(seen.insert(c.code()), "codigo repetido: {}", c.code());
    }
}

fn fault_text(f: &Fault, names: &[&str]) -> String {
    let mut s = String::new();
    report(f, names, &mut s);
    s
}

#[test]
fn the_report_says_cause_pc_and_registers() {
    static VALUES: [u64; 2] = [0xdead, 0xbeef];
    let f = Fault {
        cause: Cause::PageFault,
        raw: 14,
        detail: 0x2,
        pc: 0x1234,
        address: Some(0xcafe),
        regs: &VALUES,
    };
    let t = fault_text(&f, &["uno", "dos"]);

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
fn without_an_address_none_is_reported() {
    let f = Fault { cause: Cause::Breakpoint, raw: 3, detail: 0, pc: 0x99, address: None, regs: &[] };
    let t = fault_text(&f, &[]);
    assert!(t.contains("breakpoint"), "{t}");
    assert!(!t.contains("direccion tocada"), "{t}");
}

/// El reporte lo arma quien tiene los nombres, y este modulo no conoce ninguno
/// (D3). Si vinieran de más o de menos, no puede quedar leyendo fuera de rango.
#[test]
fn mismatched_names_and_values_do_not_overflow() {
    static THREE: [u64; 3] = [1, 2, 3];
    let f = Fault { cause: Cause::Unknown, raw: 0, detail: 0, pc: 0, address: None, regs: &THREE };

    // Más nombres que valores.
    let t = fault_text(&f, &["a", "b", "c", "d", "e"]);
    assert!(t.contains("c=") && !t.contains("d="), "{t}");

    // Y menos.
    let t = fault_text(&f, &["a"]);
    assert!(t.contains("a=") && !t.contains("b="), "{t}");
}

// ---------------------------------------------------------------------------
// Reclamos (D14)
// ---------------------------------------------------------------------------

use crate::claims::{self, Error, Request};

/// La tabla de reclamos es un estatico compartido, y los tests corren en
/// paralelo. Sin serializarlos se pisarian entre ellos y fallarian por turnos.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_clean_table<T>(f: impl FnOnce() -> T) -> T {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    claims::reset();
    f()
}

/// 0x0000-0x1000 kernel, 0x1000-0x5000 libre, 0x5000-0x6000 mmio.
static SAMPLE_MAP: [Region; 3] = [
    Region { start: 0x0000, bytes: 0x1000, kind: Kind::Kernel },
    Region { start: 0x1000, bytes: 0x4000, kind: Kind::Free },
    Region { start: 0x5000, bytes: 0x1000, kind: Kind::Mmio },
];

fn sample_machine() -> Machine {
    Machine { regions: &SAMPLE_MAP, tables: Tables::default(), failure: None }
}

fn request_of(bytes: u64) -> Request {
    Request { bytes, ..Default::default() }
}

#[test]
fn only_free_memory_is_handed_out() {
    with_clean_table(|| {
        let c = claims::claim(&sample_machine(), request_of(0x100)).unwrap();
        assert_eq!(c.kind, Kind::Free);
        // No salio de la region del kernel, que empieza en 0.
        assert!(c.start >= 0x1000, "{:#x}", c.start);
        assert!(c.end() <= 0x5000);
    });
}

#[test]
fn two_claims_do_not_overlap() {
    with_clean_table(|| {
        let m = sample_machine();
        let a = claims::claim(&m, request_of(0x1000)).unwrap();
        let b = claims::claim(&m, request_of(0x1000)).unwrap();
        assert!(
            a.end() <= b.start || b.end() <= a.start,
            "se pisan: {:#x}..{:#x} y {:#x}..{:#x}",
            a.start, a.end(), b.start, b.end()
        );
    });
}

#[test]
fn alignment_is_respected() {
    with_clean_table(|| {
        let m = sample_machine();
        // Primero algo chico para desalinear el hueco siguiente.
        claims::claim(&m, request_of(1)).unwrap();
        let c = claims::claim(&m, Request { bytes: 0x100, align: 0x1000, ..Default::default() })
            .unwrap();
        assert_eq!(c.start % 0x1000, 0, "{:#x}", c.start);
    });
}

#[test]
fn an_alignment_that_is_not_a_power_of_two_is_rejected() {
    with_clean_table(|| {
        let r = Request { bytes: 16, align: 3, ..Default::default() };
        assert_eq!(claims::claim(&sample_machine(), r), Err(Error::BadAlign));
    });
}

/// Existe porque hay dispositivos que solo hacen DMA por debajo de los 4 GiB.
#[test]
fn the_cap_is_respected() {
    with_clean_table(|| {
        let r = Request { bytes: 0x100, below: Some(0x2000), ..Default::default() };
        let c = claims::claim(&sample_machine(), r).unwrap();
        assert!(c.end() <= 0x2000, "{:#x}", c.end());

        // Y si no entra debajo del tope, se dice que no hay lugar.
        let r = Request { bytes: 0x100, below: Some(0x1000), ..Default::default() };
        assert_eq!(claims::claim(&sample_machine(), r), Err(Error::NoRoom));
    });
}

#[test]
fn what_does_not_fit_is_not_handed_out() {
    with_clean_table(|| {
        assert_eq!(claims::claim(&sample_machine(), request_of(0x100000)), Err(Error::NoRoom));
        assert_eq!(claims::claim(&sample_machine(), request_of(0)), Err(Error::Empty));
    });
}

/// El MMIO se reclama por direccion exacta: el BAR de un dispositivo esta donde
/// esta, no donde haya lugar.
#[test]
fn mmio_is_claimed_by_exact_address() {
    with_clean_table(|| {
        let r = Request { bytes: 0x1000, at: Some(0x5000), ..Default::default() };
        let c = claims::claim(&sample_machine(), r).unwrap();
        assert_eq!(c.start, 0x5000);
        assert_eq!(c.kind, Kind::Mmio);
    });
}

/// Entregar la memoria donde vive la pila del kernel no seria libertad, seria
/// incoherencia: el kernel es lo que esta prestando el servicio.
#[test]
fn kernel_memory_is_never_handed_out() {
    with_clean_table(|| {
        let r = Request { bytes: 0x100, at: Some(0x0), ..Default::default() };
        assert_eq!(claims::claim(&sample_machine(), r), Err(Error::IsKernel));
    });
}

#[test]
fn what_the_machine_did_not_report_is_not_handed_out() {
    with_clean_table(|| {
        let r = Request { bytes: 0x100, at: Some(0x99000), ..Default::default() };
        assert_eq!(claims::claim(&sample_machine(), r), Err(Error::Unmapped));

        // Ni un rango que empieza bien y se sale de la region.
        let r = Request { bytes: 0x2000, at: Some(0x5000), ..Default::default() };
        assert_eq!(claims::claim(&sample_machine(), r), Err(Error::Unmapped));
    });
}

#[test]
fn what_is_already_claimed_is_not_claimed_again() {
    with_clean_table(|| {
        let m = sample_machine();
        let c = claims::claim(&m, request_of(0x100)).unwrap();
        let r = Request { bytes: 0x10, at: Some(c.start), ..Default::default() };
        assert_eq!(claims::claim(&m, r), Err(Error::Taken));
    });
}

/// Un handle liberado no se reusa: si se reusara, un pedido que llega tarde con
/// un handle viejo tocaria lo que otro reclamo despues en el mismo lugar.
#[test]
fn handles_are_not_reused() {
    with_clean_table(|| {
        let m = sample_machine();
        let a = claims::claim(&m, request_of(0x100)).unwrap();
        assert!(claims::release(a.handle));
        let b = claims::claim(&m, request_of(0x100)).unwrap();
        assert_ne!(a.handle, b.handle);
        // Y el viejo ya no existe.
        assert!(claims::get(a.handle).is_none());
        assert!(!claims::release(a.handle));
    });
}

#[test]
fn reading_outside_the_claim_is_not_possible() {
    with_clean_table(|| {
        let c = claims::claim(&sample_machine(), request_of(0x100)).unwrap();
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
fn what_is_claimed_can_be_listed() {
    with_clean_table(|| {
        let m = sample_machine();
        let a = claims::claim(&m, request_of(0x100)).unwrap();
        let b = claims::claim(&m, request_of(0x100)).unwrap();
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
fn acpi_table(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut t = vec![0u8; 36];
    t[..4].copy_from_slice(signature);
    t.extend_from_slice(body);
    let length = t.len() as u32;
    t[4..8].copy_from_slice(&length.to_le_bytes());

    // ACPI manda que la tabla entera sume 0 modulo 256.
    let suma = t.iter().fold(0u8, |a, x| a.wrapping_add(*x));
    t[9] = 0u8.wrapping_sub(suma);
    t
}

/// Una entrada de la MADT: tipo, largo, y payload.
fn entry(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut e = vec![kind, (payload.len() + 2) as u8];
    e.extend_from_slice(payload);
    e
}

/// Arma un XSDT que apunta a las tablas dadas, y devuelve todo junto para que
/// no se muevan de lugar mientras se lee.
fn machine_with_acpi(tables: &[Vec<u8>]) -> (Vec<u8>, Vec<Box<[u8]>>) {
    // Las tablas van al heap y se quedan quietas ahi; el XSDT guarda punteros.
    let fixed: Vec<Box<[u8]>> = tables.iter().map(|t| t.clone().into_boxed_slice()).collect();
    let mut pointers = Vec::new();
    for t in &fixed {
        pointers.extend_from_slice(&(t.as_ptr() as u64).to_le_bytes());
    }
    (acpi_table(b"XSDT", &pointers), fixed)
}

fn read_hw(xsdt: &[u8]) -> acpi::Hardware {
    let rsdp = Rsdp { revision: 2, rsdt: 0, xsdt: Some(xsdt.as_ptr() as u64) };
    unsafe { acpi::read(&rsdp) }
}

#[test]
fn the_x86_cores_are_read() {
    // Tres APICs locales: dos habilitados y uno que no.
    let mut body = 0xFEE0_0000u32.to_le_bytes().to_vec(); // direccion del APIC
    body.extend_from_slice(&0u32.to_le_bytes()); // banderas
    body.extend(entry(0, &[0, 0, 1, 0, 0, 0])); // uid 0, apic 0, habilitado
    body.extend(entry(0, &[1, 7, 1, 0, 0, 0])); // uid 1, apic 7, habilitado
    body.extend(entry(0, &[2, 9, 0, 0, 0, 0])); // uid 2, apic 9, NO

    let madt = acpi_table(b"APIC", &body);
    let (xsdt, _fixed) = machine_with_acpi(&[madt]);
    let hw = read_hw(&xsdt);

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
fn the_arm_cores_are_read() {
    let mut body = vec![0u8; 8];
    // GICC: el identificador que sirve para arrancarlo es el MPIDR, en el
    // offset 68 de la entrada.
    let mut gicc = vec![0u8; 74];
    gicc[6..10].copy_from_slice(&5u32.to_le_bytes()); // uid, en el offset 8
    gicc[10..14].copy_from_slice(&1u32.to_le_bytes()); // banderas: habilitado
    gicc[66..74].copy_from_slice(&0x8000_0003u64.to_le_bytes()); // mpidr
    body.extend(entry(11, &gicc));

    // GICD: el distribuidor, version 3.
    let mut gicd = vec![0u8; 22];
    gicd[6..14].copy_from_slice(&0x0800_0000u64.to_le_bytes()); // direccion
    gicd[18] = 3; // version
    body.extend(entry(12, &gicd));

    let (xsdt, _fixed) = machine_with_acpi(&[acpi_table(b"APIC", &body)]);
    let hw = read_hw(&xsdt);

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
fn where_pcie_is_gets_read() {
    let mut body = vec![0u8; 8]; // reservado
    body.extend_from_slice(&0xE000_0000u64.to_le_bytes()); // base
    body.extend_from_slice(&0u16.to_le_bytes()); // segmento
    body.push(0); // primer bus
    body.push(255); // ultimo bus
    body.extend_from_slice(&0u32.to_le_bytes());

    let (xsdt, _fixed) = machine_with_acpi(&[acpi_table(b"MCFG", &body)]);
    let x = read_hw(&xsdt).pcie.expect("no encontro PCIe");
    assert_eq!(x.base, 0xE000_0000);
    assert_eq!(x.bus_end, 255);
}

/// Informar que existe una tabla que este kernel todavia no lee es mas util que
/// callarla (P4).
#[test]
fn tables_that_are_not_interpreted_are_still_reported() {
    let (xsdt, _fixed) = machine_with_acpi(&[
        acpi_table(b"FACP", &[0u8; 4]),
        acpi_table(b"DSDT", &[0u8; 4]),
    ]);
    let hw = read_hw(&xsdt);
    assert_eq!(hw.signatures.len(), 2);
    assert_eq!(&hw.signatures[0], b"FACP");
    assert_eq!(&hw.signatures[1], b"DSDT");
    assert!(hw.cpus.is_empty());
}

/// Es la razon de ser del checksum: recorriendo memoria cruda, dar con cuatro
/// bytes que parecen una firma es mas facil de lo que parece.
#[test]
fn a_table_with_a_broken_checksum_is_ignored() {
    let mut madt = acpi_table(b"APIC", &[0u8; 8]);
    madt[9] = madt[9].wrapping_add(1);
    let (xsdt, _fixed) = machine_with_acpi(&[madt]);
    let hw = read_hw(&xsdt);
    assert!(hw.signatures.is_empty(), "acepto una tabla que no cerraba");
}

/// Una entrada de largo cero haria girar el recorrido para siempre.
#[test]
fn an_entry_with_an_impossible_length_does_not_hang() {
    let mut body = vec![0u8; 8];
    body.push(0); // tipo
    body.push(0); // largo CERO
    body.extend_from_slice(&[0u8; 8]);

    let (xsdt, _fixed) = machine_with_acpi(&[acpi_table(b"APIC", &body)]);
    // Que vuelva ya es la prueba.
    let hw = read_hw(&xsdt);
    assert!(hw.cpus.is_empty());
}

#[test]
fn without_a_root_nothing_is_invented() {
    let rsdp = Rsdp { revision: 2, rsdt: 0, xsdt: Some(0) };
    let hw = unsafe { acpi::read(&rsdp) };
    assert!(hw.cpus.is_empty() && hw.signatures.is_empty());
    assert!(hw.interrupts.is_none() && hw.pcie.is_none());
}

// ---------------------------------------------------------------------------
// Nucleos (D13)
// ---------------------------------------------------------------------------

use crate::cores;

fn with_clean_cores<T>(f: impl FnOnce() -> T) -> T {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    cores::reset();
    f()
}

#[test]
fn a_core_is_not_alive_until_it_says_so() {
    with_clean_cores(|| {
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
fn core_zero_can_also_say_so() {
    with_clean_cores(|| {
        let (slot, _) = cores::reserve(0).unwrap();
        assert!(!cores::has_arrived(slot));
        cores::arrived(slot, 0);
        assert!(cores::has_arrived(slot), "el nucleo 0 no pudo avisar");
    });
}

#[test]
fn each_core_has_its_slot_and_its_handle() {
    with_clean_cores(|| {
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
fn what_is_claimed_can_be_listed_without_repeats() {
    with_clean_cores(|| {
        cores::reserve(3).unwrap();
        assert!(cores::is_claimed(3));
        assert!(!cores::is_claimed(4));
        assert_eq!(cores::count(), 1);
    });
}

#[test]
fn the_core_table_has_a_ceiling() {
    with_clean_cores(|| {
        for i in 0..cores::MAX {
            cores::reserve(i as u64).unwrap();
        }
        assert_eq!(cores::reserve(999), Err(cores::Error::TableFull));
    });
}

#[test]
fn the_state_of_a_core_can_be_corrected() {
    with_clean_cores(|| {
        let (slot, _) = cores::reserve(5).unwrap();
        assert_eq!(cores::all().next().unwrap().state, cores::State::Starting);
        cores::settle(slot, cores::State::Failed);
        assert_eq!(cores::all().next().unwrap().state, cores::State::Failed);
    });
}

/// El controlador que recibe las interrupciones de los aparatos, y las
/// interrupciones viejas de PC que esta maquina movio de numero.
#[test]
fn the_ioapic_and_the_moved_numbers_are_read() {
    let mut body = vec![0u8; 8];
    // Tipo 1: el IO-APIC. Direccion en el offset 4 de la entrada, primer
    // numero global en el 8.
    let mut ioapic = vec![0u8; 10];
    ioapic[2..6].copy_from_slice(&0xFEC0_0000u32.to_le_bytes());
    ioapic[6..10].copy_from_slice(&0u32.to_le_bytes());
    body.extend(entry(1, &ioapic));

    // Tipo 2: la interrupcion 4 (el serie de la PC) esta en la 20.
    let mut over = vec![0u8; 8];
    over[1] = 4; // source, en el offset 3 de la entrada
    over[2..6].copy_from_slice(&20u32.to_le_bytes());
    body.extend(entry(2, &over));

    let (xsdt, _fixed) = machine_with_acpi(&[acpi_table(b"APIC", &body)]);
    let hw = read_hw(&xsdt);

    let io = hw.ioapic.expect("no encontro el IO-APIC");
    assert_eq!(io.address, 0xFEC0_0000);
    assert_eq!(io.gsi_base, 0);

    // La 4 se movio a la 20; las que no estan en la tabla no cambiaron.
    assert_eq!(hw.gsi_of(4), 20);
    assert_eq!(hw.gsi_of(1), 1, "una interrupcion sin override no cambia");
}

/// La maquina diciendo donde tiene su consola, en vez de que la supongamos.
#[test]
fn where_the_serial_port_is_gets_read() {
    let mut body = vec![0u8; 22]; // hasta el offset 58 de la tabla
    // La direccion vive adentro de una estructura generica que arranca en el
    // offset 40 de la tabla; la direccion misma en el 44, o sea el 8 del cuerpo.
    body[8..16].copy_from_slice(&0x0900_0000u64.to_le_bytes());
    // El numero de interrupcion, en el 54 de la tabla = 18 del cuerpo.
    body[18..22].copy_from_slice(&33u32.to_le_bytes());

    let (xsdt, _fixed) = machine_with_acpi(&[acpi_table(b"SPCR", &body)]);
    let sp = read_hw(&xsdt).serial.expect("no encontro el puerto serie");
    assert_eq!(sp.address, 0x0900_0000);
    assert_eq!(sp.gsi, 33);
}

/// Una SPCR mas corta de lo que el campo necesita no se lee a medias.
#[test]
fn a_truncated_spcr_invents_nothing() {
    let (xsdt, _fixed) = machine_with_acpi(&[acpi_table(b"SPCR", &[0u8; 4])]);
    assert!(read_hw(&xsdt).serial.is_none());
}

// ---------------------------------------------------------------------------
// El segundo canal (D5, D17)
// ---------------------------------------------------------------------------

use crate::channel;

/// Arma un buzon como lo armaria el agente. Devuelve los bytes, que hay que
/// mantener vivos mientras se use.
fn mailbox(capacity: u32) -> Vec<u8> {
    let mut b = vec![0u8; channel::size_for(capacity) as usize];
    b[0..4].copy_from_slice(&channel::EXPECTED_MAGIC.to_le_bytes());
    b[4..8].copy_from_slice(&channel::EXPECTED_VERSION.to_le_bytes());
    b[8..12].copy_from_slice(&capacity.to_le_bytes());
    b
}

fn with_clean_mailbox<T>(f: impl FnOnce() -> T) -> T {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    channel::reset();
    f()
}

#[test]
fn a_well_built_mailbox_is_adopted() {
    with_clean_mailbox(|| {
        let b = mailbox(64);
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
fn without_a_signature_it_is_not_adopted() {
    with_clean_mailbox(|| {
        let mut b = mailbox(64);
        b[0] = 0;
        assert_eq!(
            unsafe { channel::adopt(1, b.as_ptr() as u64, b.len() as u64) },
            Err(channel::Error::NoMagic)
        );
        assert!(channel::current().is_none());
    });
}

#[test]
fn an_impossible_capacity_is_rejected() {
    with_clean_mailbox(|| {
        // Cero.
        let b = mailbox(0);
        assert_eq!(
            unsafe { channel::adopt(1, b.as_ptr() as u64, b.len() as u64) },
            Err(channel::Error::BadCapacity)
        );
        // Y una que no es potencia de dos: dar la vuelta seria una division.
        let mut b = mailbox(64);
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
fn a_mailbox_that_does_not_fit_is_rejected() {
    with_clean_mailbox(|| {
        let b = mailbox(64);
        let short = b.len() as u64 - 1;
        assert_eq!(
            unsafe { channel::adopt(1, b.as_ptr() as u64, short) },
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
fn what_the_agent_leaves_is_read_in_order() {
    with_clean_mailbox(|| {
        let mut b = mailbox(8);
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
fn replies_are_left_for_the_agent() {
    with_clean_mailbox(|| {
        let b = mailbox(4);
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
fn the_ring_wraps_around() {
    with_clean_mailbox(|| {
        let mut b = mailbox(4);
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

fn with_clean_handlers<T>(f: impl FnOnce() -> T) -> T {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    handlers::reset();
    f()
}

#[test]
fn a_handler_is_installed_and_found_by_its_interrupt() {
    with_clean_handlers(|| {
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
fn two_are_not_installed_for_the_same_interrupt() {
    with_clean_handlers(|| {
        handlers::reserve(34, 0x1000, false).unwrap();
        assert_eq!(handlers::reserve(34, 0x2000, false), Err(handlers::Error::Taken));
    });
}

#[test]
fn the_handler_table_has_a_ceiling() {
    with_clean_handlers(|| {
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
fn a_slot_that_was_not_used_is_released() {
    with_clean_handlers(|| {
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
fn it_is_counted_every_time_it_is_served() {
    with_clean_handlers(|| {
        let slot = handlers::reserve(34, 0x1000, false).unwrap();
        handlers::served(slot);
        handlers::served(slot);
        assert_eq!(handlers::at(slot).unwrap().count, 2);

        // Una ranura que no existe no rompe nada.
        handlers::served(handlers::MAX + 5);
    });
}

/// Lo que decide si un pedazo se le puede dejar alcanzar al agente cuando corra
/// sin privilegio (D27).
#[test]
fn which_chunks_have_kernel_inside_is_recognized() {
    // El kernel en 0x1000..0x2000, dentro del primer GiB.
    static MAP: [Region; 3] = [
        Region { start: 0, bytes: 0x1000, kind: Kind::Free },
        Region { start: 0x1000, bytes: 0x1000, kind: Kind::Kernel },
        Region { start: GIB, bytes: GIB, kind: Kind::Free },
    ];
    let m = Machine { regions: &MAP, tables: Tables::default(), failure: None };

    assert!(paging_touches(&m, 0x1000, 0x2000), "el rango del kernel mismo");
    assert!(paging_touches(&m, 0, 0x2000), "un rango que lo incluye");
    assert!(!paging_touches(&m, 0, 0x1000), "justo antes no lo toca");
    assert!(!paging_touches(&m, 0x2000, 0x3000), "justo despues tampoco");

    // Los dos pedazos hay que partirlos, por motivos distintos: el primero
    // porque tiene kernel adentro, el segundo porque tiene memoria libre y de
    // ahi salen los reclamos, que pueden pedir ser alcanzables sin privilegio.
    assert!(crate::paging::needs_split(&m, 0));
    assert!(crate::paging::needs_split(&m, 1));
}

/// Un pedazo que no tiene ni kernel ni memoria libre no se parte: nadie va a
/// necesitar decir cosas distintas de sus bloques.
#[test]
fn a_chunk_with_only_devices_is_not_split() {
    static MAP: [Region; 2] = [
        Region { start: 0, bytes: 0x1000, kind: Kind::Kernel },
        Region { start: 3 * GIB, bytes: GIB, kind: Kind::Mmio },
    ];
    let m = Machine { regions: &MAP, tables: Tables::default(), failure: None };
    assert!(crate::paging::needs_split(&m, 0), "el del kernel si");
    assert!(!crate::paging::needs_split(&m, 3), "el de puro mmio no");
    assert!(!crate::paging::needs_split(&m, 2), "y uno vacio tampoco");
}

fn paging_touches(m: &Machine, a: u64, b: u64) -> bool {
    crate::paging::touches_kernel(m, a, b)
}

/// El bloque de 2 MiB es el grano fino, y la consecuencia es que memoria del
/// agente pegada al kernel queda del lado del kernel. Conviene que este escrito.
#[test]
fn fine_grain_drags_along_what_is_next_to_it() {
    static MAP: [Region; 2] = [
        Region { start: 0x1000, bytes: 0x1000, kind: Kind::Kernel },
        Region { start: 0x2000, bytes: 0x1000, kind: Kind::Free },
    ];
    let m = Machine { regions: &MAP, tables: Tables::default(), failure: None };

    // Las dos caen en el mismo bloque de 2 MiB, asi que el bloque entero queda
    // fuera del alcance del agente aunque una de las dos sea libre.
    let block = crate::paging::BLOCK;
    assert!(crate::paging::touches_kernel(&m, 0, block));
}
