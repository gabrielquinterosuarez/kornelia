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
use crate::acpi::Hardware;
use crate::claims;
use crate::cores;
use crate::machine::Machine;
use crate::memory::Kind;
use crate::platform::Platform;
use crate::tables;

/// Lo mas grande que puede ser un pedido. Lo llena `mem.write` subiendo bytes;
/// para algo mas grande se sube por partes, que es justo para lo que existe el
/// `off` de `mem.write`.
const MAX_REQUEST: usize = 64 * 1024;
/// Lo mas grande que puede ser una respuesta. El mapa de memoria de una maquina
/// real con cientos de regiones entra con lugar de sobra.
const MAX_RESPONSE: usize = 64 * 1024;

/// Tope de una lectura, para que la respuesta entre siempre con lugar de sobra
/// para el envoltorio.
const MAX_LECTURA: u64 = 32 * 1024;

static mut INBOX: [u8; MAX_REQUEST] = [0; MAX_REQUEST];
static mut OUTBOX: [u8; MAX_RESPONSE] = [0; MAX_RESPONSE];

/// La linea que avisa que de aca en adelante lo que sale es binario.
///
/// El arranque habla en texto porque hace falta poder enchufar una terminal y
/// ver si la maquina esta viva. Desde esta marca, manda el protocolo.
pub const MARCA: &str = "-- CBOR --";

/// Atiende el cordon umbilical para siempre.
pub fn serve<P: Platform>(p: &mut P, m: &Machine, hw: &Hardware) -> ! {
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
                atender(p, &inbox[..largo], m, hw);

                // Lo que vino pegado atras es el pedido siguiente: se corre al
                // principio en vez de tirarlo.
                inbox.copy_within(largo..n, 0);
                n -= largo;
            }
        }
    }
}

/// Contesta un pedido ya completo.
fn atender<P: Platform>(p: &mut P, req: &[u8], m: &Machine, hw: &Hardware) {
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
        "describe" => describe(p, id, &mut r, m, hw),
        "mem.claim" => mem_claim(p, id, &mut r, m),
        "mem.read" => mem_read(p, id, &mut r),
        "mem.write" => mem_write(p, id, &mut r),
        "release" => release(p, id, &mut r),
        "exec" => exec(p, id, &mut r),
        "core.claim" => core_claim(p, id, &mut r, hw),
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
    /// Lo que el agente tiene reclamado. Es como recupera su estado al
    /// reconectar (D14).
    claims: bool,
    cpus: bool,
    interrupts: bool,
    pcie: bool,
    /// Los nucleos que el agente tiene reclamados y andando (D13).
    cores: bool,
    /// Si no vino la clave `what`, se devuelve el indice (D16).
    indice: bool,
}

fn describe<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>, m: &Machine, hw: &Hardware) {
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
                    Some("claims") => q.claims = true,
                    Some("cpus") => q.cpus = true,
                    Some("interrupts") => q.interrupts = true,
                    Some("pcie") => q.pcie = true,
                    Some("cores") => q.cores = true,
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
        escribir_indice(&mut w, m, hw, P::ARCH);
    } else {
        let mut secciones = 0;
        if q.memory {
            secciones += 1;
        }
        if q.tables {
            secciones += 1;
        }
        if q.claims {
            secciones += 1;
        }
        for extra in [q.cpus, q.interrupts, q.pcie, q.cores] {
            if extra {
                secciones += 1;
            }
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
        if q.claims {
            w.text("claims");
            w.array(claims::count());
            for c in claims::all() {
                escribir_claim(&mut w, &c);
            }
        }
        if q.cpus {
            w.text("cpus");
            w.array(hw.cpus.len());
            for c in hw.cpus {
                w.map(3);
                w.text("id");
                w.uint(c.id);
                w.text("uid");
                w.uint(c.uid as u64);
                w.text("enabled");
                w.bool(c.enabled);
            }
        }
        if q.interrupts {
            w.text("interrupts");
            match hw.interrupts {
                None => w.null(),
                Some(i) => {
                    w.map(3);
                    w.text("kind");
                    w.text(i.kind);
                    w.text("address");
                    w.uint(i.address);
                    w.text("version");
                    w.uint(i.version as u64);
                }
            }
        }
        if q.cores {
            w.text("cores");
            w.array(cores::count());
            for c in cores::all() {
                escribir_core(&mut w, &c);
            }
        }
        if q.pcie {
            w.text("pcie");
            match hw.pcie {
                None => w.null(),
                Some(x) => {
                    w.map(4);
                    w.text("base");
                    w.uint(x.base);
                    w.text("segment");
                    w.uint(x.segment as u64);
                    w.text("bus_start");
                    w.uint(x.bus_start as u64);
                    w.text("bus_end");
                    w.uint(x.bus_end as u64);
                }
            }
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
fn escribir_indice(w: &mut Writer<'_>, m: &Machine, hw: &Hardware, arch: &str) {
    w.map(9);

    w.text("arch");
    w.text(arch);

    w.text("sections");
    w.array(7);
    w.text("memory");
    w.text("tables");
    w.text("claims");
    w.text("cpus");
    w.text("interrupts");
    w.text("pcie");
    w.text("cores");

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

    // D14: al reconectar, el agente recupera aca lo que tenia reclamado.
    w.text("claims");
    w.uint(claims::count() as u64);

    w.text("cpus");
    w.map(2);
    w.text("total");
    w.uint(hw.cpus.len() as u64);
    w.text("usable");
    w.uint(hw.usable_cpus() as u64);

    w.text("interrupts");
    w.bool(hw.interrupts.is_some());

    w.text("pcie");
    w.bool(hw.pcie.is_some());

    w.text("cores");
    w.uint(cores::count() as u64);
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

// ---------------------------------------------------------------------------
// Los verbos de memoria
// ---------------------------------------------------------------------------

/// Lee los argumentos de un pedido de memoria.
///
/// Todo lo que no se reconoce se saltea: agregar una clave nueva no tiene por
/// que romper a un kernel viejo.
struct Args<'a> {
    bytes: Option<u64>,
    at: Option<u64>,
    align: Option<u64>,
    below: Option<u64>,
    handle: Option<u64>,
    off: Option<u64>,
    len: Option<u64>,
    /// El identificador de un nucleo, para `core.claim`.
    core_id: Option<u64>,
    /// Prestados del buffer de entrada, no copiados: subir codigo maquina no
    /// puede costar una copia mas. La vida util los ata al pedido, asi que se
    /// dejan de poder usar cuando llega el siguiente — que es exactamente
    /// cuando se pisan.
    datos: Option<&'a [u8]>,
}

fn leer_args<'a>(r: &mut Reader<'a>) -> Option<Args<'a>> {
    let mut a = Args {
        bytes: None,
        at: None,
        align: None,
        below: None,
        handle: None,
        off: None,
        len: None,
        core_id: None,
        datos: None,
    };

    let Some(pares) = r.map() else { return Some(a) };
    for _ in 0..pares {
        let clave = r.text()?;
        match clave {
            "bytes" => {
                // En `mem.write` la clave `bytes` trae los datos; en el resto,
                // un tamano. Se distinguen por el tipo, que CBOR ya lleva.
                if let Some(d) = r.bytes() {
                    a.datos = Some(d);
                } else {
                    a.bytes = Some(r.uint()?);
                }
            }
            "at" => a.at = Some(r.uint()?),
            "align" => a.align = Some(r.uint()?),
            "below" => a.below = Some(r.uint()?),
            "handle" => a.handle = Some(r.uint()?),
            "off" => a.off = Some(r.uint()?),
            "len" => a.len = Some(r.uint()?),
            "id" => a.core_id = Some(r.uint()?),
            _ => r.skip()?,
        }
    }
    Some(a)
}

fn responder_fallo<P: Platform>(p: &mut P, id: u64, e: claims::Error) {
    responder_error(p, id, e.code());
}

fn mem_claim<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>, m: &Machine) {
    let Some(a) = leer_args(r) else {
        return responder_error(p, id, "malformed arguments");
    };
    let Some(bytes) = a.bytes else {
        return responder_error(p, id, "mem.claim needs bytes");
    };

    let pedido = claims::Request {
        bytes,
        at: a.at,
        align: a.align.unwrap_or(1),
        below: a.below,
    };

    match claims::claim(m, pedido) {
        Err(e) => responder_fallo(p, id, e),
        Ok(c) => {
            let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
            let mut w = Writer::new(out);
            w.array(3);
            w.uint(id);
            w.bool(true);
            escribir_claim(&mut w, &c);
            terminar(p, id, w);
        }
    }
}

fn mem_read<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>) {
    let Some(a) = leer_args(r) else {
        return responder_error(p, id, "malformed arguments");
    };
    let (Some(handle), Some(len)) = (a.handle, a.len) else {
        return responder_error(p, id, "mem.read needs handle and len");
    };
    let off = a.off.unwrap_or(0);

    // Se acota antes de leer: una lectura que no entra en la respuesta se avisa
    // en vez de mandar menos de lo pedido sin decirlo.
    if len > MAX_LECTURA {
        return responder_error(p, id, "read too large");
    }

    match claims::range_of(handle, off, len) {
        Err(e) => responder_fallo(p, id, e),
        Ok(dir) => {
            let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
            let mut w = Writer::new(out);
            w.array(3);
            w.uint(id);
            w.bool(true);
            w.map(1);
            w.text("bytes");
            // De a un byte y volatil: esto puede ser el registro de un
            // dispositivo, no RAM.
            w.bytes_by(len as usize, |i| unsafe {
                core::ptr::read_volatile((dir + i as u64) as *const u8)
            });
            terminar(p, id, w);
        }
    }
}

fn mem_write<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>) {
    let Some(a) = leer_args(r) else {
        return responder_error(p, id, "malformed arguments");
    };
    let (Some(handle), Some(datos)) = (a.handle, a.datos) else {
        return responder_error(p, id, "mem.write needs handle and bytes");
    };
    let off = a.off.unwrap_or(0);

    match claims::range_of(handle, off, datos.len() as u64) {
        Err(e) => responder_fallo(p, id, e),
        Ok(dir) => {
            for (i, b) in datos.iter().enumerate() {
                unsafe { core::ptr::write_volatile((dir + i as u64) as *mut u8, *b) };
            }
            let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
            let mut w = Writer::new(out);
            w.array(3);
            w.uint(id);
            w.bool(true);
            w.map(1);
            w.text("written");
            w.uint(datos.len() as u64);
            terminar(p, id, w);
        }
    }
}

fn release<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>) {
    let Some(a) = leer_args(r) else {
        return responder_error(p, id, "malformed arguments");
    };
    let Some(handle) = a.handle else {
        return responder_error(p, id, "release needs handle");
    };

    if !claims::release(handle) {
        return responder_fallo(p, id, claims::Error::NoSuchHandle);
    }

    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
    let mut w = Writer::new(out);
    w.array(3);
    w.uint(id);
    w.bool(true);
    w.map(1);
    w.text("released");
    w.uint(handle);
    terminar(p, id, w);
}

fn escribir_claim(w: &mut Writer<'_>, c: &claims::Claim) {
    w.map(4);
    w.text("handle");
    w.uint(c.handle);
    w.text("start");
    w.uint(c.start);
    w.text("bytes");
    w.uint(c.bytes);
    // De que clase era la region: el agente decide con el dato a la vista.
    w.text("kind");
    w.text(c.kind.code());
}

fn terminar<P: Platform>(p: &mut P, id: u64, w: Writer<'_>) {
    match w.finish() {
        Some(bytes) => emitir(p, bytes),
        None => responder_error(p, id, "response too large"),
    }
}

// ---------------------------------------------------------------------------
// exec
// ---------------------------------------------------------------------------

/// Salta a codigo del agente y contesta con lo que haya pasado.
///
/// Es el verbo por el que existe todo lo anterior. El agente sube codigo con
/// `mem.write` y lo corre aca; si falla, **el fault vuelve como respuesta** en
/// vez de matar la maquina (P5). El kernel no mira ese codigo ni lo valida: no
/// tiene una opinion sobre lo que el agente deberia hacer (P2).
///
/// Lo que todavia no hace: elegir nucleo —`core.claim` no existe, asi que corre
/// en este— ni recibir un estado inicial de registros. El codigo recibe en el
/// primer registro de argumento su propia direccion, para poder encontrar sus
/// datos sin depender de donde lo hayan cargado.
fn exec<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>) {
    let Some(a) = leer_args(r) else {
        return responder_error(p, id, "malformed arguments");
    };
    let Some(handle) = a.handle else {
        return responder_error(p, id, "exec needs handle");
    };
    let off = a.off.unwrap_or(0);

    // Se comprueba que la entrada este adentro del reclamo. Un byte alcanza:
    // hasta donde llega el codigo lo sabe el codigo, no el kernel.
    let entrada = match claims::range_of(handle, off, 1) {
        Err(e) => return responder_fallo(p, id, e),
        Ok(dir) => dir,
    };
    // El reclamo entero, para que la arquitectura pueda sincronizar cachés.
    let Some(c) = claims::get(handle) else {
        return responder_fallo(p, id, claims::Error::NoSuchHandle);
    };

    // SAFETY: la direccion esta dentro de un reclamo vigente, y el identity map
    // de D12 cubre toda la memoria de la maquina. Lo que haya ahi puede ser
    // cualquier cosa — de eso se trata.
    let salida = unsafe { p.exec(entrada, (c.start, c.bytes)) };

    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
    let mut w = Writer::new(out);
    w.array(3);
    w.uint(id);
    // `true`: el pedido se atendio. Que el codigo haya fallado no es un fallo
    // del pedido — es su resultado, y va adentro.
    w.bool(true);

    w.map(3);

    w.text("faulted");
    w.bool(salida.faulted);

    // Los registros con los nombres de ESTA maquina (D3).
    w.text("registers");
    let nombres = P::REGISTERS;
    let cuantos = nombres.len().min(salida.regs.len());
    w.map(cuantos);
    for i in 0..cuantos {
        w.text(nombres[i]);
        w.uint(salida.regs[i]);
    }

    w.text("fault");
    match salida.fault {
        None => w.null(),
        Some(f) => {
            let pares = if f.address.is_some() { 5 } else { 4 };
            w.map(pares);
            w.text("cause");
            w.text(f.cause.code());
            // El numero que uso la maquina, sin traducir (P4).
            w.text("raw");
            w.uint(f.raw);
            w.text("detail");
            w.uint(f.detail);
            w.text("pc");
            w.uint(f.pc);
            if let Some(dir) = f.address {
                w.text("address");
                w.uint(dir);
            }
        }
    }

    terminar(p, id, w);
}

// ---------------------------------------------------------------------------
// core.claim
// ---------------------------------------------------------------------------

/// Cuanto se espera a que un nucleo avise que llego, en vueltas de espera.
///
/// No hay reloj todavia, asi que se cuenta en iteraciones. El numero es
/// generoso: arrancar un nucleo tarda microsegundos, y esperar de mas solo
/// cuesta tiempo la unica vez que el nucleo no arranca.
const ESPERA: u64 = 200_000_000;

/// Arranca un nucleo y lo deja esperando trabajo (D13).
///
/// El agente no necesita que el kernel sea plural para serlo el: si quiere
/// cinco cosas a la vez, reclama cinco nucleos (P2).
fn core_claim<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>, hw: &Hardware) {
    let Some(a) = leer_args(r) else {
        return responder_error(p, id, "malformed arguments");
    };
    let Some(pedido) = a.core_id else {
        return responder_error(p, id, "core.claim needs id");
    };

    // Tiene que ser un nucleo que la maquina informe, y que informe como
    // usable: inventarle uno seria mandar una interrupcion al vacio.
    let Some(cpu) = hw.cpus.iter().find(|c| c.id == pedido) else {
        return responder_fallo_core(p, id, cores::Error::NoSuchCore);
    };
    if !cpu.enabled {
        return responder_fallo_core(p, id, cores::Error::NotUsable);
    }
    if pedido == p.this_core() {
        // Es el que esta contestando este pedido.
        return responder_fallo_core(p, id, cores::Error::IsBootCore);
    }
    if cores::is_claimed(pedido) {
        return responder_fallo_core(p, id, cores::Error::Taken);
    }

    let (slot, handle) = match cores::reserve(pedido) {
        Err(e) => return responder_fallo_core(p, id, e),
        Ok(x) => x,
    };

    // SAFETY: las tablas de paginas y la captura de faults ya estan puestas;
    // el nucleo nuevo copia esa configuracion.
    if let Err(e) = unsafe { p.start_core(hw, pedido, slot) } {
        cores::settle(slot, cores::State::Failed);
        return responder_fallo_core(p, id, e);
    }

    // Que el pedido se haya hecho no significa que el nucleo este vivo: son dos
    // CPUs distintas y una no puede afirmar por la otra. Se espera a que avise.
    let mut vueltas = 0u64;
    while !cores::has_arrived(slot) && vueltas < ESPERA {
        core::hint::spin_loop();
        vueltas += 1;
    }

    if !cores::has_arrived(slot) {
        cores::settle(slot, cores::State::Failed);
        return responder_fallo_core(p, id, cores::Error::NeverArrived);
    }
    cores::settle(slot, cores::State::Idle);

    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
    let mut w = Writer::new(out);
    w.array(3);
    w.uint(id);
    w.bool(true);
    escribir_core(&mut w, &cores::Core { handle, id: pedido, state: cores::State::Idle });
    terminar(p, id, w);
}

fn escribir_core(w: &mut Writer<'_>, c: &cores::Core) {
    w.map(3);
    w.text("handle");
    w.uint(c.handle);
    w.text("id");
    w.uint(c.id);
    w.text("state");
    w.text(c.state.code());
}

fn responder_fallo_core<P: Platform>(p: &mut P, id: u64, e: cores::Error) {
    responder_error(p, id, e.code());
}
