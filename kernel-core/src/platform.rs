//! La frontera de portabilidad.
//!
//! Todo lo que una arquitectura debe proveer vive en este trait. El resto del
//! kernel no sabe sobre qué silicio corre.

use core::fmt::{self, Write};

pub trait Platform {
    /// Nombre de la arquitectura. El agente lo recibe en `describe`; el
    /// protocolo nunca lleva nombres de registros horneados (D3).
    const ARCH: &'static str;

    /// Emite un byte por el cordón umbilical (D5: el UART nunca se abandona).
    fn uart_write_byte(&mut self, b: u8);

    /// Detiene este núcleo para siempre, con el menor consumo posible.
    fn park(&mut self) -> !;
}

/// Escritor de texto sobre el cordón umbilical.
///
/// Es la única concesión a la legibilidad humana en todo el kernel, y existe
/// solo para el arranque y la depuración: el canal del agente es CBOR binario.
pub struct Cordon<'a, P: Platform> {
    p: &'a mut P,
}

impl<'a, P: Platform> Cordon<'a, P> {
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
}

impl<'a, P: Platform> Write for Cordon<'a, P> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.as_bytes() {
            self.p.uart_write_byte(*b);
        }
        Ok(())
    }
}
