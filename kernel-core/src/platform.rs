//! La frontera de portabilidad.
//!
//! Todo lo que una arquitectura debe proveer vive en este trait. El resto del
//! kernel no sabe sobre qué silicio corre.

use crate::fault::Fault;
use crate::machine::Machine;
use crate::paging::Mapping;
use core::fmt::{self, Write};

pub trait Platform {
    /// Nombre de la arquitectura. El agente lo recibe en `describe`; el
    /// protocolo nunca lleva nombres de registros horneados (D3).
    const ARCH: &'static str;

    /// Emite un byte por el cordón umbilical (D5: el UART nunca se abandona).
    fn uart_write_byte(&mut self, b: u8);

    /// Levanta un byte del cordón umbilical si hay alguno esperando.
    ///
    /// **No bloquea.** Devuelve `None` si no llegó nada. Es a propósito: el
    /// kernel va a tener que escuchar por el UART y por el transporte que
    /// escriba el agente al mismo tiempo (D17), y un `read` que bloquea deja
    /// sordo al otro canal.
    fn uart_read_byte(&mut self) -> Option<u8>;

    /// Detiene este núcleo para siempre, con el menor consumo posible.
    fn park(&mut self) -> !;

    /// Lo que se averiguó de la máquina durante el arranque.
    ///
    /// Devuelve `'static` a propósito: el mapa se captura en la única ventana
    /// que hay para preguntarle al firmware (D25), y desde ahí es un hecho fijo
    /// de la máquina y no algo atado a esta llamada. El núcleo no sabe si vino
    /// de UEFI, de un device tree o de una ROM de arranque (D24).
    fn machine(&self) -> Machine;

    /// Arma las tablas de páginas del kernel y las carga (D12).
    ///
    /// El **qué** mapear lo decide `paging`, que es portable; el cómo escribir
    /// una tabla es lo menos portable que hay y por eso cruza la frontera.
    ///
    /// # Safety
    ///
    /// Solo se puede llamar después de `ExitBootServices`: cambiar la
    /// traducción con el firmware todavía vivo le saca el piso a sus propias
    /// estructuras.
    unsafe fn install_page_tables(&mut self, m: &Machine) -> Result<Mapping, &'static str>;

    /// Los nombres de los registros de esta máquina, en el mismo orden en que
    /// vienen los valores de un `Fault`.
    ///
    /// El protocolo nunca lleva nombres de registros horneados (D3): x86_64
    /// tiene RAX, aarch64 tiene X0–X30, RISC-V tiene x0–x31. La máquina informa
    /// cuáles tiene (P4).
    const REGISTERS: &'static [&'static str];

    /// Instala la captura de excepciones (P5, D7).
    ///
    /// # Safety
    ///
    /// Los handlers tienen que estar mapeados y ejecutables.
    unsafe fn install_fault_handlers(&mut self) -> Result<(), &'static str>;

    /// Provoca un breakpoint a propósito.
    ///
    /// Existe para que el arranque pueda comprobar que la captura funciona en
    /// vez de suponerlo. Un breakpoint es la única excepción pensada para que
    /// se pueda seguir después.
    fn trigger_breakpoint(&mut self);

    /// El último fault capturado, si hubo alguno.
    fn last_fault(&self) -> Option<Fault>;
}

/// Escritor de texto sobre el cordón umbilical.
///
/// Es la única concesión a la legibilidad humana en todo el kernel, y existe
/// solo para el arranque y la depuración: el canal del agente es CBOR binario.
pub struct Umbilical<'a, P: Platform> {
    p: &'a mut P,
}

impl<'a, P: Platform> Umbilical<'a, P> {
    pub fn new(p: &'a mut P) -> Self {
        Self { p }
    }

    pub fn line(&mut self, s: &str) {
        let _ = self.write_str(s);
        let _ = self.write_str("\r\n");
    }

    pub fn kv(&mut self, k: &str, v: &str) {
        let _ = write!(self, "{k}: {v}\r\n");
    }

    /// Un tamaño en la unidad binaria más grande que lo represente **exacto**.
    ///
    /// Nunca redondea: un "512 MiB" que en realidad eran 511,9 sería el kernel
    /// mintiendo sobre la máquina, y todo el proyecto depende de que no lo haga
    /// (P4). Si no entra exacto en MiB, sale en KiB.
    pub fn size(&mut self, bytes: u64) {
        const KI: u64 = 1024;
        const MI: u64 = KI * KI;
        const GI: u64 = MI * KI;

        let _ = if bytes >= GI && bytes % GI == 0 {
            write!(self, "{:>6} GiB", bytes / GI)
        } else if bytes >= MI && bytes % MI == 0 {
            write!(self, "{:>6} MiB", bytes / MI)
        } else if bytes % KI == 0 {
            write!(self, "{:>6} KiB", bytes / KI)
        } else {
            write!(self, "{bytes:>6} B  ")
        };
    }
}

impl<'a, P: Platform> Write for Umbilical<'a, P> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.as_bytes() {
            self.p.uart_write_byte(*b);
        }
        Ok(())
    }
}
