//! Lo justo de CBOR para armar un pedido (D6).
//!
//! El protocolo del kernel es CBOR y nunca JSON, porque por ahi viajan codigo
//! maquina y volcados. Del lado del blob hace falta **escribirlo**, no leerlo:
//! los pedidos los arma el, y de la respuesta le alcanza con saber cuanto
//! ocupa — quien la interpreta de verdad es el agente del otro lado del cable.
//!
//! Por eso esto es un escritor y no un codec. Todo lo que un pedido necesita son
//! cuatro formas: un arreglo, un mapa, un entero y un texto.

/// Arma un mensaje CBOR en un buffer prestado.
pub struct Writer<'a> {
    buf: &'a mut [u8],
    used: usize,
    /// Si no entro. Se anota en vez de cortar: un mensaje a medias es peor que
    /// ninguno, asi que al final `done` devuelve cero y el que llama decide.
    spilled: bool,
}

impl<'a> Writer<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, used: 0, spilled: false }
    }

    fn byte(&mut self, b: u8) {
        match self.buf.get_mut(self.used) {
            Some(slot) => {
                *slot = b;
                self.used += 1;
            }
            None => self.spilled = true,
        }
    }

    /// La cabeza de cualquier valor: tres bits que dicen de que tipo es, y el
    /// numero. Si el numero no entra en los cinco bits que quedan, se dice con
    /// cuantos bytes viene y siguen esos bytes.
    fn head(&mut self, major: u8, value: u64) {
        let tag = major << 5;
        if value < 24 {
            self.byte(tag | value as u8);
        } else if value < 0x100 {
            self.byte(tag | 24);
            self.byte(value as u8);
        } else if value < 0x1_0000 {
            self.byte(tag | 25);
            self.byte((value >> 8) as u8);
            self.byte(value as u8);
        } else if value < 0x1_0000_0000 {
            self.byte(tag | 26);
            for shift in [24, 16, 8, 0] {
                self.byte((value >> shift) as u8);
            }
        } else {
            self.byte(tag | 27);
            for shift in [56, 48, 40, 32, 24, 16, 8, 0] {
                self.byte((value >> shift) as u8);
            }
        }
    }

    pub fn array(&mut self, items: u64) {
        self.head(4, items);
    }

    pub fn map(&mut self, pairs: u64) {
        self.head(5, pairs);
    }

    pub fn uint(&mut self, value: u64) {
        self.head(0, value);
    }

    pub fn text(&mut self, s: &str) {
        self.head(3, s.len() as u64);
        for b in s.as_bytes() {
            self.byte(*b);
        }
    }

    /// Cuantos bytes ocupa lo armado, o cero si no entro.
    pub fn done(&self) -> usize {
        if self.spilled {
            0
        } else {
            self.used
        }
    }
}
