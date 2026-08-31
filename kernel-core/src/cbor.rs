//! CBOR: el formato binario del protocolo (D6).
//!
//! # Por que CBOR y no JSON
//!
//! Por acá van a viajar código máquina y volcados de memoria. JSON obligaría a
//! base64 (un tercio más de bytes) y a escapar strings. CBOR manda bytes crudos
//! y lo parsea cualquier lenguaje.
//!
//! # Como se sabe donde termina un mensaje
//!
//! No hay capa de framing, y es a propósito: **CBOR ya es autodelimitado**. Un
//! decodificador sabe dónde termina un item sin que nadie le diga el largo.
//! Agregar un prefijo de longitud duplicaría información que la codificación ya
//! lleva, y un prefijo que no coincida con el contenido sería un modo de falla
//! nuevo que hoy no existe.
//!
//! Por eso `scan` puede decir "todavía faltan bytes" sobre un buffer a medio
//! llenar: es lo que permite ir acumulando lo que llega por el UART de a un byte
//! y darse cuenta solo de cuándo el mensaje está entero.
//!
//! # Lo que no se soporta
//!
//! Los largos indefinidos (`0x1f`) se rechazan. Existen para emitir sin saber
//! de antemano cuánto vas a mandar, y acá siempre se sabe. Aceptarlos sería
//! código de más para un caso que este protocolo no genera.

/// El tamaño mas grande que puede tener un encabezado: 1 byte + 8 de valor.
const MAX_HEAD: usize = 9;

/// Hasta donde se permite anidar. Un mensaje del protocolo no pasa de tres o
/// cuatro niveles; el limite existe para que un mensaje hostil no haga
/// recursion infinita.
const MAX_DEPTH: u32 = 16;

// ---------------------------------------------------------------------------
// Escritura
// ---------------------------------------------------------------------------

/// Escribe CBOR en un buffer prestado.
///
/// Si no entra, **no entra en panico y no escribe fuera**: prende una bandera y
/// sigue contando. Quien termina pregunta con `finish()`. Un kernel que se cae
/// porque una respuesta no entro en el buffer seria exactamente lo contrario de
/// P5.
pub struct Writer<'a> {
    buf: &'a mut [u8],
    n: usize,
    overflow: bool,
}

impl<'a> Writer<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, n: 0, overflow: false }
    }

    /// Los bytes escritos, o `None` si en algun momento no entro algo.
    pub fn finish(self) -> Option<&'a [u8]> {
        if self.overflow {
            None
        } else {
            Some(&self.buf[..self.n])
        }
    }

    fn raw(&mut self, b: u8) {
        if self.n < self.buf.len() {
            self.buf[self.n] = b;
            self.n += 1;
        } else {
            self.overflow = true;
        }
    }

    /// El encabezado: tipo mayor en los 3 bits de arriba, y el valor en los 5 de
    /// abajo si entra. Si no entra, esos 5 bits dicen cuantos bytes lo siguen.
    fn head(&mut self, major: u8, value: u64) {
        let m = major << 5;
        if value < 24 {
            self.raw(m | value as u8);
        } else if value <= u8::MAX as u64 {
            self.raw(m | 24);
            self.raw(value as u8);
        } else if value <= u16::MAX as u64 {
            self.raw(m | 25);
            for b in (value as u16).to_be_bytes() {
                self.raw(b);
            }
        } else if value <= u32::MAX as u64 {
            self.raw(m | 26);
            for b in (value as u32).to_be_bytes() {
                self.raw(b);
            }
        } else {
            self.raw(m | 27);
            for b in value.to_be_bytes() {
                self.raw(b);
            }
        }
    }

    pub fn uint(&mut self, v: u64) {
        self.head(0, v);
    }

    /// Bytes crudos. Es lo que va a llevar el codigo maquina que suba el agente.
    pub fn bytes(&mut self, b: &[u8]) {
        self.head(2, b.len() as u64);
        for x in b {
            self.raw(*x);
        }
    }

    /// Una cadena de bytes que se leen de a uno.
    ///
    /// Existe aparte de `bytes` porque leer MMIO con una copia de slice deja
    /// que el compilador reordene o agrupe los accesos, y a un registro de
    /// dispositivo eso lo rompe. Aca cada byte se pide explicitamente.
    pub fn bytes_by(&mut self, len: usize, mut f: impl FnMut(usize) -> u8) {
        self.head(2, len as u64);
        for i in 0..len {
            self.raw(f(i));
        }
    }

    pub fn text(&mut self, s: &str) {
        self.head(3, s.len() as u64);
        for x in s.as_bytes() {
            self.raw(*x);
        }
    }

    /// Abre un arreglo de `len` items. Los items se escriben despues.
    pub fn array(&mut self, len: usize) {
        self.head(4, len as u64);
    }

    /// Abre un mapa de `len` pares. Se escriben clave y valor, alternando.
    pub fn map(&mut self, len: usize) {
        self.head(5, len as u64);
    }

    pub fn bool(&mut self, v: bool) {
        self.raw(if v { 0xf5 } else { 0xf4 });
    }

    pub fn null(&mut self) {
        self.raw(0xf6);
    }
}

// ---------------------------------------------------------------------------
// Delimitacion
// ---------------------------------------------------------------------------

/// Que se puede decir de los bytes que hay hasta ahora.
#[derive(PartialEq, Eq, Debug)]
pub enum Scan {
    /// Hay un item entero, y ocupa estos bytes. Puede sobrar cola.
    Complete(usize),
    /// Lo que hay es un prefijo valido: falta que llegue mas.
    Incomplete,
    /// Esto no es CBOR. No sirve esperar mas bytes.
    Malformed,
}

/// Mira si en `b` hay un item de CBOR completo, y de que largo.
pub fn scan(b: &[u8]) -> Scan {
    match scan_one(b, 0, 0) {
        Ok(end) => Scan::Complete(end),
        Err(e) => e,
    }
}

/// Lee un encabezado. Devuelve (tipo mayor, valor, posicion siguiente).
fn head(b: &[u8], pos: usize) -> Result<(u8, u64, usize), Scan> {
    let ib = *b.get(pos).ok_or(Scan::Incomplete)?;
    let major = ib >> 5;
    let ai = ib & 0x1f;

    let (largo, valor) = match ai {
        0..=23 => (0usize, ai as u64),
        24 => (1, 0),
        25 => (2, 0),
        26 => (4, 0),
        27 => (8, 0),
        // 28, 29 y 30 no existen. 31 es largo indefinido, que no se soporta.
        _ => return Err(Scan::Malformed),
    };

    if largo == 0 {
        return Ok((major, valor, pos + 1));
    }

    let desde = pos + 1;
    let hasta = desde + largo;
    if hasta > b.len() {
        return Err(Scan::Incomplete);
    }

    let mut v: u64 = 0;
    for x in &b[desde..hasta] {
        v = (v << 8) | *x as u64;
    }
    Ok((major, v, hasta))
}

/// Recorre un item entero y devuelve donde termina.
fn scan_one(b: &[u8], pos: usize, depth: u32) -> Result<usize, Scan> {
    if depth > MAX_DEPTH {
        return Err(Scan::Malformed);
    }

    let (major, valor, mut p) = head(b, pos)?;

    match major {
        // Enteros y valores simples: se agotan en el encabezado.
        0 | 1 | 7 => Ok(p),

        // Cadenas: el valor es cuantos bytes siguen.
        2 | 3 => {
            let fin = p.checked_add(valor as usize).ok_or(Scan::Malformed)?;
            if fin > b.len() {
                Err(Scan::Incomplete)
            } else {
                Ok(fin)
            }
        }

        // Arreglo: el valor es cuantos items siguen.
        4 => {
            for _ in 0..valor {
                p = scan_one(b, p, depth + 1)?;
            }
            Ok(p)
        }

        // Mapa: el valor es cuantos PARES siguen.
        5 => {
            for _ in 0..valor.saturating_mul(2) {
                p = scan_one(b, p, depth + 1)?;
            }
            Ok(p)
        }

        // Etiqueta: la sigue un item.
        6 => scan_one(b, p, depth + 1),

        _ => Err(Scan::Malformed),
    }
}

// ---------------------------------------------------------------------------
// Lectura
// ---------------------------------------------------------------------------

/// Recorre un mensaje ya completo.
///
/// Cada metodo devuelve `None` si lo que hay no es del tipo que se pidio, para
/// que un mensaje mal armado se conteste con un error en vez de tumbar nada.
pub struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }

    fn head(&mut self) -> Option<(u8, u64)> {
        let (major, valor, p) = head(self.b, self.pos).ok()?;
        self.pos = p;
        Some((major, valor))
    }

    fn expect(&mut self, major: u8) -> Option<u64> {
        let guardado = self.pos;
        match self.head() {
            Some((m, v)) if m == major => Some(v),
            _ => {
                self.pos = guardado;
                None
            }
        }
    }

    /// Abre un arreglo y devuelve cuantos items tiene.
    pub fn array(&mut self) -> Option<u64> {
        self.expect(4)
    }

    /// Abre un mapa y devuelve cuantos pares tiene.
    pub fn map(&mut self) -> Option<u64> {
        self.expect(5)
    }

    pub fn uint(&mut self) -> Option<u64> {
        self.expect(0)
    }

    pub fn text(&mut self) -> Option<&'a str> {
        let guardado = self.pos;
        let n = self.expect(3)? as usize;
        let fin = self.pos.checked_add(n)?;
        let s = self.b.get(self.pos..fin).and_then(|x| core::str::from_utf8(x).ok());
        match s {
            Some(s) => {
                self.pos = fin;
                Some(s)
            }
            None => {
                self.pos = guardado;
                None
            }
        }
    }

    /// Una cadena de bytes cruda: es lo que trae el codigo maquina que sube el
    /// agente.
    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let guardado = self.pos;
        let n = self.expect(2)? as usize;
        let fin = self.pos.checked_add(n)?;
        match self.b.get(self.pos..fin) {
            Some(x) => {
                self.pos = fin;
                Some(x)
            }
            None => {
                self.pos = guardado;
                None
            }
        }
    }

    /// Saltea el item que viene, sea lo que sea.
    ///
    /// Es lo que permite ignorar una clave que este kernel no conoce sin perder
    /// el hilo del resto del mensaje.
    pub fn skip(&mut self) -> Option<()> {
        let fin = scan_one(self.b, self.pos, 0).ok()?;
        self.pos = fin;
        Some(())
    }
}

/// Cuanto ocupa el encabezado de un valor. Sirve para calcular de antemano si
/// una respuesta va a entrar.
pub const fn head_len(value: u64) -> usize {
    if value < 24 {
        1
    } else if value <= u8::MAX as u64 {
        2
    } else if value <= u16::MAX as u64 {
        3
    } else if value <= u32::MAX as u64 {
        5
    } else {
        MAX_HEAD
    }
}
