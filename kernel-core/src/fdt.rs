//! El device tree: la otra forma en que una maquina se describe (P4, D24).
//!
//! # Por que hacen falta las dos
//!
//! ACPI y device tree dicen lo mismo y no se parecen en nada. ACPI es lo que
//! traen los servidores —x86 siempre, ARM de servidor casi siempre— y son
//! tablas con firma y checksum, cada una con su formato. El device tree es lo
//! de ARM y RISC-V embebido: **un arbol**, con nodos que tienen propiedades y
//! nombres de texto.
//!
//! Sin esto, una placa que no trae ACPI es una maquina sobre la que el kernel
//! no sabe nada: ni cuantos nucleos hay, ni donde esta el controlador de
//! interrupciones, ni donde esta su propio cable. Arranca igual —la direccion
//! del UART se hereda horneada— pero queda ciega, girando en el serie y sin
//! poder reclamar un solo nucleo.
//!
//! # Como esta armado
//!
//! Tres bloques, y **todo en big-endian** aunque la maquina no lo sea: el
//! formato viene de PowerPC y se quedo asi.
//!
//! ```text
//!   encabezado -> donde empieza cada bloque y cuanto mide
//!   estructura -> una secuencia de fichas: "abre nodo", "propiedad", "cierra"
//!   cadenas    -> los nombres de las propiedades, uno atras de otro
//! ```
//!
//! Una propiedad no lleva su nombre: lleva un desplazamiento al bloque de
//! cadenas. Asi el mismo nombre —`compatible` aparece en cada nodo— se guarda
//! una sola vez.
//!
//! # Lo que hay que leer del arbol y no suponer
//!
//! Cuanto mide una direccion **lo dice el nodo padre**, en `#address-cells`, y
//! cuanto mide un tamano en `#size-cells`. Un `reg` es una lista de pares
//! (direccion, tamano) medidos en esas unidades. Dar por sentado que son dos
//! celdas de 32 bits cada una anda en QEMU y falla en la mitad de las placas
//! reales, que es exactamente la clase de suposicion que P4 viene a sacar.

use crate::acpi::{Cpu, Hardware, Interrupts, Iommu, Pcie, Psci, Serial};

/// Lo primero que tiene todo device tree. Si no esta, no es uno.
const MAGIC: u32 = 0xd00d_feed;

// Las fichas del bloque de estructura.
const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROP: u32 = 3;
const NOP: u32 = 4;
const END: u32 = 9;

/// Hasta cuantos nodos de profundidad se recorre.
///
/// El arbol de una maquina real tiene cuatro o cinco niveles. El tope existe
/// para que un blob corrupto no haga un recorrido infinito, no porque haga
/// falta el espacio.
const MAX_DEPTH: usize = 24;

const MAX_CPUS: usize = 256;
static mut CPUS: [Cpu; MAX_CPUS] = [Cpu { id: 0, uid: 0, enabled: false }; MAX_CPUS];

/// Cuantos nombres de nodo se recuerdan para informarlos.
///
/// Es el equivalente de las firmas de ACPI: que exista en la maquina algo que
/// este kernel todavia no sabe leer es mas util que callarlo (P4).
const MAX_NAMES: usize = 64;
static mut NAMES: [[u8; 4]; MAX_NAMES] = [[0; 4]; MAX_NAMES];

/// Un numero de 32 bits en big-endian.
///
/// # Safety
///
/// `at` tiene que estar adentro del blob.
unsafe fn be32(at: u64) -> u32 {
    u32::from_be_bytes([
        core::ptr::read_volatile(at as *const u8),
        core::ptr::read_volatile((at + 1) as *const u8),
        core::ptr::read_volatile((at + 2) as *const u8),
        core::ptr::read_volatile((at + 3) as *const u8),
    ])
}

/// Un valor de `cells` celdas de 32 bits, que es como el arbol guarda las
/// direcciones y los tamanos.
unsafe fn cells_at(at: u64, cells: u32) -> u64 {
    let mut v = 0u64;
    for i in 0..cells.min(2) {
        v = (v << 32) | be32(at + (i as u64) * 4) as u64;
    }
    v
}

/// Compara el contenido de una propiedad con un texto.
///
/// `compatible` puede traer **varias** cadenas pegadas, cada una terminada en
/// cero: una placa dice "soy esto, y tambien soy compatible con esto otro". Se
/// las mira todas, porque quedarse con la primera es como no mirar.
unsafe fn matches(at: u64, len: u32, want: &str) -> bool {
    let mut i = 0u32;
    while i < len {
        let mut k = 0u32;
        while i + k < len && k < want.len() as u32 {
            if core::ptr::read_volatile((at + (i + k) as u64) as *const u8) != want.as_bytes()[k as usize] {
                break;
            }
            k += 1;
        }
        // Coincide entera y termina donde tiene que terminar.
        if k == want.len() as u32
            && (i + k == len
                || core::ptr::read_volatile((at + (i + k) as u64) as *const u8) == 0)
        {
            return true;
        }
        // Al final de esta cadena, y a la siguiente.
        while i < len && core::ptr::read_volatile((at + i as u64) as *const u8) != 0 {
            i += 1;
        }
        i += 1;
    }
    false
}

/// Si el nombre de la propiedad, que vive en el bloque de cadenas, es ese.
unsafe fn named(strings: u64, off: u32, want: &str) -> bool {
    for (k, b) in want.as_bytes().iter().enumerate() {
        if core::ptr::read_volatile((strings + off as u64 + k as u64) as *const u8) != *b {
            return false;
        }
    }
    core::ptr::read_volatile((strings + off as u64 + want.len() as u64) as *const u8) == 0
}

/// Lo que se sabe del nodo que se esta recorriendo.
#[derive(Clone, Copy)]
struct Node {
    /// Cuantas celdas mide una direccion adentro de este nodo, y cuantas un
    /// tamano. Se heredan del padre si el nodo no las redefine.
    address_cells: u32,
    size_cells: u32,
    /// El `reg` de este nodo, si lo tiene: donde estan sus bytes y cuantos.
    reg: Option<(u64, u32)>,
    /// Las clases con las que se identifica, sin interpretar todavia.
    compatible: Option<(u64, u32)>,
    /// Su `interrupts`, si lo tiene.
    interrupts: Option<(u64, u32)>,
    /// Si el nodo se llama como uno que nos interesa por el nombre y no por la
    /// clase — `cpu@1`, `psci`.
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Other,
    Cpu,
    Psci,
}

/// Recorre el arbol y arma lo mismo que arma `acpi::read`.
///
/// Devuelve el `Hardware` a medio llenar si el blob esta cortado: lo que se
/// pudo leer vale, y lo que no aparece como ausente en vez de como cero.
///
/// # Safety
///
/// `blob` tiene que apuntar a un device tree aplanado, y su memoria estar
/// mapeada.
pub unsafe fn read(blob: u64) -> Hardware {
    let mut hw = Hardware::blank();
    if blob == 0 || be32(blob) != MAGIC {
        return hw;
    }

    let struct_at = blob + be32(blob + 8) as u64;
    let strings_at = blob + be32(blob + 12) as u64;
    let struct_len = be32(blob + 36) as u64;

    // La raiz define las unidades por omision. Dos celdas es lo habitual, pero
    // se lee del arbol apenas aparezca.
    let mut stack = [Node {
        address_cells: 2,
        size_cells: 1,
        reg: None,
        compatible: None,
        interrupts: None,
        kind: Kind::Other,
    }; MAX_DEPTH];
    let mut depth = 0usize;

    let mut n_cpus = 0usize;
    let mut n_names = 0usize;
    let mut gic_version = 0u32;

    let mut at = struct_at;
    let end = struct_at + struct_len;

    while at + 4 <= end {
        let token = be32(at);
        at += 4;

        match token {
            NOP => {}
            END => break,

            BEGIN_NODE => {
                // El nombre, terminado en cero y rellenado hasta multiplo de 4.
                let name_at = at;
                let mut len = 0u64;
                while at + len < end
                    && core::ptr::read_volatile((name_at + len) as *const u8) != 0
                {
                    len += 1;
                }
                at += (len + 4) & !3;

                if depth + 1 >= MAX_DEPTH {
                    return hw;
                }
                // Hereda del padre, que es lo que manda el formato.
                let parent = stack[depth];
                depth += 1;
                stack[depth] = Node {
                    address_cells: parent.address_cells,
                    size_cells: parent.size_cells,
                    reg: None,
                    compatible: None,
                    interrupts: None,
                    kind: name_kind(name_at, len),
                };

                if n_names < MAX_NAMES && len > 0 {
                    let mut short = [0u8; 4];
                    for (k, s) in short.iter_mut().enumerate() {
                        if (k as u64) < len {
                            *s = core::ptr::read_volatile((name_at + k as u64) as *const u8);
                        }
                    }
                    (&mut *core::ptr::addr_of_mut!(NAMES))[n_names] = short;
                    n_names += 1;
                }
            }

            END_NODE => {
                if depth == 0 {
                    break;
                }
                // Al cerrar es cuando el nodo esta completo: recien aca se sabe
                // que tiene `compatible` **y** `reg`, que pueden venir en
                // cualquier orden.
                let node = stack[depth];
                let parent = stack[depth - 1];
                interpret(&node, &parent, &mut hw, &mut n_cpus, &mut gic_version);
                depth -= 1;
            }

            PROP => {
                if at + 8 > end {
                    break;
                }
                let len = be32(at);
                let name_off = be32(at + 4);
                let value = at + 8;
                at = value + ((len as u64 + 3) & !3);

                let node = &mut stack[depth];
                if named(strings_at, name_off, "#address-cells") && len >= 4 {
                    node.address_cells = be32(value);
                } else if named(strings_at, name_off, "#size-cells") && len >= 4 {
                    node.size_cells = be32(value);
                } else if named(strings_at, name_off, "reg") {
                    node.reg = Some((value, len));
                } else if named(strings_at, name_off, "compatible") {
                    node.compatible = Some((value, len));
                } else if named(strings_at, name_off, "interrupts") {
                    node.interrupts = Some((value, len));
                } else if named(strings_at, name_off, "method") && node.kind == Kind::Psci {
                    // `hvc` o `smc`: con cual se le habla al nivel de abajo.
                    hw.psci = Some(Psci { use_hvc: matches(value, len, "hvc") });
                }
            }

            _ => break,
        }
    }

    hw.cpus = core::slice::from_raw_parts(core::ptr::addr_of!(CPUS) as *const Cpu, n_cpus);
    hw.signatures =
        core::slice::from_raw_parts(core::ptr::addr_of!(NAMES) as *const [u8; 4], n_names);
    if let Some(i) = hw.interrupts.as_mut() {
        i.version = gic_version;
    }
    hw
}

/// Los nodos que se reconocen por el nombre y no por la clase.
unsafe fn name_kind(at: u64, len: u64) -> Kind {
    let starts = |want: &str| -> bool {
        if len < want.len() as u64 {
            return false;
        }
        for (k, b) in want.as_bytes().iter().enumerate() {
            if core::ptr::read_volatile((at + k as u64) as *const u8) != *b {
                return false;
            }
        }
        true
    };
    // `cpu@0`, `cpu@1`... pero no `cpus`, que es el nodo que los agrupa.
    if starts("cpu@") {
        Kind::Cpu
    } else if starts("psci") {
        Kind::Psci
    } else {
        Kind::Other
    }
}

/// Traduce un nodo ya completo a lo que el kernel entiende.
unsafe fn interpret(
    node: &Node,
    parent: &Node,
    hw: &mut Hardware,
    n_cpus: &mut usize,
    gic_version: &mut u32,
) {
    // Un nucleo. Su `reg` es el numero con el que la maquina lo nombra, que en
    // aarch64 es el MPIDR — lo mismo que hace falta para arrancarlo con PSCI.
    if node.kind == Kind::Cpu {
        if let Some((at, len)) = node.reg {
            if len >= 4 && *n_cpus < MAX_CPUS {
                let id = cells_at(at, parent.address_cells);
                (&mut *core::ptr::addr_of_mut!(CPUS))[*n_cpus] =
                    Cpu { id, uid: *n_cpus as u32, enabled: true };
                *n_cpus += 1;
            }
        }
        return;
    }

    let Some((c_at, c_len)) = node.compatible else { return };

    // El controlador de interrupciones. Las dos versiones que hay dicen su
    // nombre distinto, y la version importa: el codigo que lo programa no es
    // el mismo.
    for (want, version) in [("arm,gic-v3", 3u32), ("arm,cortex-a15-gic", 2), ("arm,gic-400", 2)] {
        if matches(c_at, c_len, want) {
            if let Some((at, len)) = node.reg {
                let cells = parent.address_cells + parent.size_cells;
                let address = cells_at(at, parent.address_cells);
                // El segundo rango: en GICv2 es la interfaz de CPU, en GICv3 el
                // redistribuidor. Solo si el `reg` trae los dos.
                let second = if len as u32 >= cells * 8 {
                    cells_at(at + (cells as u64) * 4, parent.address_cells)
                } else {
                    0
                };
                hw.interrupts = Some(Interrupts {
                    kind: "gic",
                    address,
                    version,
                    cpu_interface: second,
                });
                *gic_version = version;
            }
            return;
        }
    }

    // El puerto serie. Con esto el kernel deja de creerle a la direccion
    // horneada tambien en una placa sin ACPI (deuda 2).
    if matches(c_at, c_len, "arm,pl011") {
        if let Some((at, _)) = node.reg {
            hw.serial = Some(Serial {
                address: cells_at(at, parent.address_cells),
                gsi: node.interrupts.map(|(i, l)| spi_of(i, l)).unwrap_or(0),
            });
        }
        return;
    }

    // Donde se configura PCIe. El `reg` del nodo es la ventana ECAM.
    if matches(c_at, c_len, "pci-host-ecam-generic") {
        if let Some((at, _)) = node.reg {
            let base = cells_at(at, parent.address_cells);
            let size = cells_at(at + (parent.address_cells as u64) * 4, parent.size_cells);
            // Cada bus ocupa 1 MiB de espacio de configuracion. La cuenta va en
            // 64 bits hasta el final: 256 buses no entran en el byte donde se
            // guarda el ultimo, y recortarlo antes de restarle uno daba cero —
            // o sea "un solo bus" justo en la maquina que tiene todos.
            let buses = (size >> 20).clamp(1, 256);
            hw.pcie = Some(Pcie {
                base,
                segment: 0,
                bus_start: 0,
                bus_end: (buses - 1) as u8,
            });
        }
        return;
    }

    // El IOMMU (D8).
    if matches(c_at, c_len, "arm,smmu-v3") {
        if let Some((at, _)) = node.reg {
            hw.iommu = Some(Iommu {
                kind: "smmuv3",
                base: cells_at(at, parent.address_cells),
                address_width: 0,
            });
        }
    }
}

/// El numero global de una interrupcion declarada en el arbol.
///
/// Vienen de a tres celdas: si es privada del nucleo o compartida, el numero, y
/// como se dispara. Un numero compartido (SPI) arranca en 32 — los 32 de abajo
/// se los reserva la arquitectura para las privadas de cada nucleo, y el arbol
/// los cuenta desde cero.
unsafe fn spi_of(at: u64, len: u32) -> u32 {
    if len < 12 {
        return 0;
    }
    let is_spi = be32(at) == 0;
    let number = be32(at + 4);
    if is_spi {
        number + 32
    } else {
        number + 16
    }
}
