//! El segundo cable del cordon umbilical: escribir en la pantalla (D5).
//!
//! # Por que existe
//!
//! Una PC moderna **no tiene puerto serie**. Sin esto, el kernel arranca en
//! hierro de verdad y no hay forma de enterarse de nada: ni si encontro la
//! memoria, ni por que fallo, ni si llego a algun lado. La pantalla es lo unico
//! que queda.
//!
//! # Y por que no contradice D4
//!
//! D4 dice que el kernel no tiene drivers salvo el UART. Esto no es un driver:
//! el firmware entrega una direccion, un tamano y un formato, y escribir ahi es
//! poner pixeles en memoria — sin interrupciones, sin DMA, sin programar nada.
//! Es la misma pregunta que el kernel le hace a la maquina para todo lo demas
//! (P4), con otra respuesta.
//!
//! Y es **solo salida**, a proposito. La entrada seguiria siendo un teclado, que
//! por USB es un stack entero; pero sobre todo el operador que este kernel
//! supone no es una persona (P3, D1). Por el canal viaja CBOR binario. La
//! entrada del agente es el cable, o el transporte que el mismo escribe (D17).
//!
//! # La regla que lo mantiene chico
//!
//! **Escribe exactamente los mismos bytes que salen por el cable.** No es una
//! interfaz mejor: es la misma linea, dibujada en vez de enviada. Asi no hay una
//! abstraccion nueva que mantener, y nada de lo que el kernel dice puede existir
//! en un lado y no en el otro.

use crate::font;
use crate::machine::Screen;

/// Blanco sobre negro, y por una razon que ahorra codigo: el firmware puede
/// entregar los canales en orden rojo-verde-azul o al reves, y de estos dos
/// colores **da igual** — todos unos y todos ceros se leen iguales en los dos
/// ordenes. Un kernel que escribiera en color tendria que mirar el formato.
const INK: u32 = 0xFFFF_FFFF;
const PAPER: u32 = 0x0000_0000;

/// La pantalla adoptada, si hay.
static mut CONSOLE: Option<Console> = None;

struct Console {
    /// Donde empieza el framebuffer, en fisicas.
    base: u64,
    /// Cuantos pixeles hay de una fila de pixeles a la siguiente.
    stride: u32,
    /// Cuantos caracteres entran a lo ancho y a lo alto.
    columns: u32,
    lines: u32,
    /// Donde va a caer el proximo caracter.
    column: u32,
    line: u32,
}

/// Toma la pantalla que informo el firmware y la deja lista para escribir.
///
/// # Safety
///
/// El framebuffer tiene que estar mapeado: se llama despues de instalar las
/// tablas de paginas, no antes.
pub unsafe fn adopt(where_it_is: Screen) {
    let columns = where_it_is.width / font::WIDTH as u32;
    let lines = where_it_is.height / font::HEIGHT as u32;
    // Una pantalla donde no entra ni un caracter no sirve de nada, y aceptarla
    // solo llevaria a dividir por cero mas adelante.
    if columns == 0 || lines == 0 {
        return;
    }
    let console = Console {
        base: where_it_is.base,
        stride: where_it_is.stride,
        columns,
        lines,
        column: 0,
        line: 0,
    };
    // Limpiarla entera: abajo puede haber quedado el logo del firmware, y texto
    // encima de un logo no se lee.
    for y in 0..where_it_is.height {
        console.paint_row(y, PAPER);
    }
    CONSOLE = Some(console);
}

/// Si hay pantalla donde escribir.
pub fn present() -> bool {
    // SAFETY: un solo nucleo escribe el banner, y despues esto no cambia.
    unsafe { CONSOLE.is_some() }
}

/// Escribe un byte, el mismo que salio por el cable.
pub fn put(b: u8) {
    // SAFETY: quien escribe es el nucleo del protocolo, de a un byte por vez.
    let console = unsafe {
        match &mut *core::ptr::addr_of_mut!(CONSOLE) {
            Some(c) => c,
            None => return,
        }
    };
    console.put(b);
}

impl Console {
    /// Pinta una fila entera de pixeles de un color.
    fn paint_row(&self, y: u32, color: u32) {
        let row = self.base + (y as u64) * (self.stride as u64) * 4;
        for x in 0..self.stride {
            // SAFETY: cae adentro del framebuffer que informo el firmware.
            unsafe {
                core::ptr::write_volatile((row + (x as u64) * 4) as *mut u32, color);
            }
        }
    }

    /// Baja un renglon, y si llego abajo vuelve arriba.
    ///
    /// **Vuelve arriba en vez de desplazar todo hacia arriba**, y no por
    /// simplicidad: desplazar obliga a **leer** el framebuffer, y leer memoria de
    /// video es lentisimo —esta mapeada sin cache— asi que cada renglon costaria
    /// varios megabytes de lecturas y el arranque se arrastraria. La historia
    /// completa igual esta del otro lado del cable; la pantalla es lo que queda
    /// cuando no hay cable.
    fn newline(&mut self) {
        self.column = 0;
        self.line += 1;
        if self.line >= self.lines {
            self.line = 0;
        }
        // Limpiar el renglon antes de escribirlo: si no, al dar la vuelta el
        // texto nuevo se mezcla con el viejo y no se entiende ninguno.
        let top = self.line * font::HEIGHT as u32;
        for y in top..top + font::HEIGHT as u32 {
            self.paint_row(y, PAPER);
        }
    }

    fn put(&mut self, b: u8) {
        match b {
            // El cable manda CR LF; en pantalla el CR solo vuelve al margen.
            b'\r' => {
                self.column = 0;
                return;
            }
            b'\n' => {
                self.newline();
                return;
            }
            _ => {}
        }
        // Lo que la tipografia no trae no se dibuja. No se reemplaza por un
        // signo raro: lo que el kernel escribe es ASCII imprimible (regla 4), asi
        // que si aparece otra cosa es un error de quien escribio, no algo que
        // haya que mostrar.
        if b < font::FIRST || b > font::LAST {
            return;
        }
        if self.column >= self.columns {
            self.newline();
        }

        let glyph = &font::GLYPHS[(b - font::FIRST) as usize];
        let left = self.column * font::WIDTH as u32;
        let top = self.line * font::HEIGHT as u32;
        for (dy, bits) in glyph.iter().enumerate() {
            let row = self.base + ((top + dy as u32) as u64) * (self.stride as u64) * 4;
            for dx in 0..font::WIDTH {
                // El bit mas alto es el pixel de la izquierda.
                let on = bits & (0x80 >> dx) != 0;
                let at = row + ((left + dx as u32) as u64) * 4;
                // SAFETY: cae adentro del framebuffer.
                unsafe {
                    core::ptr::write_volatile(at as *mut u32, if on { INK } else { PAPER });
                }
            }
        }
        self.column += 1;
    }
}
