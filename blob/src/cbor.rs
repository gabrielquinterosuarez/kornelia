//! Lo justo de CBOR para armar un pedido (D6).
//!
//! El protocolo del kernel es CBOR y nunca JSON, porque por ahi viajan codigo
//! maquina y volcados. Del lado del blob hace falta **escribirlo**, no leerlo:
//! los pedidos los arma el, y de la respuesta le alcanza con saber cuanto
//! ocupa — quien la interpreta de verdad es el agente del otro lado del cable.
//!
//! Por eso esto es un escritor y no un codec. Todo lo que un pedido necesita son
//! cuatro formas: un arreglo, un mapa, un entero y un texto.

/// Arma un mensaje CBOR.
///
/// Es dueno de su propio espacio y **no lo inicializa**, que en un blob no es
/// una optimizacion sino un requisito: poner un buffer en cero hace que el
/// compilador llame a `memset`, y una llamada a otro objeto es una entrada en la
/// GOT, que aca no puede existir (ver `blob.ld`). Lo que no se escribio no se
/// lee nunca: `done` dice hasta donde llego.
pub struct Writer<const N: usize> {
    room: core::mem::MaybeUninit<[u8; N]>,
    used: usize,
    /// Si no entro. Se anota en vez de cortar: un mensaje a medias es peor que
    /// ninguno, asi que al final `done` no devuelve nada y el que llama decide.
    spilled: bool,
}

impl<const N: usize> Writer<N> {
    pub fn new() -> Self {
        Self { room: core::mem::MaybeUninit::uninit(), used: 0, spilled: false }
    }

    fn byte(&mut self, b: u8) {
        if self.used >= N {
            self.spilled = true;
            return;
        }
        // SAFETY: `used` es menor que N, asi que cae adentro del espacio propio.
        unsafe { (self.room.as_mut_ptr() as *mut u8).add(self.used).write(b) };
        self.used += 1;
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

    /// Una cadena de bytes crudos. Es distinta de un texto y el kernel las
    /// distingue: por aca viaja codigo maquina, no palabras (D6).
    pub fn blob(&mut self, data: &[u8]) {
        self.head(2, data.len() as u64);
        for b in data {
            self.byte(*b);
        }
    }

    pub fn bool(&mut self, value: bool) {
        self.byte(if value { 0xF5 } else { 0xF4 });
    }

    /// Los bytes armados, o nada si no entraron.
    pub fn done(&self) -> Option<&[u8]> {
        if self.spilled {
            return None;
        }
        // SAFETY: los primeros `used` bytes se escribieron uno por uno.
        Some(unsafe { core::slice::from_raw_parts(self.room.as_ptr() as *const u8, self.used) })
    }
}

/// Compara dos cadenas de bytes sin que el compilador invente un `memcmp`.
///
/// Un `==` entre slices se convierte en una llamada a `memcmp`, y una llamada a
/// otro objeto es una entrada en la GOT, que en un blob no puede existir (ver
/// `blob.ld`). Leer de a un byte con `read_volatile` es lo que se lo impide: el
/// compilador no puede dar por hecho que esas lecturas se pueden agrupar.
fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        // SAFETY: `i` es menor que el largo de las dos.
        let (x, y) = unsafe {
            (
                core::ptr::read_volatile(a.as_ptr().add(i)),
                core::ptr::read_volatile(b.as_ptr().add(i)),
            )
        };
        if x != y {
            return false;
        }
        i += 1;
    }
    true
}

/// Recorre un mensaje CBOR ya recibido.
///
/// El blob necesita leer poco pero necesita leerlo bien: de una respuesta le
/// interesan dos o tres campos y todo lo demas hay que poder **saltearlo** sin
/// entenderlo. Por eso el lector sabe medir cualquier valor aunque no sepa que
/// significa: es lo que permite buscar una clave adentro de un mapa que puede
/// crecer sin que este codigo se entere.
pub struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

/// Hasta donde se anida un valor al saltearlo. Un mensaje del kernel no anida
/// mas de tres o cuatro niveles; el tope existe para que una respuesta rota no
/// se lleve puesta la pila del blob, que es chica.
const MAX_DEPTH: u32 = 16;

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    /// La cabeza de un valor: que tipo es y con que numero viene.
    fn head(&mut self) -> Option<(u8, u64)> {
        let first = *self.buf.get(self.at)?;
        self.at += 1;
        let major = first >> 5;
        let extra = first & 0x1F;
        let width = match extra {
            0..=23 => return Some((major, extra as u64)),
            24 => 1,
            25 => 2,
            26 => 4,
            27 => 8,
            // 28..30 no existen y 31 es largo indefinido, que este kernel no usa.
            _ => return None,
        };
        let mut value: u64 = 0;
        for _ in 0..width {
            value = (value << 8) | *self.buf.get(self.at)? as u64;
            self.at += 1;
        }
        Some((major, value))
    }

    /// Un entero sin signo.
    pub fn uint(&mut self) -> Option<u64> {
        match self.head()? {
            (0, value) => Some(value),
            _ => None,
        }
    }

    /// El comienzo de un arreglo. Devuelve cuantos elementos trae.
    pub fn array(&mut self) -> Option<u64> {
        match self.head()? {
            (4, items) => Some(items),
            _ => None,
        }
    }

    /// El comienzo de un mapa. Devuelve cuantos pares trae.
    pub fn map(&mut self) -> Option<u64> {
        match self.head()? {
            (5, pairs) => Some(pairs),
            _ => None,
        }
    }

    /// Un `true` o un `false`.
    pub fn bool(&mut self) -> Option<bool> {
        match self.head()? {
            (7, 20) => Some(false),
            (7, 21) => Some(true),
            _ => None,
        }
    }

    /// Una cadena de bytes o de texto: se devuelve prestada, sin copiarla.
    ///
    /// No copiar no es una optimizacion: copiar querria decir un `memcpy`, y un
    /// `memcpy` es una entrada en la GOT, que en un blob no puede existir (ver
    /// `blob.ld`).
    fn run(&mut self, want: u8) -> Option<&'a [u8]> {
        let (major, len) = self.head()?;
        if major != want {
            return None;
        }
        let from = self.at;
        let to = from.checked_add(len as usize)?;
        let slice = self.buf.get(from..to)?;
        self.at = to;
        Some(slice)
    }

    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        self.run(2)
    }

    pub fn text(&mut self) -> Option<&'a [u8]> {
        self.run(3)
    }

    /// Saltea el proximo valor sea el que sea, con todo lo que tenga adentro.
    pub fn skip(&mut self) -> Option<()> {
        self.skip_deep(0)
    }

    fn skip_deep(&mut self, depth: u32) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        let (major, value) = self.head()?;
        match major {
            // Enteros con y sin signo: la cabeza ya los trajo enteros.
            0 | 1 => Some(()),
            // Cadenas: el numero es cuantos bytes siguen.
            2 | 3 => {
                self.at = self.at.checked_add(value as usize)?;
                if self.at > self.buf.len() {
                    return None;
                }
                Some(())
            }
            // Arreglo: el numero es cuantos valores siguen.
            4 => {
                for _ in 0..value {
                    self.skip_deep(depth + 1)?;
                }
                Some(())
            }
            // Mapa: el doble, porque cada par son dos valores.
            5 => {
                for _ in 0..value.checked_mul(2)? {
                    self.skip_deep(depth + 1)?;
                }
                Some(())
            }
            // Etiqueta: no cuenta como valor, lo que sigue es el valor real.
            6 => self.skip_deep(depth + 1),
            // Simples: true, false, null. Todo lo que este kernel usa entra en
            // la cabeza; los flotantes no aparecen en el protocolo.
            7 => Some(()),
            _ => None,
        }
    }

    /// Lo que queda por leer desde donde esta parado.
    ///
    /// Sirve para mirar dos veces el mismo valor: `find` avanza, asi que para
    /// buscar dos claves de un mismo mapa hace falta un segundo lector sobre el
    /// mismo lugar. Es prestar la vista, no copiar nada.
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.at.min(self.buf.len())..]
    }

    /// Estando parado en un mapa, deja al lector **sobre el valor** de esa clave.
    ///
    /// Busca en vez de contar posiciones a proposito: asi el kernel puede
    /// agregarle campos a una respuesta sin romper a quien la lee (P4 aplicado
    /// al formato, no solo al contenido).
    pub fn find(&mut self, key: &str) -> Option<()> {
        let pairs = self.map()?;
        for _ in 0..pairs {
            let name = self.text()?;
            if same(name, key.as_bytes()) {
                return Some(());
            }
            self.skip()?;
        }
        None
    }
}
