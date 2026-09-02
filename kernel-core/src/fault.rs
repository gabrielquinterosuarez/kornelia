//! Los faults, como datos (P5).
//!
//! En un sistema operativo normal un page fault es un SIGSEGV y el proceso se
//! muere. Aca es un valor: causa, donde paso, que direccion se quiso tocar y el
//! estado entero de los registros.
//!
//! # Por que importa tanto
//!
//! El agente escribe codigo maquina y lo sube. Va a estar mal seguido — es un
//! generador estocastico de codigo. Si el error se lleva puesta la maquina, el
//! agente no se entera de nada y no puede corregir. Si el error vuelve como un
//! dato, es una iteracion mas.
//!
//! # Los registros no se nombran aca (D3)
//!
//! x86_64 tiene RAX, aarch64 tiene X0-X30, RISC-V tiene x0-x31. Este modulo no
//! sabe ninguno: cada arquitectura publica sus nombres en `Platform::REGISTERS`
//! y los valores vienen en ese mismo orden. La maquina se describe a si misma
//! (P4).

/// Que paso, traducido a algo que signifique lo mismo en toda arquitectura.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub enum Cause {
    /// Un `int3` / `brk`: puesto a proposito.
    Breakpoint,
    /// Division por cero.
    DivideByZero,
    /// La instruccion no existe.
    InvalidOpcode,
    /// Se toco una direccion que no esta mapeada, o sin permiso.
    PageFault,
    /// La instruccion no se pudo ni buscar.
    InstructionFetch,
    /// Un acceso desalineado donde no se permite.
    Alignment,
    /// Se hizo algo que el nivel de privilegio no permite.
    Protection,
    /// Un fault mientras se atendia otro fault.
    Double,
    /// La maquina informo algo que este kernel no sabe traducir. El numero
    /// crudo viaja igual en `raw`, sin inventarle significado (P4).
    Unknown,
}

impl Cause {
    /// El identificador que viaja por el protocolo (D6). Ingles y estable.
    pub fn code(&self) -> &'static str {
        match self {
            Cause::Breakpoint => "breakpoint",
            Cause::DivideByZero => "divide-by-zero",
            Cause::InvalidOpcode => "invalid-opcode",
            Cause::PageFault => "page-fault",
            Cause::InstructionFetch => "instruction-fetch",
            Cause::Alignment => "alignment",
            Cause::Protection => "protection",
            Cause::Double => "double-fault",
            Cause::Unknown => "unknown",
        }
    }

    /// Si tiene sentido seguir ejecutando despues de esto.
    ///
    /// Un breakpoint es un alto pedido: se sigue en la instruccion de al lado.
    /// Lo demas volveria a fallar en la misma instruccion, para siempre.
    pub fn resumable(&self) -> bool {
        matches!(self, Cause::Breakpoint)
    }
}

/// Un fault, tal como se captura.
#[derive(Clone, Copy)]
pub struct Fault {
    pub cause: Cause,
    /// El numero que uso la maquina: vector en x86_64, EC del ESR en aarch64.
    /// Se informa crudo para no perder lo que la maquina dijo (P4).
    pub raw: u64,
    /// Un segundo numero que algunas arquitecturas adjuntan (el codigo de error
    /// de x86, el ISS del ESR en ARM). Cero si no hubo.
    pub detail: u64,
    /// Donde estaba ejecutando cuando paso.
    pub pc: u64,
    /// La direccion que se intento tocar, cuando la causa la tiene.
    pub address: Option<u64>,
    /// Los valores de los registros, en el orden de `Platform::REGISTERS`.
    pub regs: &'static [u64],
}

impl Fault {
    /// Un fault vacio, para poder tener estaticos sin asignador.
    pub const fn none() -> Self {
        Self {
            cause: Cause::Unknown,
            raw: 0,
            detail: 0,
            pc: 0,
            address: None,
            regs: &[],
        }
    }
}

/// Escribe un fault en texto, para el cordon umbilical.
///
/// Recibe los nombres por separado porque este modulo no conoce ninguno (D3):
/// los pone la arquitectura. Escribe sobre cualquier `Write`, y no sobre
/// `Umbilical`, porque desde adentro de un handler no hay una `Platform` a mano
/// — ahi solo se tiene el UART pelado.
/// Un candado para que dos nucleos que fallan a la vez no entrelacen el texto.
///
/// El estado del fault ya es por nucleo, asi que nada se corrompe — lo que se
/// pierde es la posibilidad de **leerlo**: dos reportes intercalados byte a byte
/// son dos reportes ilegibles, y este texto existe justo para el momento en que
/// algo salio mal y no hay otra forma de mirar.
///
/// Es un candado de girar, y esta bien que lo sea: quien lo espera ya se estaba
/// deteniendo. No se libera nunca si el que lo tiene se cuelga, y tambien esta
/// bien: si un nucleo se colgo adentro del reporte de un fault, que el otro no
/// escriba encima es lo que mas ayuda a entender que paso.
static PRINTING: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub fn report(f: &Fault, names: &[&str], out: &mut impl core::fmt::Write) {
    use core::sync::atomic::Ordering;
    while PRINTING
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    let _ = writeln!(out, "FAULT: {} (crudo {}, detalle {:#x})\r", f.cause.code(), f.raw, f.detail);
    let _ = writeln!(out, "  pc {:#018x}\r", f.pc);
    if let Some(a) = f.address {
        let _ = writeln!(out, "  direccion tocada {a:#018x}\r");
    }

    // De a cuatro por renglon: por serie a 115200, treinta y tres renglones de
    // un registro cada uno es una espera perceptible.
    for (i, (n, v)) in names.iter().zip(f.regs).enumerate() {
        if i % 4 == 0 {
            let _ = write!(out, "  ");
        }
        let _ = write!(out, "{n:>6}={v:016x} ");
        if i % 4 == 3 {
            let _ = write!(out, "\r\n");
        }
    }
    if f.regs.len() % 4 != 0 {
        let _ = write!(out, "\r\n");
    }

    PRINTING.store(false, Ordering::Release);
}

/// Como termino un `exec`.
///
/// Las dos salidas son datos: que el codigo del agente falle no es una
/// excepcion al funcionamiento normal, es el funcionamiento normal (P5). El
/// agente es un generador estocastico de codigo maquina; va a fallar seguido, y
/// cada falla tiene que volver como algo que se pueda leer y corregir.
#[derive(Clone, Copy)]
pub struct Outcome {
    /// Si termino por un fault en vez de volver solo.
    pub faulted: bool,
    /// El estado de los registros al terminar, en el orden de
    /// `Platform::REGISTERS`.
    pub regs: &'static [u64],
    /// Que paso, si fue un fault.
    pub fault: Option<Fault>,
    /// Si no termino solo ni por un fault, sino porque **se lo interrumpio**.
    ///
    /// Es como se recupera un nucleo cuyo codigo no vuelve: se le manda una
    /// interrupcion y el handler lo desvia al mismo punto de recuperacion que
    /// usa un fault. Se distingue del fault a proposito — el codigo no hizo nada
    /// mal, se lo cortaron, y eso es informacion distinta para el que depura.
    pub cancelled: bool,
}
