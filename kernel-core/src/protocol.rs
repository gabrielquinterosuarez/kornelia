//! El protocolo del agente (D6).
//!
//! # La forma
//!
//! Pedido:    `[id, verbo, argumentos]`
//! Respuesta: `[id, ok, carga]`
//!
//! El `id` lo elige quien pregunta y vuelve tal cual. Con un solo agente (D13)
//! podria parecer de mas, pero es lo que permite tener varios pedidos en vuelo
//! sin adivinar cual respuesta es de cual — y agregarlo despues obligaria a
//! romper a todo cliente ya escrito.
//!
//! `ok` es `true` o `false`. Un pedido que sale mal **se contesta**, no se
//! ignora ni tumba nada: un fallo es un dato (P5).
//!
//! # Los nombres van en ingles
//!
//! Los verbos, las claves y los codigos de esta interfaz son ingles, igual que
//! el resto del codigo. El espanol es para lo que lee un humano en la terminal,
//! y esto no lo lee un humano.
//!
//! # `describe` no adivina (D16)
//!
//! Sin argumentos NO vuelca todo: devuelve el **indice** de lo que se puede
//! pedir, con cuanto hay de cada cosa. Volcar todo ahogaria a un cliente chico
//! y resumir le sacaria informacion a uno grande; devolver el indice deja que
//! cada uno pida la profundidad que quiera, sin que el kernel tenga que suponer
//! con quien habla.
//!
//! Y si se pide una seccion que no existe, se contesta con error en vez de
//! mandar lo que si se reconocio: devolver menos de lo pedido sin decirlo seria
//! el kernel mintiendo por omision.

use crate::cbor::{self, Reader, Scan, Writer};
use crate::machine::Machine;
use crate::memory::Kind;
use crate::platform::Platform;
use crate::tables;

/// Lo mas grande que puede ser un pedido.
const MAX_REQUEST: usize = 8 * 1024;
/// Lo mas grande que puede ser una respuesta. El mapa de memoria de una maquina
/// real con cientos de regiones entra con lugar de sobra.
const MAX_RESPONSE: usize = 64 * 1024;

static mut INBOX: [u8; MAX_REQUEST] = [0; MAX_REQUEST];
static mut OUTBOX: [u8; MAX_RESPONSE] = [0; MAX_RESPONSE];

/// La linea que avisa que de aca en adelante lo que sale es binario.
///
/// El arranque habla en texto porque hace falta poder enchufar una terminal y
/// ver si la maquina esta viva. Desde esta marca, manda el protocolo.
pub const MARCA: &str = "-- CBOR --";

/// Atiende el cordon umbilical para siempre.
pub fn serve<P: Platform>(p: &mut P, m: &Machine) -> ! {
    let inbox = unsafe { &mut *core::ptr::addr_of_mut!(INBOX) };
    let mut n = 0usize;

    loop {
        let Some(b) = p.uart_read_byte() else {
            core::hint::spin_loop();
            continue;
        };

        if n < inbox.len() {
            inbox[n] = b;
            n += 1;
        } else {
            // Un pedido mas grande que el buffer no se puede completar nunca.
            // Se avisa y se tira, en vez de quedarse callado para siempre.
            responder_error(p, 0, "request too large");
            n = 0;
            continue;
        }

        match cbor::scan(&inbox[..n]) {
            Scan::Incomplete => {}

            Scan::Malformed => {
                // No sirve esperar mas bytes: lo que hay ya no puede volverse
                // valido. Se descarta todo y se arranca de nuevo.
                responder_error(p, 0, "malformed cbor");
                n = 0;
            }

            Scan::Complete(largo) => {
                atender(p, &inbox[..largo], m);

                // Lo que vino pegado atras es el pedido siguiente: se corre al
                // principio en vez de tirarlo.
                inbox.copy_within(largo..n, 0);
                n -= largo;
            }
        }
    }
}

/// Contesta un pedido ya completo.
fn atender<P: Platform>(p: &mut P, req: &[u8], m: &Machine) {
    let mut r = Reader::new(req);

    // `[id, verbo, argumentos]`
    let Some(3) = r.array() else {
        return responder_error(p, 0, "request must be [id, verb, args]");
    };
    let Some(id) = r.uint() else {
        return responder_error(p, 0, "id must be an unsigned integer");
    };
    let Some(verbo) = r.text() else {
        return responder_error(p, id, "verb must be a text string");
    };

    match verbo {
        "describe" => describe(p, id, &mut r, m),
        _ => responder_error(p, id, "unknown verb"),
    }
}

// ---------------------------------------------------------------------------
// describe
// ---------------------------------------------------------------------------

/// Que secciones pidio el cliente.
#[derive(Default, Clone, Copy)]
struct Pedido {
    memory: bool,
    tables: bool,
    /// Si no vino la clave `what`, se devuelve el indice (D16).
    indice: bool,
}

fn describe<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>, m: &Machine) {
    let mut q = Pedido { indice: true, ..Default::default() };

    // Los argumentos son un mapa, y puede no venir.
    if let Some(pares) = r.map() {
        for _ in 0..pares {
            let Some(clave) = r.text() else {
                return responder_error(p, id, "argument keys must be text");
            };
            if clave != "what" {
                // Una clave que este kernel no conoce se saltea: agregar una
                // clave nueva no tiene por que romper a un kernel viejo.
                if r.skip().is_none() {
                    return responder_error(p, id, "malformed argument value");
                }
                continue;
            }

            let Some(cuantas) = r.array() else {
                return responder_error(p, id, "what must be an array of names");
            };
            q.indice = false;
            for _ in 0..cuantas {
                match r.text() {
                    Some("memory") => q.memory = true,
                    Some("tables") => q.tables = true,
                    // Contestar solo con lo que se reconocio, callado, seria
                    // mentir por omision.
                    Some(_) => return responder_error(p, id, "unknown section in what"),
                    None => return responder_error(p, id, "section names must be text"),
                }
            }
        }
    }

    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
    let mut w = Writer::new(out);

    w.array(3);
    w.uint(id);
    w.bool(true);

    if q.indice {
        escribir_indice(&mut w, m, P::ARCH);
    } else {
        let mut secciones = 0;
        if q.memory {
            secciones += 1;
        }
        if q.tables {
            secciones += 1;
        }
        w.map(secciones);
        if q.memory {
            w.text("memory");
            escribir_memoria(&mut w, m);
        }
        if q.tables {
            w.text("tables");
            escribir_tablas(&mut w, m);
        }
    }

    match w.finish() {
        Some(bytes) => emitir(p, bytes),
        // Si no entro, se avisa. Quedarse callado dejaria al cliente esperando
        // una respuesta que no va a llegar nunca.
        None => responder_error(p, id, "response too large"),
    }
}

/// El indice: que hay para pedir, y cuanto de cada cosa.
fn escribir_indice(w: &mut Writer<'_>, m: &Machine, arch: &str) {
    w.map(4);

    w.text("arch");
    w.text(arch);

    w.text("sections");
    w.array(2);
    w.text("memory");
    w.text("tables");

    w.text("memory");
    w.map(2);
    w.text("regions");
    w.uint(m.regions.len() as u64);
    w.text("free");
    w.uint(m.free_bytes());

    w.text("tables");
    w.map(3);
    w.text("acpi");
    w.bool(m.tables.acpi.is_some());
    w.text("device_tree");
    w.bool(m.tables.device_tree.is_some());
    w.text("smbios");
    w.bool(m.tables.smbios.is_some());
}

/// El mapa de memoria: un arreglo de `[inicio, bytes, clase]`.
///
/// Cuando la clase es un tipo que este kernel no reconoce, el arreglo trae un
/// cuarto elemento con el numero crudo que informo la maquina. Los arreglos de
/// CBOR llevan su largo, asi que el cliente ve 3 o 4 y no hay ambiguedad — y
/// asi no se pierde lo que la maquina dijo de si misma (P4).
fn escribir_memoria(w: &mut Writer<'_>, m: &Machine) {
    w.array(m.regions.len());
    for r in m.regions {
        match r.kind {
            Kind::Other(crudo) => {
                w.array(4);
                w.uint(r.start);
                w.uint(r.bytes);
                w.text(r.kind.code());
                w.uint(crudo as u64);
            }
            _ => {
                w.array(3);
                w.uint(r.start);
                w.uint(r.bytes);
                w.text(r.kind.code());
            }
        }
    }
}

/// Donde la maquina guarda su propia descripcion, ya verificada.
fn escribir_tablas(w: &mut Writer<'_>, m: &Machine) {
    w.map(3);

    w.text("acpi");
    match m.tables.acpi {
        None => w.null(),
        Some(addr) => {
            // SAFETY: la direccion la publico el firmware y `read_acpi`
            // verifica firma y checksum antes de creerle.
            match unsafe { tables::read_acpi(addr) } {
                None => {
                    // El puntero estaba, pero no apuntaba a un RSDP. Se dice.
                    w.map(2);
                    w.text("addr");
                    w.uint(addr);
                    w.text("valid");
                    w.bool(false);
                }
                Some(a) => {
                    w.map(4);
                    w.text("addr");
                    w.uint(addr);
                    w.text("revision");
                    w.uint(a.revision as u64);
                    w.text("rsdt");
                    w.uint(a.rsdt as u64);
                    w.text("xsdt");
                    match a.xsdt {
                        Some(x) => w.uint(x),
                        None => w.null(),
                    }
                }
            }
        }
    }

    w.text("device_tree");
    match m.tables.device_tree {
        None => w.null(),
        Some(addr) => {
            // SAFETY: idem; `read_device_tree` verifica el numero magico.
            match unsafe { tables::read_device_tree(addr) } {
                None => {
                    w.map(2);
                    w.text("addr");
                    w.uint(addr);
                    w.text("valid");
                    w.bool(false);
                }
                Some(d) => {
                    w.map(3);
                    w.text("addr");
                    w.uint(addr);
                    w.text("bytes");
                    w.uint(d.bytes as u64);
                    w.text("version");
                    w.uint(d.version as u64);
                }
            }
        }
    }

    w.text("smbios");
    match m.tables.smbios {
        None => w.null(),
        // Todavia no se interpreta: se dice donde esta y nada mas. Inventarle
        // campos seria peor que admitir que no se leyo.
        Some(addr) => {
            w.map(1);
            w.text("addr");
            w.uint(addr);
        }
    }
}

// ---------------------------------------------------------------------------
// Salida
// ---------------------------------------------------------------------------

fn responder_error<P: Platform>(p: &mut P, id: u64, motivo: &str) {
    // Un error va a un buffer chico y propio: si se usara OUTBOX, un error
    // ocurrido a mitad de armar una respuesta se pisaria con ella.
    let mut buf = [0u8; 128];
    let mut w = Writer::new(&mut buf);
    w.array(3);
    w.uint(id);
    w.bool(false);
    w.map(1);
    w.text("error");
    w.text(motivo);

    if let Some(bytes) = w.finish() {
        emitir(p, bytes);
    }
}

fn emitir<P: Platform>(p: &mut P, bytes: &[u8]) {
    for b in bytes {
        p.uart_write_byte(*b);
    }
}

/// Cuanto ocupa el encabezado de un valor. Reexportado para que quien arme una
/// respuesta grande pueda estimar antes de escribirla.
pub use cbor::head_len;
