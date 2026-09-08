//! Como el blob le pide cosas a la maquina.
//!
//! Es el equivalente de `ask_verb` del cliente: arma un pedido, lo pasa por la
//! ventanilla y deja un lector sobre la respuesta. Del otro lado es **el mismo
//! `dispatch` de los once verbos** que atiende el cable, con un origen mas
//! (D17), asi que lo que se puede pedir desde aca es exactamente lo que se puede
//! pedir desde afuera. Ni mas —el blob no tiene privilegios especiales— ni
//! menos.

use crate::cbor::{Reader, Writer};
use crate::gate;

/// Cuanto ocupa el pedido mas grande que arma este blob.
const REQUEST_ROOM: usize = 96;
/// Y cuanto lugar se deja para una respuesta. `describe` puede devolver mucho,
/// pero este blob solo pide sus secciones chicas.
const REPLY_ROOM: usize = 512;

/// Un reclamo de memoria, como lo devuelve `mem.claim`.
#[derive(Clone, Copy)]
pub struct Claim {
    /// Con que se lo nombra despues. Nunca se reusa (D14).
    pub handle: u64,
    /// Donde quedo, en fisicas.
    pub start: u64,
}

/// El canal del blob hacia el kernel.
pub struct Session {
    next_id: u64,
    /// Sin inicializar **a proposito**: lo llena el kernel. Ponerlo en cero
    /// haria que el compilador llame a `memset`, que es una entrada en la GOT, y
    /// eso en un blob no puede existir (ver `blob.ld`).
    reply: core::mem::MaybeUninit<[u8; REPLY_ROOM]>,
}

impl Session {
    pub fn new() -> Self {
        Self { next_id: 0x0B10, reply: core::mem::MaybeUninit::uninit() }
    }

    /// Un identificador distinto para cada pedido. El kernel lo devuelve tal
    /// cual, que es como se reconoce a que pregunta contesta una respuesta.
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Manda un pedido ya armado y deja el lector **sobre el payload**.
    ///
    /// Devuelve `None` si el kernel no contesto o si contesto que no. Que el
    /// error no diga cual es no es descuido: el blob no tiene por donde
    /// contarlo, y lo que importa afuera es que no siguio adelante como si
    /// hubiera andado.
    fn send(&mut self, request: &[u8]) -> Option<Reader<'_>> {
        let room = self.reply.as_mut_ptr() as *mut u8;
        // SAFETY: `room` son REPLY_ROOM bytes nuestros, y el pedido vive
        // mientras dure la llamada.
        let used =
            unsafe { gate::service(request.as_ptr(), request.len(), room, REPLY_ROOM) };
        if used == 0 || used > REPLY_ROOM {
            return None;
        }
        // SAFETY: el kernel dejo `used` bytes ahi, asi que esa parte ya no esta
        // sin inicializar.
        let bytes = unsafe { core::slice::from_raw_parts(room as *const u8, used) };

        // Toda respuesta es `[id, ok, payload]`.
        let mut r = Reader::new(bytes);
        r.array()?;
        r.uint()?;
        if !r.bool()? {
            return None;
        }
        Some(r)
    }

    /// Donde se configura PCIe, que es lo primero que necesita cualquier driver.
    ///
    /// El kernel publica **donde preguntar**, no que hay conectado: quien
    /// recorre el bus es el agente (D4, P4).
    pub fn pcie_base(&mut self) -> Option<u64> {
        let id = self.id();
        let mut w = Writer::<REQUEST_ROOM>::new();
        w.array(3);
        w.uint(id);
        w.text("describe");
        w.map(1);
        w.text("what");
        w.array(1);
        w.text("pcie");
        let mut r = self.send(w.done()?)?;
        r.find("pcie")?;
        r.find("base")?;
        r.uint()
    }

    /// Reclama un rango exacto de direcciones. Asi se pide MMIO.
    pub fn claim_at(&mut self, at: u64, bytes: u64) -> Option<Claim> {
        let id = self.id();
        let mut w = Writer::<REQUEST_ROOM>::new();
        w.array(3);
        w.uint(id);
        w.text("mem.claim");
        w.map(2);
        w.text("at");
        w.uint(at);
        w.text("bytes");
        w.uint(bytes);
        self.claim_reply(w.done()?)
    }

    /// Reclama memoria por tamano, y la deja alcanzable sin privilegio.
    ///
    /// `user` va en true porque el blob corre `supervised` (D29): sin ese bit el
    /// silicio no lo deja ni leer lo que reclamo.
    pub fn claim(&mut self, bytes: u64, align: u64) -> Option<Claim> {
        let id = self.id();
        let mut w = Writer::<REQUEST_ROOM>::new();
        w.array(3);
        w.uint(id);
        w.text("mem.claim");
        w.map(3);
        w.text("bytes");
        w.uint(bytes);
        w.text("align");
        w.uint(align);
        w.text("user");
        w.bool(true);
        self.claim_reply(w.done()?)
    }

    /// La parte comun de los dos: mandar y sacar handle y direccion.
    fn claim_reply(&mut self, request: &[u8]) -> Option<Claim> {
        let mut r = self.send(request)?;
        // Se lee dos veces desde el principio del mapa porque `find` avanza: son
        // dos busquedas independientes sobre la misma respuesta.
        let mut again = Reader::new(r.rest());
        r.find("handle")?;
        let handle = r.uint()?;
        again.find("start")?;
        let start = again.uint()?;
        Some(Claim { handle, start })
    }

    /// Lee cuatro bytes de un reclamo, con accesos de cuatro bytes.
    ///
    /// El ancho no es un detalle: un registro de dispositivo que solo acepta
    /// lecturas de 32 bits, leido de a un byte, devuelve ceros en x86_64 y mata
    /// el bus en aarch64.
    pub fn read_u32(&mut self, handle: u64, off: u64) -> Option<u32> {
        let id = self.id();
        let mut w = Writer::<REQUEST_ROOM>::new();
        w.array(3);
        w.uint(id);
        w.text("mem.read");
        w.map(4);
        w.text("handle");
        w.uint(handle);
        w.text("off");
        w.uint(off);
        w.text("len");
        w.uint(4);
        w.text("width");
        w.uint(4);
        let mut r = self.send(w.done()?)?;
        r.find("bytes")?;
        let got = r.bytes()?;
        if got.len() < 4 {
            return None;
        }
        Some(u32::from_le_bytes([got[0], got[1], got[2], got[3]]))
    }

    /// Escribe dos bytes en un reclamo. Alcanza para prender un aparato del bus.
    pub fn write_u16(&mut self, handle: u64, off: u64, value: u16) -> Option<()> {
        let id = self.id();
        let mut w = Writer::<REQUEST_ROOM>::new();
        w.array(3);
        w.uint(id);
        w.text("mem.write");
        w.map(3);
        w.text("handle");
        w.uint(handle);
        w.text("off");
        w.uint(off);
        w.text("bytes");
        w.blob(&value.to_le_bytes());
        self.send(w.done()?).map(|_| ())
    }

    /// Devuelve lo reclamado. Lo que el agente toma, el agente devuelve.
    pub fn release(&mut self, handle: u64) -> Option<()> {
        let id = self.id();
        let mut w = Writer::<REQUEST_ROOM>::new();
        w.array(3);
        w.uint(id);
        w.text("release");
        w.map(1);
        w.text("handle");
        w.uint(handle);
        self.send(w.done()?).map(|_| ())
    }
}
