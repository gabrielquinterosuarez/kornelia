//! Las tablas de ACPI: nucleos, controlador de interrupciones y PCIe.
//!
//! El mapa de memoria dice que RAM hay, pero no cuantos nucleos tiene la
//! maquina, ni donde esta el controlador de interrupciones, ni donde se
//! configuran los dispositivos PCIe. Eso vive aca.
//!
//! # Por que va en `kernel-core`
//!
//! ACPI es un formato de la maquina, no del entorno de arranque ni de la
//! arquitectura (D24). Quien *encuentra* el RSDP depende de como se arranco;
//! recorrer las tablas es identico en x86_64 y en aarch64 — lo que cambia es lo
//! que hay adentro: un x86 describe APICs y un ARM describe GICs, y las dos
//! cosas se normalizan al mismo vocabulario.
//!
//! # Como esta armado
//!
//! El RSDP apunta al XSDT, que es una tabla con una lista de punteros a las
//! demas. Cada tabla arranca con el mismo encabezado de 36 bytes, cuyos
//! primeros cuatro son una firma de texto: `APIC` es la MADT, que trae los
//! nucleos, y `MCFG` trae PCIe.
//!
//! Todas llevan un checksum sobre su largo entero, y se verifica siempre: un
//! puntero que casualmente empiece con la firma correcta es mas facil de lo que
//! parece cuando se recorre memoria cruda.

use crate::tables::Acpi as Rsdp;

/// Cuantos nucleos se pueden anotar.
const MAX_CPUS: usize = 256;
/// Cuantas firmas de tabla se recuerdan para informarlas.
const MAX_FIRMAS: usize = 64;

/// Un nucleo, tal como lo describe la maquina.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub struct Cpu {
    /// El numero con el que la maquina lo nombra: APIC ID en x86_64, MPIDR en
    /// aarch64. Es lo que va a hacer falta para arrancarlo con `core.claim`.
    pub id: u64,
    /// El identificador que usa ACPI, que puede no ser el mismo.
    pub uid: u32,
    /// Si la maquina dice que se puede usar. Un nucleo deshabilitado esta
    /// fisicamente presente pero el firmware no lo ofrece.
    pub enabled: bool,
}

/// Donde esta el controlador de interrupciones.
#[derive(Clone, Copy)]
pub struct Interrupts {
    /// `apic` en x86_64, `gic` en aarch64. Se informa como texto porque el
    /// protocolo no lleva nombres de arquitectura horneados (D3).
    pub kind: &'static str,
    /// Direccion fisica de sus registros.
    pub address: u64,
    /// Version, cuando la maquina la informa. Cero si no.
    pub version: u32,
    /// En aarch64, la parte del GIC que mira cada nucleo. Cero en x86_64.
    pub cpu_interface: u64,
}

/// Como se le pide al firmware que arranque un nucleo, en aarch64.
///
/// PSCI es la interfaz estandar de ARM para eso, y se invoca con una
/// instruccion de llamada al nivel de abajo. **Cual de las dos es depende de la
/// maquina**, y la maquina lo dice en la FADT: no se adivina (P4).
#[derive(Clone, Copy)]
pub struct Psci {
    /// `true` para `hvc` (hay un hipervisor abajo), `false` para `smc`.
    pub use_hvc: bool,
}

/// El controlador de interrupciones de dispositivos en x86_64.
///
/// El APIC local atiende a **un** nucleo; este otro es el que recibe las
/// interrupciones de los aparatos y decide a que nucleo mandarlas.
#[derive(Clone, Copy)]
pub struct IoApic {
    pub address: u64,
    /// El primer numero de interrupcion global que atiende.
    pub gsi_base: u32,
}

/// Una interrupcion vieja de PC que en esta maquina esta en otro numero.
///
/// Los numeros de interrupcion de la PC original (el teclado en la 1, el serie
/// en la 4) sobreviven como nombres, pero la maquina puede haberlos movido. La
/// tabla dice adonde.
#[derive(Clone, Copy)]
pub struct Override {
    /// El numero viejo.
    pub source: u8,
    /// El numero de verdad.
    pub gsi: u32,
}

/// Donde esta el puerto serie y por que interrupcion avisa.
///
/// Sale de la tabla SPCR, que no todas las maquinas traen. Cuando esta, es la
/// maquina diciendo donde tiene la consola en vez de que nosotros lo supongamos
/// (P4).
#[derive(Clone, Copy)]
pub struct Serial {
    pub address: u64,
    /// El numero de interrupcion, o cero si la tabla no lo informa.
    pub gsi: u32,
}

/// Donde se configura PCIe.
///
/// El espacio de configuracion esta mapeado en memoria: escribirle a la
/// direccion correcta dentro de este rango es como se lee y programa cualquier
/// dispositivo del bus, incluidos sus BARs.
#[derive(Clone, Copy)]
pub struct Pcie {
    pub base: u64,
    pub segment: u16,
    pub bus_start: u8,
    pub bus_end: u8,
}

/// Lo que se pudo leer de ACPI.
#[derive(Clone, Copy)]
pub struct Hardware {
    pub cpus: &'static [Cpu],
    pub interrupts: Option<Interrupts>,
    pub pcie: Option<Pcie>,
    /// Como arrancar los otros nucleos, si la maquina lo informa.
    pub psci: Option<Psci>,
    /// Quien recibe las interrupciones de los aparatos, en x86_64.
    pub ioapic: Option<IoApic>,
    /// Las interrupciones viejas de PC que esta maquina movio de numero.
    pub overrides: &'static [Override],
    /// Donde esta el puerto serie, si la maquina lo dice.
    pub serial: Option<Serial>,
    /// Las firmas de todas las tablas que hay, se interpreten o no. Informar
    /// que existe algo que este kernel todavia no lee es mas util que callarlo
    /// (P4).
    pub signatures: &'static [[u8; 4]],
}

impl Hardware {
    pub const fn vacio() -> Self {
        Self {
            cpus: &[],
            interrupts: None,
            pcie: None,
            psci: None,
            ioapic: None,
            overrides: &[],
            serial: None,
            signatures: &[],
        }
    }

    /// A que numero de interrupcion corresponde una de las viejas de PC.
    ///
    /// Si la maquina no la movio, el numero es el mismo. Esa es la regla de
    /// ACPI: lo que no esta en la tabla no cambio.
    pub fn gsi_of(&self, legacy: u8) -> u32 {
        for o in self.overrides {
            if o.source == legacy {
                return o.gsi;
            }
        }
        legacy as u32
    }

    /// Cuantos nucleos ofrece la maquina para usar.
    pub fn usable_cpus(&self) -> usize {
        self.cpus.iter().filter(|c| c.enabled).count()
    }
}

const MAX_OVERRIDES: usize = 32;

static mut CPUS: [Cpu; MAX_CPUS] = [Cpu { id: 0, uid: 0, enabled: false }; MAX_CPUS];
static mut OVERRIDES: [Override; MAX_OVERRIDES] =
    [Override { source: 0, gsi: 0 }; MAX_OVERRIDES];
static mut FIRMAS: [[u8; 4]; MAX_FIRMAS] = [[0; 4]; MAX_FIRMAS];

/// El encabezado que llevan todas las tablas de ACPI.
const ENCABEZADO: usize = 36;

/// # Safety
///
/// `addr` tiene que ser legible.
unsafe fn u8_en(addr: u64, off: usize) -> u8 {
    core::ptr::read_unaligned((addr as *const u8).add(off))
}
unsafe fn u16_en(addr: u64, off: usize) -> u16 {
    core::ptr::read_unaligned((addr as *const u8).add(off) as *const u16)
}
unsafe fn u32_en(addr: u64, off: usize) -> u32 {
    core::ptr::read_unaligned((addr as *const u8).add(off) as *const u32)
}
unsafe fn u64_en(addr: u64, off: usize) -> u64 {
    core::ptr::read_unaligned((addr as *const u8).add(off) as *const u64)
}

/// Lee el encabezado de una tabla y comprueba su checksum.
///
/// Devuelve (firma, largo). `None` si el checksum no cierra, que es la senal de
/// que ahi no habia una tabla.
///
/// # Safety
///
/// `addr` tiene que ser legible.
unsafe fn encabezado(addr: u64) -> Option<([u8; 4], usize)> {
    let largo = u32_en(addr, 4) as usize;
    // Una tabla mas chica que su propio encabezado no es una tabla. El techo es
    // para no recorrer memoria sin fin si el largo vino con basura.
    if largo < ENCABEZADO || largo > 1 << 20 {
        return None;
    }

    // ACPI manda que los bytes de la tabla entera sumen 0 modulo 256.
    let mut suma: u8 = 0;
    for i in 0..largo {
        suma = suma.wrapping_add(u8_en(addr, i));
    }
    if suma != 0 {
        return None;
    }

    let firma = [u8_en(addr, 0), u8_en(addr, 1), u8_en(addr, 2), u8_en(addr, 3)];
    Some((firma, largo))
}

/// Recorre las tablas de ACPI desde el RSDP.
///
/// # Safety
///
/// El RSDP tiene que estar ya verificado, y la memoria que apunta, mapeada.
pub unsafe fn read(rsdp: &Rsdp) -> Hardware {
    // El XSDT tiene punteros de 64 bits; el RSDT, de 32. Se prefiere el XSDT
    // cuando esta, que es lo que manda ACPI 2.0 en adelante.
    let (raiz, ancho) = match rsdp.xsdt {
        Some(x) if x != 0 => (x, 8usize),
        _ => (rsdp.rsdt as u64, 4usize),
    };
    if raiz == 0 {
        return Hardware::vacio();
    }

    let Some((firma, largo)) = encabezado(raiz) else {
        return Hardware::vacio();
    };
    if &firma != b"XSDT" && &firma != b"RSDT" {
        return Hardware::vacio();
    }

    let cuantas = (largo - ENCABEZADO) / ancho;

    let mut hw = Hardware::vacio();
    let mut n_cpus = 0usize;
    let mut n_firmas = 0usize;
    let mut n_over = 0usize;

    for i in 0..cuantas {
        let off = ENCABEZADO + i * ancho;
        let tabla = if ancho == 8 {
            u64_en(raiz, off)
        } else {
            u32_en(raiz, off) as u64
        };
        if tabla == 0 {
            continue;
        }

        let Some((firma, largo)) = encabezado(tabla) else {
            continue;
        };

        if n_firmas < MAX_FIRMAS {
            (&mut *core::ptr::addr_of_mut!(FIRMAS))[n_firmas] = firma;
            n_firmas += 1;
        }

        match &firma {
            b"APIC" => leer_madt(tabla, largo, &mut hw, &mut n_cpus, &mut n_over),
            b"SPCR" => hw.serial = leer_spcr(tabla, largo),
            b"MCFG" => hw.pcie = leer_mcfg(tabla, largo),
            b"FACP" => hw.psci = leer_fadt(tabla, largo),
            _ => {}
        }
    }

    hw.cpus = core::slice::from_raw_parts(core::ptr::addr_of!(CPUS) as *const Cpu, n_cpus);
    hw.overrides =
        core::slice::from_raw_parts(core::ptr::addr_of!(OVERRIDES) as *const Override, n_over);
    hw.signatures =
        core::slice::from_raw_parts(core::ptr::addr_of!(FIRMAS) as *const [u8; 4], n_firmas);
    hw
}

/// La MADT: los nucleos y el controlador de interrupciones.
///
/// Despues del encabezado viene una lista de entradas de largo variable, cada
/// una con su tipo y su largo adelante. Los tipos que importan son distintos en
/// cada arquitectura pero conviven en el mismo formato.
///
/// # Safety
///
/// `tabla` tiene que apuntar a una MADT ya verificada.
unsafe fn leer_madt(
    tabla: u64,
    largo: usize,
    hw: &mut Hardware,
    n_cpus: &mut usize,
    n_over: &mut usize,
) {
    // En x86 el encabezado de la MADT trae la direccion del APIC local. En ARM
    // ese campo esta pero no se usa: el GIC llega en una entrada aparte.
    let apic_local = u32_en(tabla, 36) as u64;

    let cpus = &mut *core::ptr::addr_of_mut!(CPUS);
    let mut off = ENCABEZADO + 8;

    while off + 2 <= largo {
        let tipo = u8_en(tabla, off);
        let len = u8_en(tabla, off + 1) as usize;
        // Una entrada de largo cero haria girar el bucle para siempre.
        if len < 2 || off + len > largo {
            break;
        }

        match tipo {
            // 0: APIC local de x86. El nucleo esta si el bit 0 de las banderas
            // esta prendido, o si es "capaz de estar en linea" (bit 1).
            0 => {
                if *n_cpus < MAX_CPUS && len >= 8 {
                    let banderas = u32_en(tabla, off + 4);
                    cpus[*n_cpus] = Cpu {
                        uid: u8_en(tabla, off + 2) as u32,
                        id: u8_en(tabla, off + 3) as u64,
                        enabled: banderas & 0b11 != 0,
                    };
                    *n_cpus += 1;
                }
                if hw.interrupts.is_none() {
                    hw.interrupts = Some(Interrupts {
                        kind: "apic",
                        address: apic_local,
                        version: 0,
                        cpu_interface: 0,
                    });
                }
            }

            // 1: el controlador que recibe las interrupciones de los aparatos.
            1 => {
                if hw.ioapic.is_none() && len >= 12 {
                    hw.ioapic = Some(IoApic {
                        address: u32_en(tabla, off + 4) as u64,
                        gsi_base: u32_en(tabla, off + 8),
                    });
                }
            }

            // 2: una interrupcion vieja de PC que esta maquina movio de numero.
            2 => {
                if *n_over < MAX_OVERRIDES && len >= 10 {
                    (&mut *core::ptr::addr_of_mut!(OVERRIDES))[*n_over] = Override {
                        source: u8_en(tabla, off + 3),
                        gsi: u32_en(tabla, off + 4),
                    };
                    *n_over += 1;
                }
            }

            // 9: x2APIC, la version de x86 para maquinas con muchos nucleos.
            9 => {
                if *n_cpus < MAX_CPUS && len >= 16 {
                    let banderas = u32_en(tabla, off + 8);
                    cpus[*n_cpus] = Cpu {
                        id: u32_en(tabla, off + 4) as u64,
                        uid: u32_en(tabla, off + 12),
                        enabled: banderas & 0b11 != 0,
                    };
                    *n_cpus += 1;
                }
            }

            // 11: GICC, el nucleo del lado ARM. El identificador que sirve para
            // arrancarlo es el MPIDR, no el numero de interfaz.
            11 => {
                if *n_cpus < MAX_CPUS && len >= 76 {
                    let banderas = u32_en(tabla, off + 12);
                    cpus[*n_cpus] = Cpu {
                        id: u64_en(tabla, off + 68),
                        uid: u32_en(tabla, off + 8),
                        enabled: banderas & 0b1 != 0,
                    };
                    *n_cpus += 1;
                }
                // La parte del GIC que mira cada nucleo viene en la misma
                // entrada, y es la misma para todos.
                if let Some(i) = &mut hw.interrupts {
                    if i.cpu_interface == 0 && len >= 40 {
                        i.cpu_interface = u64_en(tabla, off + 32);
                    }
                }
            }

            // 12: GICD, el distribuidor. Es el equivalente ARM del APIC.
            12 => {
                if len >= 24 {
                    let cpu = hw.interrupts.map(|i| i.cpu_interface).unwrap_or(0);
                    hw.interrupts = Some(Interrupts {
                        kind: "gic",
                        address: u64_en(tabla, off + 8),
                        version: u8_en(tabla, off + 20) as u32,
                        cpu_interface: cpu,
                    });
                }
            }

            _ => {}
        }
        off += len;
    }
}

/// La FADT: de aca sale como arrancar los otros nucleos en aarch64.
///
/// El campo son dos banderas en el offset 129: si la maquina cumple PSCI, y con
/// cual de las dos instrucciones hay que llamarlo.
///
/// # Safety
///
/// `tabla` tiene que apuntar a una FADT ya verificada.
unsafe fn leer_fadt(tabla: u64, largo: usize) -> Option<Psci> {
    // Las FADT viejas son mas cortas y no llegan a tener este campo.
    if largo < 131 {
        return None;
    }
    let banderas = u16_en(tabla, 129);
    if banderas & 0b1 == 0 {
        // La maquina no dice cumplir PSCI: no se inventa que si.
        return None;
    }
    Some(Psci { use_hvc: banderas & 0b10 != 0 })
}

/// La SPCR: donde esta el puerto serie y por que interrupcion avisa.
///
/// Es la maquina diciendo donde tiene su consola. Sin esta tabla hay que
/// suponerlo, y suponer es justo lo que P4 evita.
///
/// # Safety
///
/// `tabla` tiene que apuntar a una SPCR ya verificada.
unsafe fn leer_spcr(tabla: u64, largo: usize) -> Option<Serial> {
    // La direccion viene adentro de una estructura generica de 12 bytes que
    // empieza en el 40; los primeros 4 dicen de que espacio es y como se accede,
    // y la direccion misma arranca en el 44.
    if largo < 58 {
        return None;
    }
    Some(Serial { address: u64_en(tabla, 44), gsi: u32_en(tabla, 54) })
}

/// La MCFG: donde esta el espacio de configuracion de PCIe.
///
/// # Safety
///
/// `tabla` tiene que apuntar a una MCFG ya verificada.
unsafe fn leer_mcfg(tabla: u64, largo: usize) -> Option<Pcie> {
    // Despues del encabezado hay 8 bytes reservados y ahi arrancan las
    // entradas, de 16 bytes cada una.
    let off = ENCABEZADO + 8;
    if off + 16 > largo {
        return None;
    }
    Some(Pcie {
        base: u64_en(tabla, off),
        segment: u16_en(tabla, off + 8),
        bus_start: u8_en(tabla, off + 10),
        bus_end: u8_en(tabla, off + 11),
    })
}
