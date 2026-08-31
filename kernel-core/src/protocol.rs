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
use crate::channel;
use crate::claims;
use crate::cores;
use crate::handlers;
use crate::machine::Machine;
use crate::memory::Kind;
use crate::platform::Platform;
use crate::serial;
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
const MAX_READ: u64 = 32 * 1024;

static mut INBOX: [u8; MAX_REQUEST] = [0; MAX_REQUEST];
static mut OUTBOX: [u8; MAX_RESPONSE] = [0; MAX_RESPONSE];

/// Por donde llego el pedido que se esta atendiendo.
///
/// D17: el kernel contesta **por donde le llegaron**. Si contestara siempre por
/// el cable, el canal rapido serviria para preguntar y no para escuchar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Cable,
    Mailbox,
}

static mut ORIGIN: Origin = Origin::Cable;

/// La linea que avisa que de aca en adelante lo que sale es binario.
///
/// El arranque habla en texto porque hace falta poder enchufar una terminal y
/// ver si la maquina esta viva. Desde esta marca, manda el protocolo.
pub const MARKER: &str = "-- CBOR --";

/// Atiende el cordon umbilical para siempre.
pub fn serve<P: Platform>(p: &mut P, m: &Machine, hw: &Hardware, with_doorbell: bool) -> ! {
    let inbox = unsafe { &mut *core::ptr::addr_of_mut!(INBOX) };
    let mut n = 0usize;

    loop {
        // Con timbre, los bytes los dejó quien atendió el timbre y acá solo se
        // sacan; sin timbre hay que preguntarle al UART, que es lo que quema un
        // núcleo entero y por lo que existe todo esto.
        // Primero el cable, que es el que nunca se abandona (D17).
        let from_cable = if with_doorbell {
            // SAFETY: el bucle corre con el timbre apagado salvo mientras
            // duerme, así que nadie más está en el anillo ahora.
            unsafe { serial::pop() }
        } else {
            p.uart_read_byte()
        };

        // Y si por ahí no vino nada, el buzón del agente, si armó uno.
        let arrived_ok = match from_cable {
            Some(b) => {
                unsafe { ORIGIN = Origin::Cable };
                Some(b)
            }
            None => match channel::current() {
                None => None,
                // SAFETY: el buzón se verificó al adoptarlo y vive en memoria
                // que el agente reclamó, así que sigue mapeada.
                Some(m) => unsafe {
                    m.pop().map(|b| {
                        ORIGIN = Origin::Mailbox;
                        b
                    })
                },
            },
        };

        let Some(b) = arrived_ok else {
            if with_doorbell {
                // Nada que hacer: dormir hasta que alguien hable. Es lo que
                // convierte un núcleo quemado en un núcleo reservado.
                //
                // TODO: mientras el buzón no tenga timbre propio, un pedido que
                // llega solo por ahí espera hasta la próxima vez que el cable
                // despierte al núcleo. Está anotado en DISENO.md.
                p.sleep();
            } else {
                core::hint::spin_loop();
            }
            continue;
        };

        if n < inbox.len() {
            inbox[n] = b;
            n += 1;
        } else {
            // Un pedido mas grande que el buffer no se puede completar nunca.
            // Se avisa y se tira, en vez de quedarse callado para siempre.
            reply_error(p, 0, "request too large");
            n = 0;
            continue;
        }

        match cbor::scan(&inbox[..n]) {
            Scan::Incomplete => {}

            Scan::Malformed => {
                // No sirve esperar mas bytes: lo que hay ya no puede volverse
                // valido. Se descarta todo y se arranca de nuevo.
                reply_error(p, 0, "malformed cbor");
                n = 0;
            }

            Scan::Complete(length) => {
                dispatch(p, &inbox[..length], m, hw);

                // Lo que vino pegado atras es el pedido siguiente: se corre al
                // principio en vez de tirarlo.
                inbox.copy_within(length..n, 0);
                n -= length;
            }
        }
    }
}

/// Contesta un pedido ya completo.
fn dispatch<P: Platform>(p: &mut P, req: &[u8], m: &Machine, hw: &Hardware) {
    let mut r = Reader::new(req);

    // `[id, verbo, argumentos]`
    let Some(3) = r.array() else {
        return reply_error(p, 0, "request must be [id, verb, args]");
    };
    let Some(id) = r.uint() else {
        return reply_error(p, 0, "id must be an unsigned integer");
    };
    let Some(verb) = r.text() else {
        return reply_error(p, id, "verb must be a text string");
    };

    match verb {
        "describe" => describe(p, id, &mut r, m, hw),
        "mem.claim" => mem_claim(p, id, &mut r, m),
        "mem.read" => mem_read(p, id, &mut r),
        "mem.write" => mem_write(p, id, &mut r),
        "release" => release(p, id, &mut r),
        "exec" => exec(p, id, &mut r),
        "core.claim" => core_claim(p, id, &mut r, hw),
        "listen" => listen(p, id, &mut r),
        "irq.install" => irq_install(p, id, &mut r, hw, false),
        "irq.install_raw" => irq_install(p, id, &mut r, hw, true),
        _ => reply_error(p, id, "unknown verb"),
    }
}

// ---------------------------------------------------------------------------
// describe
// ---------------------------------------------------------------------------

/// Que secciones pidio el cliente.
#[derive(Default, Clone, Copy)]
struct Sections {
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
    /// El acuerdo del segundo canal, para que el agente no lo hornee (D17).
    channel: bool,
    /// Los handlers de interrupcion que el agente tiene instalados (D9).
    handlers: bool,
    /// Con que privilegios se puede correr codigo, y como se vuelve (D27).
    exec: bool,
    /// Si no vino la clave `what`, se devuelve el indice (D16).
    index: bool,
}

fn describe<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>, m: &Machine, hw: &Hardware) {
    let mut q = Sections { index: true, ..Default::default() };

    // Los argumentos son un mapa, y puede no venir.
    if let Some(pairs) = r.map() {
        for _ in 0..pairs {
            let Some(key) = r.text() else {
                return reply_error(p, id, "argument keys must be text");
            };
            if key != "what" {
                // Una clave que este kernel no conoce se saltea: agregar una
                // clave nueva no tiene por que romper a un kernel viejo.
                if r.skip().is_none() {
                    return reply_error(p, id, "malformed argument value");
                }
                continue;
            }

            let Some(how_many) = r.array() else {
                return reply_error(p, id, "what must be an array of names");
            };
            q.index = false;
            for _ in 0..how_many {
                match r.text() {
                    Some("memory") => q.memory = true,
                    Some("tables") => q.tables = true,
                    Some("claims") => q.claims = true,
                    Some("cpus") => q.cpus = true,
                    Some("interrupts") => q.interrupts = true,
                    Some("pcie") => q.pcie = true,
                    Some("cores") => q.cores = true,
                    Some("channel") => q.channel = true,
                    Some("handlers") => q.handlers = true,
                    Some("exec") => q.exec = true,
                    // Contestar solo con lo que se reconocio, callado, seria
                    // mentir por omision.
                    Some(_) => return reply_error(p, id, "unknown section in what"),
                    None => return reply_error(p, id, "section names must be text"),
                }
            }
        }
    }

    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
    let mut w = Writer::new(out);

    w.array(3);
    w.uint(id);
    w.bool(true);

    if q.index {
        write_index(&mut w, m, hw, P::ARCH);
    } else {
        let mut sections = 0;
        if q.memory {
            sections += 1;
        }
        if q.tables {
            sections += 1;
        }
        if q.claims {
            sections += 1;
        }
        for extra in [q.cpus, q.interrupts, q.pcie, q.cores, q.channel, q.handlers,
                      q.exec] {
            if extra {
                sections += 1;
            }
        }
        w.map(sections);
        if q.memory {
            w.text("memory");
            write_memory(&mut w, m);
        }
        if q.tables {
            w.text("tables");
            write_tables(&mut w, m, hw);
        }
        if q.claims {
            w.text("claims");
            w.array(claims::count());
            for c in claims::all() {
                write_claim(&mut w, &c);
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
                    w.map(6);
                    w.text("kind");
                    w.text(i.kind);
                    w.text("address");
                    w.uint(i.address);
                    w.text("version");
                    w.uint(i.version as u64);
                    // La parte del GIC que mira cada nucleo. Cero en x86_64.
                    w.text("cpu_interface");
                    w.uint(i.cpu_interface);
                    // Quien recibe las interrupciones de los aparatos en x86_64.
                    w.text("ioapic");
                    match hw.ioapic {
                        None => w.null(),
                        Some(io) => {
                            w.map(2);
                            w.text("address");
                            w.uint(io.address);
                            w.text("gsi_base");
                            w.uint(io.gsi_base as u64);
                        }
                    }
                    // Donde esta el puerto serie y por que interrupcion avisa,
                    // si la maquina lo dice (P4).
                    w.text("serial");
                    match hw.serial {
                        None => w.null(),
                        Some(sp) => {
                            w.map(2);
                            w.text("address");
                            w.uint(sp.address);
                            w.text("gsi");
                            w.uint(sp.gsi as u64);
                        }
                    }
                }
            }
        }
        if q.cores {
            w.text("cores");
            w.array(cores::count());
            for c in cores::all() {
                write_core(&mut w, &c);
            }
        }
        if q.handlers {
            w.text("handlers");
            w.array(handlers::count());
            for h in handlers::all() {
                w.map(5);
                w.text("interrupt");
                w.uint(h.interrupt as u64);
                w.text("entry");
                w.uint(h.entry);
                w.text("raw");
                w.bool(h.raw);
                // Cuantas veces se atendio: el agente sabe si su aparato habla
                // sin tener que instrumentar su propio codigo.
                w.text("served");
                w.uint(h.count);
                // Como hacerla sonar a proposito, para que el agente pueda
                // probar su handler sin esperar al aparato.
                w.text("trigger");
                w.array(h.trigger.count);
                for (addr, value, width) in &h.trigger.writes[..h.trigger.count] {
                    w.array(3);
                    w.uint(*addr);
                    w.uint(*value);
                    w.uint(*width as u64);
                }
            }
        }
        if q.channel {
            w.text("channel");
            write_channel(&mut w);
        }
        if q.exec {
            w.text("exec");
            write_exec::<P>(&mut w);
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
        Some(bytes) => emit(p, bytes),
        // Si no entro, se avisa. Quedarse callado dejaria al cliente esperando
        // una respuesta que no va a llegar nunca.
        None => reply_error(p, id, "response too large"),
    }
}

/// El indice: que hay para pedir, y cuanto de cada cosa.
fn write_index(w: &mut Writer<'_>, m: &Machine, hw: &Hardware, arch: &str) {
    w.map(12);

    w.text("arch");
    w.text(arch);

    w.text("sections");
    w.array(10);
    w.text("memory");
    w.text("tables");
    w.text("claims");
    w.text("cpus");
    w.text("interrupts");
    w.text("pcie");
    w.text("cores");
    w.text("channel");
    w.text("handlers");
    w.text("exec");

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

    w.text("channel");
    w.bool(channel::current().is_some());

    w.text("handlers");
    w.uint(handlers::count() as u64);

    // Los dos privilegios con los que el agente puede correr codigo (D27). El
    // como se vuelve de `supervised` esta en la seccion, que es donde entra un
    // dato que no es un numero.
    w.text("exec");
    w.array(2);
    w.text("supervised");
    w.text("raw");
}

/// Con que privilegio puede correr el codigo del agente, y como vuelve (D27).
///
/// El kernel ofrece los dos y no elige: elegir es del agente (P6). Lo que si
/// hace es **publicar el acuerdo**, para que no lo tenga horneado (P4).
fn write_exec<P: Platform>(w: &mut Writer<'_>) {
    w.map(3);

    w.text("modes");
    w.array(2);
    w.text("supervised");
    w.text("raw");

    // Como vuelve el codigo supervisado. Desde el nivel de abajo un retorno
    // comun no vuelve: hay que pasar por una ventanilla, y estos son los bytes
    // exactos que la abren. Van como codigo maquina y no como el nombre de una
    // instruccion —`int` en x86_64, `svc` en aarch64— para que el agente los
    // pegue al final de lo que emite sin saber sobre que silicio corre (D3).
    w.text("return");
    w.bytes(P::EXEC_RETURN);

    // Y de donde sale la pila: del final del mismo reclamo. Se dice porque es
    // memoria que el agente tiene que dejarle libre a su propio codigo.
    w.text("stack");
    w.text("claim-end");
}

/// El mapa de memoria: un arreglo de `[inicio, bytes, clase]`.
///
/// Cuando la clase es un tipo que este kernel no reconoce, el arreglo trae un
/// cuarto elemento con el numero crudo que informo la maquina. Los arreglos de
/// CBOR llevan su largo, asi que el cliente ve 3 o 4 y no hay ambiguedad — y
/// asi no se pierde lo que la maquina dijo de si misma (P4).
fn write_memory(w: &mut Writer<'_>, m: &Machine) {
    w.array(m.regions.len());
    for r in m.regions {
        match r.kind {
            Kind::Other(raw_bytes) => {
                w.array(4);
                w.uint(r.start);
                w.uint(r.bytes);
                w.text(r.kind.code());
                w.uint(raw_bytes as u64);
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
fn write_tables(w: &mut Writer<'_>, m: &Machine, hw: &Hardware) {
    w.map(4);

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

    // Las firmas de todas las tablas de ACPI que hay, se interpreten o no:
    // informar que existe algo que este kernel todavia no lee es mas util que
    // callarlo (P4).
    w.text("acpi_signatures");
    w.array(hw.signatures.len());
    for f in hw.signatures {
        w.text(core::str::from_utf8(f).unwrap_or("????"));
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

fn reply_error<P: Platform>(p: &mut P, id: u64, reason: &str) {
    // Un error va a un buffer chico y propio: si se usara OUTBOX, un error
    // ocurrido a mitad de armar una respuesta se pisaria con ella.
    let mut buf = [0u8; 128];
    let mut w = Writer::new(&mut buf);
    w.array(3);
    w.uint(id);
    w.bool(false);
    w.map(1);
    w.text("error");
    w.text(reason);

    if let Some(bytes) = w.finish() {
        emit(p, bytes);
    }
}

/// Contesta por donde llegó el pedido (D17).
fn emit<P: Platform>(p: &mut P, bytes: &[u8]) {
    let from_mailbox = unsafe { ORIGIN } == Origin::Mailbox;

    if from_mailbox {
        if let Some(m) = channel::current() {
            // SAFETY: verificado al adoptarlo, y en memoria reclamada.
            let whole = unsafe { bytes.iter().all(|b| m.push(*b)) };
            if whole {
                return;
            }
            // No entró. El cable siempre está, así que se contesta por ahí en
            // vez de perder la respuesta en silencio.
        }
    }

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
    /// Que la memoria sea alcanzable sin privilegio (D27).
    user: Option<bool>,
    handle: Option<u64>,
    off: Option<u64>,
    len: Option<u64>,
    /// El identificador de un nucleo, para `core.claim`.
    core_id: Option<u64>,
    /// El numero de una interrupcion, para `irq.install`.
    interrupt: Option<u64>,
    /// Con que privilegio corre el codigo, para `exec`. Lo declara el agente
    /// (D27) y no tiene valor por omision: elegir por el seria justo lo que
    /// D27 le devuelve.
    mode: Option<&'a str>,
    /// Prestados del buffer de entrada, no copiados: subir codigo maquina no
    /// puede costar una copia mas. La vida util los ata al pedido, asi que se
    /// dejan de poder usar cuando llega el siguiente — que es exactamente
    /// cuando se pisan.
    data: Option<&'a [u8]>,
}

fn read_args<'a>(r: &mut Reader<'a>) -> Option<Args<'a>> {
    let mut a = Args {
        bytes: None,
        at: None,
        align: None,
        below: None,
        user: None,
        handle: None,
        off: None,
        len: None,
        core_id: None,
        interrupt: None,
        mode: None,
        data: None,
    };

    let Some(pairs) = r.map() else { return Some(a) };
    for _ in 0..pairs {
        let key = r.text()?;
        match key {
            "bytes" => {
                // En `mem.write` la clave `bytes` trae los datos; en el resto,
                // un tamano. Se distinguen por el tipo, que CBOR ya lleva.
                if let Some(d) = r.bytes() {
                    a.data = Some(d);
                } else {
                    a.bytes = Some(r.uint()?);
                }
            }
            "at" => a.at = Some(r.uint()?),
            "align" => a.align = Some(r.uint()?),
            "below" => a.below = Some(r.uint()?),
            "user" => a.user = Some(r.bool()?),
            "handle" => a.handle = Some(r.uint()?),
            "off" => a.off = Some(r.uint()?),
            "len" => a.len = Some(r.uint()?),
            "id" => a.core_id = Some(r.uint()?),
            "interrupt" => a.interrupt = Some(r.uint()?),
            "mode" => a.mode = Some(r.text()?),
            _ => r.skip()?,
        }
    }
    Some(a)
}

fn reply_failure<P: Platform>(p: &mut P, id: u64, e: claims::Error) {
    reply_error(p, id, e.code());
}

fn mem_claim<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>, m: &Machine) {
    let Some(a) = read_args(r) else {
        return reply_error(p, id, "malformed arguments");
    };
    let Some(bytes) = a.bytes else {
        return reply_error(p, id, "mem.claim needs bytes");
    };

    let wants_user = a.user.unwrap_or(false);
    let request = claims::Request {
        bytes,
        at: a.at,
        align: a.align.unwrap_or(1),
        below: a.below,
        user: wants_user,
    };

    match claims::claim(m, request) {
        Err(e) => reply_failure(p, id, e),
        Ok(mut c) => {
            // Y si lo pidio alcanzable sin privilegio, marcarlo de verdad. Si
            // no se puede, se suelta: entregar memoria que dice ser del agente
            // y no lo es seria la peor forma de fallar.
            if wants_user {
                // SAFETY: el rango salio de un reclamo vigente y quedo alineado
                // al bloque, que es lo que `set_user_access` exige.
                if unsafe { p.set_user_access(c.start, c.bytes, true) }.is_err() {
                    claims::release(c.handle);
                    return reply_failure(p, id, claims::Error::CannotGrant);
                }
                claims::mark_user(c.handle);
                c.user = true;
            }
            let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
            let mut w = Writer::new(out);
            w.array(3);
            w.uint(id);
            w.bool(true);
            write_claim(&mut w, &c);
            finish_reply(p, id, w);
        }
    }
}

fn mem_read<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>) {
    let Some(a) = read_args(r) else {
        return reply_error(p, id, "malformed arguments");
    };
    let (Some(handle), Some(len)) = (a.handle, a.len) else {
        return reply_error(p, id, "mem.read needs handle and len");
    };
    let off = a.off.unwrap_or(0);

    // Se acota antes de leer: una lectura que no entra en la respuesta se avisa
    // en vez de mandar menos de lo pedido sin decirlo.
    if len > MAX_READ {
        return reply_error(p, id, "read too large");
    }

    match claims::range_of(handle, off, len) {
        Err(e) => reply_failure(p, id, e),
        Ok(addr) => {
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
                core::ptr::read_volatile((addr + i as u64) as *const u8)
            });
            finish_reply(p, id, w);
        }
    }
}

fn mem_write<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>) {
    let Some(a) = read_args(r) else {
        return reply_error(p, id, "malformed arguments");
    };
    let (Some(handle), Some(data)) = (a.handle, a.data) else {
        return reply_error(p, id, "mem.write needs handle and bytes");
    };
    let off = a.off.unwrap_or(0);

    match claims::range_of(handle, off, data.len() as u64) {
        Err(e) => reply_failure(p, id, e),
        Ok(addr) => {
            for (i, b) in data.iter().enumerate() {
                unsafe { core::ptr::write_volatile((addr + i as u64) as *mut u8, *b) };
            }
            let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
            let mut w = Writer::new(out);
            w.array(3);
            w.uint(id);
            w.bool(true);
            w.map(1);
            w.text("written");
            w.uint(data.len() as u64);
            finish_reply(p, id, w);
        }
    }
}

fn release<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>) {
    let Some(a) = read_args(r) else {
        return reply_error(p, id, "malformed arguments");
    };
    let Some(handle) = a.handle else {
        return reply_error(p, id, "release needs handle");
    };

    // Si era alcanzable sin privilegio, se le saca el permiso antes de soltarla:
    // memoria devuelta que sigue marcada seria un agujero silencioso.
    if let Some(c) = claims::get(handle) {
        if c.user {
            // SAFETY: el rango sigue siendo el del reclamo, alineado al bloque.
            let _ = unsafe { p.set_user_access(c.start, c.bytes, false) };
        }
    }

    if !claims::release(handle) {
        return reply_failure(p, id, claims::Error::NoSuchHandle);
    }

    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
    let mut w = Writer::new(out);
    w.array(3);
    w.uint(id);
    w.bool(true);
    w.map(1);
    w.text("released");
    w.uint(handle);
    finish_reply(p, id, w);
}

fn write_claim(w: &mut Writer<'_>, c: &claims::Claim) {
    w.map(5);
    w.text("handle");
    w.uint(c.handle);
    w.text("start");
    w.uint(c.start);
    w.text("bytes");
    w.uint(c.bytes);
    // De que clase era la region: el agente decide con el dato a la vista.
    w.text("kind");
    w.text(c.kind.code());
    // Si quedo alcanzable sin privilegio. Se informa siempre, porque el tamano
    // pudo haberse redondeado al pedirlo.
    w.text("user");
    w.bool(c.user);
}

fn finish_reply<P: Platform>(p: &mut P, id: u64, w: Writer<'_>) {
    match w.finish() {
        Some(bytes) => emit(p, bytes),
        None => reply_error(p, id, "response too large"),
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
/// **El agente declara con que privilegio corre** (D27): `mode` es obligatorio
/// y vale `supervised` —anillo 3 en x86_64, EL0 en aarch64— o `raw`, que es el
/// privilegio del kernel. No hay valor por omision a proposito: si faltara y el
/// kernel eligiera, estaria eligiendo el kernel, que es exactamente lo que D27
/// le devuelve al agente (P6).
///
/// Lo que todavia no hace: elegir nucleo —falta un buzon por nucleo, deuda 9—
/// ni recibir un estado inicial de registros. El codigo recibe en el primer
/// registro de argumento su propia direccion, para poder encontrar sus datos sin
/// depender de donde lo hayan cargado.
fn exec<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>) {
    let Some(a) = read_args(r) else {
        return reply_error(p, id, "malformed arguments");
    };
    let Some(handle) = a.handle else {
        return reply_error(p, id, "exec needs handle");
    };
    let supervised = match a.mode {
        Some("supervised") => true,
        Some("raw") => false,
        Some(_) => return reply_error(p, id, "mode must be supervised or raw"),
        None => return reply_error(p, id, "exec needs mode: supervised or raw"),
    };
    let off = a.off.unwrap_or(0);

    // Se comprueba que la entrada este adentro del reclamo. Un byte alcanza:
    // hasta donde llega el codigo lo sabe el codigo, no el kernel.
    let entry = match claims::range_of(handle, off, 1) {
        Err(e) => return reply_failure(p, id, e),
        Ok(addr) => addr,
    };
    // El reclamo entero: de ahi sale la pila en `supervised`, y con eso la
    // arquitectura puede sincronizar cachés.
    let Some(c) = claims::get(handle) else {
        return reply_failure(p, id, claims::Error::NoSuchHandle);
    };

    // El privilegio declarado y el permiso de la memoria tienen que coincidir,
    // y **la misma pagina no puede ser las dos cosas**: una marcada para el
    // agente deja de ser ejecutable con privilegio, y una que no lo esta no se
    // alcanza sin el. Aceptar el pedido igual seria prometer algo que el
    // hardware va a negar un microsegundo despues, con un fault que no se
    // parece a la causa.
    if supervised && !c.user {
        return reply_error(p, id, "exec supervised needs memory claimed with user");
    }
    if !supervised && c.user {
        return reply_error(p, id, "exec raw needs memory not claimed for the agent");
    }

    // SAFETY: la direccion esta dentro de un reclamo vigente, y el identity map
    // de D12 cubre toda la memoria de la maquina. Lo que haya ahi puede ser
    // cualquier cosa — de eso se trata.
    // En el nucleo del kernel la interrupcion tiene prioridad sobre el codigo
    // del agente (D29): se prenden los timbres mientras corre, asi un `exec`
    // largo no deja al cordon sin atender. El bucle vuelve a apagarlos al salir
    // porque su propio diseno depende de eso.
    p.set_interrupts(true);
    let outcome = unsafe { p.exec(entry, (c.start, c.bytes), supervised) };
    p.set_interrupts(false);

    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
    let mut w = Writer::new(out);
    w.array(3);
    w.uint(id);
    // `true`: el pedido se atendio. Que el codigo haya fallado no es un fallo
    // del pedido — es su resultado, y va adentro.
    w.bool(true);

    w.map(4);

    // Con que privilegio corrio de verdad. Se devuelve aunque el agente lo
    // acabe de mandar: la respuesta tiene que poder leerse sola.
    w.text("mode");
    w.text(if supervised { "supervised" } else { "raw" });

    w.text("faulted");
    w.bool(outcome.faulted);

    // Los registros con los nombres de ESTA maquina (D3).
    w.text("registers");
    let names = P::REGISTERS;
    let how_many = names.len().min(outcome.regs.len());
    w.map(how_many);
    for i in 0..how_many {
        w.text(names[i]);
        w.uint(outcome.regs[i]);
    }

    w.text("fault");
    match outcome.fault {
        None => w.null(),
        Some(f) => {
            let pairs = if f.address.is_some() { 5 } else { 4 };
            w.map(pairs);
            w.text("cause");
            w.text(f.cause.code());
            // El numero que uso la maquina, sin traducir (P4).
            w.text("raw");
            w.uint(f.raw);
            w.text("detail");
            w.uint(f.detail);
            w.text("pc");
            w.uint(f.pc);
            if let Some(addr) = f.address {
                w.text("address");
                w.uint(addr);
            }
        }
    }

    finish_reply(p, id, w);
}

// ---------------------------------------------------------------------------
// core.claim
// ---------------------------------------------------------------------------

/// Cuanto se espera a que un nucleo avise que llego, en vueltas de espera.
///
/// No hay reloj todavia, asi que se cuenta en iteraciones. El numero es
/// generoso: arrancar un nucleo tarda microsegundos, y esperar de mas solo
/// cuesta tiempo la unica vez que el nucleo no arranca.
const WAIT_ROUNDS: u64 = 200_000_000;

/// Arranca un nucleo y lo deja esperando trabajo (D13).
///
/// El agente no necesita que el kernel sea plural para serlo el: si quiere
/// cinco cosas a la vez, reclama cinco nucleos (P2).
fn core_claim<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>, hw: &Hardware) {
    let Some(a) = read_args(r) else {
        return reply_error(p, id, "malformed arguments");
    };
    let Some(request) = a.core_id else {
        return reply_error(p, id, "core.claim needs id");
    };

    // Tiene que ser un nucleo que la maquina informe, y que informe como
    // usable: inventarle uno seria mandar una interrupcion al vacio.
    let Some(cpu) = hw.cpus.iter().find(|c| c.id == request) else {
        return reply_core_failure(p, id, cores::Error::NoSuchCore);
    };
    if !cpu.enabled {
        return reply_core_failure(p, id, cores::Error::NotUsable);
    }
    if request == p.this_core() {
        // Es el que esta contestando este pedido.
        return reply_core_failure(p, id, cores::Error::IsBootCore);
    }
    if cores::is_claimed(request) {
        return reply_core_failure(p, id, cores::Error::Taken);
    }

    let (slot, handle) = match cores::reserve(request) {
        Err(e) => return reply_core_failure(p, id, e),
        Ok(x) => x,
    };

    // SAFETY: las tablas de paginas y la captura de faults ya estan puestas;
    // el nucleo nuevo copia esa configuracion.
    if let Err(e) = unsafe { p.start_core(hw, request, slot) } {
        cores::settle(slot, cores::State::Failed);
        return reply_core_failure(p, id, e);
    }

    // Que el pedido se haya hecho no significa que el nucleo este vivo: son dos
    // CPUs distintas y una no puede afirmar por la otra. Se espera a que avise.
    let mut rounds = 0u64;
    while !cores::has_arrived(slot) && rounds < WAIT_ROUNDS {
        core::hint::spin_loop();
        rounds += 1;
    }

    if !cores::has_arrived(slot) {
        cores::settle(slot, cores::State::Failed);
        return reply_core_failure(p, id, cores::Error::NeverArrived);
    }
    cores::settle(slot, cores::State::Idle);

    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
    let mut w = Writer::new(out);
    w.array(3);
    w.uint(id);
    w.bool(true);
    write_core(&mut w, &cores::Core { handle, id: request, state: cores::State::Idle });
    finish_reply(p, id, w);
}

fn write_core(w: &mut Writer<'_>, c: &cores::Core) {
    w.map(3);
    w.text("handle");
    w.uint(c.handle);
    w.text("id");
    w.uint(c.id);
    w.text("state");
    w.text(c.state.code());
}

fn reply_core_failure<P: Platform>(p: &mut P, id: u64, e: cores::Error) {
    reply_error(p, id, e.code());
}

// ---------------------------------------------------------------------------
// listen
// ---------------------------------------------------------------------------

/// Adopta el buzon que armo el agente como segundo canal (D5, D17).
///
/// Es el verbo numero once, y se agrego a proposito: D17 promete que el kernel
/// escucha por el transporte que escribe el agente, y no habia forma de
/// entregarselo. Un acuerdo implicito hubiera mantenido la lista de diez a costa
/// de esconder complejidad donde nadie la ve.
///
/// El kernel sigue sin saber nada de red: recibe un pedazo de memoria con una
/// forma acordada y mira ahi. Quien mueve los paquetes es el agente (D4).
fn listen<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>) {
    let Some(a) = read_args(r) else {
        return reply_error(p, id, "malformed arguments");
    };
    let Some(handle) = a.handle else {
        return reply_error(p, id, "listen needs handle");
    };

    // Tiene que ser memoria que el agente reclamo: el kernel no adopta un
    // puntero suelto, adopta algo que ya esta anotado como suyo.
    let Some(c) = claims::get(handle) else {
        return reply_failure(p, id, claims::Error::NoSuchHandle);
    };

    // SAFETY: el reclamo esta vigente y el identity map cubre toda la memoria.
    match unsafe { channel::adopt(handle, c.start, c.bytes) } {
        Err(e) => reply_error(p, id, e.code()),
        Ok(m) => {
            let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
            let mut w = Writer::new(out);
            w.array(3);
            w.uint(id);
            w.bool(true);
            w.map(2);
            w.text("handle");
            w.uint(handle);
            w.text("capacity");
            w.uint(m.capacity() as u64);
            finish_reply(p, id, w);
        }
    }
}

/// El acuerdo del segundo canal: que escribir, donde, y que forma tiene.
///
/// Se publica en vez de documentarse aparte para que el agente no lo tenga
/// horneado: si algun dia cambia, lo pregunta y se enteró (P4).
fn write_channel(w: &mut Writer<'_>) {
    w.map(6);

    // Lo que el agente tiene que escribir para que el kernel lo reconozca.
    w.text("magic");
    w.uint(channel::EXPECTED_MAGIC as u64);
    w.text("version");
    w.uint(channel::EXPECTED_VERSION as u64);

    // Donde va cada campo del encabezado.
    w.text("layout");
    w.map(channel::LAYOUT.len());
    for (name, off) in channel::LAYOUT {
        w.text(name);
        w.uint(*off);
    }

    // Como tocarle el timbre al kernel: una lista de escrituras a hacer en
    // orden. Se describe asi y no como "escribile al APIC" a proposito: el
    // agente hace las escrituras que le dijeron y no necesita saber que
    // controlador de interrupciones tiene la maquina (P4).
    w.text("doorbell");
    match channel::doorbell() {
        None => w.null(),
        Some(d) => {
            w.map(2);
            w.text("id");
            w.uint(d.id as u64);
            w.text("writes");
            w.array(d.count);
            for (addr, value, width) in &d.writes[..d.count] {
                w.array(3);
                w.uint(*addr);
                w.uint(*value);
                w.uint(*width as u64);
            }
        }
    }

    // Cuantas veces sono. Es lo que permite comprobar que el timbre anda: el
    // unico canal por el que un cliente puede mirar es el cable, y usarlo
    // despierta al nucleo igual — asi que sin este numero no seria observable.
    w.text("rings");
    w.uint(channel::rings());

    // Y si ya hay uno adoptado.
    w.text("adopted");
    match channel::current() {
        None => w.null(),
        Some(m) => {
            w.map(2);
            w.text("handle");
            w.uint(m.handle);
            w.text("capacity");
            w.uint(m.capacity() as u64);
        }
    }
}

// ---------------------------------------------------------------------------
// irq.install
// ---------------------------------------------------------------------------

/// Pone el codigo del agente a atender una interrupcion de un aparato (D9).
///
/// El kernel no le pasa el evento al agente: el agente escribe el codigo que
/// corre **sin el**, y el kernel solo lo pone en la tabla. Una interrupcion se
/// atiende en microsegundos y el agente contesta en segundos — nunca puede
/// estar en ese lazo (P3).
///
/// La variante `raw` no restringe menos: es que el kernel no pone prologo,
/// epilogo ni el aviso de "ya atendi". No es un guardarrail lo que se saca, son
/// veinte bytes que el agente escribiria igual.
fn irq_install<P: Platform>(
    p: &mut P,
    id: u64,
    r: &mut Reader<'_>,
    hw: &Hardware,
    raw: bool,
) {
    let Some(a) = read_args(r) else {
        return reply_error(p, id, "malformed arguments");
    };
    let (Some(handle), Some(interrupt)) = (a.handle, a.interrupt) else {
        return reply_error(p, id, "irq.install needs handle and interrupt");
    };
    let off = a.off.unwrap_or(0);

    // La entrada tiene que estar adentro de un reclamo vigente. Un byte alcanza:
    // hasta donde llega el handler lo sabe el handler.
    let entry = match claims::range_of(handle, off, 1) {
        Err(e) => return reply_failure(p, id, e),
        Ok(addr) => addr,
    };

    let slot = match handlers::reserve(interrupt as u32, entry, raw) {
        Err(e) => return reply_error(p, id, e.code()),
        Ok(s) => s,
    };

    // SAFETY: la entrada esta dentro de un reclamo y el identity map cubre todo.
    match unsafe { p.install_irq(hw, interrupt as u32, slot, raw) } {
        Err(e) => {
            // Si la arquitectura no pudo, la ranura se suelta: dejarla tomada
            // haria que el proximo intento diga "ya instalado" por nada.
            handlers::release_slot(slot);
            reply_error(p, id, e.code())
        }
        Ok(trigger) => {
            handlers::set_trigger(slot, trigger);
            let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
            let mut w = Writer::new(out);
            w.array(3);
            w.uint(id);
            w.bool(true);
            w.map(3);
            w.text("interrupt");
            w.uint(interrupt);
            w.text("entry");
            w.uint(entry);
            w.text("raw");
            w.bool(raw);
            finish_reply(p, id, w);
        }
    }
}
