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
use crate::dma;
use crate::fault::Outcome;
use crate::handlers;
use crate::machine::Machine;
use crate::memory::Kind;
use crate::platform::Platform;
use crate::serial;
use crate::tables;
use crate::work;

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
    /// El blob (D18). Corre **antes** de que exista el protocolo, asi que su
    /// transporte no es un cable ni un buzon: es un pedazo de memoria suyo, que
    /// pasa en la misma llamada.
    Blob,
}

static mut ORIGIN: Origin = Origin::Cable;

/// Donde dejar la respuesta cuando el pedido vino del blob.
struct BlobReply {
    at: *mut u8,
    cap: usize,
    used: usize,
    /// La respuesta no entro. Se anota en vez de mandar media: media respuesta
    /// se parsea hasta la mitad y despues miente.
    overflowed: bool,
}

static mut BLOB_REPLY: BlobReply =
    BlobReply { at: core::ptr::null_mut(), cap: 0, used: 0, overflowed: false };

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
                // Y despierta por los dos lados: el cable tiene su timbre y el
                // buzón el suyo, así que un pedido que llega solo por el buzón
                // no espera a que alguien toque el cable. Acá hubo un TODO que
                // decía lo contrario y quedó viejo cuando el buzón tuvo timbre.
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

/// Con qué atender los pedidos del blob mientras corre. Punteros crudos porque
/// la ventanilla es una función `extern "C"` que el blob llama y a la que no le
/// puede pasar nada de esto.
static mut BLOB_P: *mut () = core::ptr::null_mut();
static mut BLOB_M: *const Machine = core::ptr::null();
static mut BLOB_HW: *const Hardware = core::ptr::null();

/// La ventanilla del blob: atiende **un** pedido y deja la respuesta en su
/// buffer. Devuelve cuantos bytes ocupa, o 0 si no se pudo.
///
/// El blob corre con privilegio completo y en el mismo espacio de direcciones,
/// asi que no le hace falta una ventanilla del estilo de `supervised`: llama a
/// esta funcion como a cualquier otra. Y adentro es el mismo `dispatch` de los
/// once verbos — lo unico distinto es por donde sale la respuesta (D17).
///
/// # Safety
///
/// `req` y `out` tienen que ser rangos validos de `len` y `cap` bytes. Nadie los
/// puede comprobar: el blob corre `raw` y ya podia escribir donde quisiera.
unsafe extern "C" fn serve_blob<P: Platform>(
    req: *const u8,
    len: usize,
    out: *mut u8,
    cap: usize,
) -> usize {
    // SAFETY: los tres los dejó `open_blob_gate` y valen mientras el blob corre.
    if req.is_null() || out.is_null() || unsafe { BLOB_P }.is_null() {
        return 0;
    }
    let p = unsafe { &mut *(BLOB_P as *mut P) };
    let m = unsafe { &*BLOB_M };
    let hw = unsafe { &*BLOB_HW };

    let before = unsafe { ORIGIN };
    unsafe {
        ORIGIN = Origin::Blob;
        BLOB_REPLY = BlobReply { at: out, cap, used: 0, overflowed: false };
    }

    // SAFETY: es el rango que declaró el blob.
    unsafe { dispatch(p, core::slice::from_raw_parts(req, len), m, hw) };

    let r = unsafe { &*core::ptr::addr_of!(BLOB_REPLY) };
    let n = if r.overflowed { 0 } else { r.used };
    unsafe { ORIGIN = before };
    n
}

/// Deja lista la ventanilla y devuelve su direccion, para pasarsela al blob.
///
/// # Safety
///
/// El `&mut P` se guarda como puntero crudo: hay que cerrar la ventanilla con
/// `close_blob_gate` apenas el blob vuelve, o queda apuntando a algo muerto.
pub unsafe fn open_blob_gate<P: Platform>(p: &mut P, m: &Machine, hw: &Hardware) -> u64 {
    unsafe {
        BLOB_P = p as *mut P as *mut ();
        BLOB_M = m;
        BLOB_HW = hw;
    }
    serve_blob::<P> as *const () as usize as u64
}

/// Cierra la ventanilla. Un blob que guardo la direccion para usarla despues no
/// va a encontrar nada: es a proposito, porque lo que hay del otro lado dejo de
/// existir cuando el blob volvio.
pub fn close_blob_gate() {
    unsafe {
        BLOB_P = core::ptr::null_mut();
        BLOB_M = core::ptr::null();
        BLOB_HW = core::ptr::null();
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
        "release" => release(p, id, &mut r, hw),
        "exec" => exec(p, id, &mut r),
        "core.claim" => core_claim(p, id, &mut r, hw),
        "listen" => listen(p, id, &mut r),
        "irq.install" => irq_install(p, id, &mut r, hw, false),
        "irq.install_raw" => irq_install(p, id, &mut r, hw, true),
        "dma.allow" => dma_allow(p, id, &mut r, hw),
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
    /// El IOMMU: quien decide que memoria puede tocar un aparato (D8).
    iommu: bool,
    /// El reloj de la maquina, si informa a que ritmo sube (deuda 17).
    clock: bool,
    /// El estado del cordon umbilical, incluidos los bytes que se perdieron.
    cable: bool,
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
                    Some("iommu") => q.iommu = true,
                    Some("clock") => q.clock = true,
                    Some("cable") => q.cable = true,
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

    let clock = p.clock();

    w.array(3);
    w.uint(id);
    w.bool(true);

    if q.index {
        // El reloj se lee antes de tomar el escritor: `Writer` no presta `p`,
        // pero leerlo aca deja el indice armado de un solo tiro.
        write_index(&mut w, m, hw, P::ARCH, clock);
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
                      q.exec, q.iommu, q.clock, q.cable] {
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
                    w.map(7);
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
                    // Por donde un aparato dispara una interrupcion
                    // escribiendo en memoria. En x86_64 no hace falta un
                    // aparato en el medio, asi que va en nulo.
                    w.text("msi");
                    match hw.msi {
                        None => w.null(),
                        Some(m) => {
                            w.map(3);
                            w.text("base");
                            w.uint(m.base);
                            w.text("spi_base");
                            w.uint(m.spi_base as u64);
                            w.text("spi_count");
                            w.uint(m.spi_count as u64);
                        }
                    }

                    w.text("serial");
                    match hw.serial {
                        None => w.null(),
                        Some(sp) => {
                            w.map(3);
                            w.text("address");
                            w.uint(sp.address);
                            w.text("gsi");
                            w.uint(sp.gsi as u64);
                            // Y si el kernel **la esta usando**, que no es lo
                            // mismo que informarla: la diferencia entre andar y
                            // andar porque la maquina dijo donde es lo unico
                            // que hace que ande en otra placa (P4).
                            w.text("in_use");
                            w.bool(p.serial_from_machine());
                        }
                    }
                }
            }
        }
        if q.cores {
            w.text("cores");
            w.array(cores::count());
            for c in cores::all() {
                write_core::<P>(&mut w, &c);
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
        if q.cable {
            w.text("cable");
            write_cable(&mut w);
        }
        if q.exec {
            w.text("exec");
            write_exec::<P>(p, &mut w);
        }
        if q.iommu {
            w.text("iommu");
            match hw.iommu {
                None => w.null(),
                Some(i) => {
                    w.map(6);
                    // Como lo llama la maquina, sin traducir (P4).
                    w.text("kind");
                    w.text(i.kind);
                    w.text("address");
                    w.uint(i.base);
                    w.text("address_width");
                    w.uint(i.address_width as u64);
                    // Si esta traduciendo **ahora**, preguntado al silicio. Que
                    // la maquina tenga IOMMU no quiere decir que el kernel se lo
                    // este programando: decir una cosa por la otra dejaria al
                    // agente escribiendo drivers contra una garantia que no hay.
                    w.text("enabled");
                    w.bool(p.iommu_enabled());
                    // Cuantos permisos hay declarados ahora mismo (D14).
                    w.text("grants");
                    w.uint(dma::count() as u64);
                    // Y lo que el silicio anoto: un DMA negado no se pierde,
                    // queda contado. Es P5 aplicado a lo que hacen los aparatos.
                    w.text("faults");
                    match p.dma_faults() {
                        None => w.null(),
                        Some(f) => w.uint(f),
                    }
                }
            }
        }
        if q.clock {
            // El reloj de la maquina (deuda 17). Se publica porque el agente lo
            // va a necesitar por lo mismo que lo necesita el kernel: sin saber a
            // que ritmo sube el contador, dos lecturas son una diferencia y no
            // un tiempo.
            w.text("clock");
            match p.clock() {
                // Que no haya se dice: es lo que hace que el agente sepa que
                // tiene que medir de otra forma, en vez de creer un numero mal
                // calculado (P4).
                None => w.null(),
                Some(c) => {
                    w.map(3);
                    // Como lo llama la maquina, sin traducir (D3).
                    w.text("kind");
                    w.text(c.kind);
                    w.text("hz");
                    w.uint(c.hz);
                    // Y una lectura, para que se pueda empezar a medir sin otro
                    // viaje de ida y vuelta.
                    w.text("ticks");
                    w.uint(p.ticks());
                }
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
        Some(bytes) => emit(p, bytes),
        // Si no entro, se avisa. Quedarse callado dejaria al cliente esperando
        // una respuesta que no va a llegar nunca.
        None => reply_error(p, id, "response too large"),
    }
}

/// El indice: que hay para pedir, y cuanto de cada cosa.
fn write_index(
    w: &mut Writer<'_>,
    m: &Machine,
    hw: &Hardware,
    arch: &str,
    clock: Option<crate::platform::Clock>,
) {
    w.map(14);

    w.text("arch");
    w.text(arch);

    w.text("sections");
    w.array(13);
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
    w.text("iommu");
    w.text("clock");
    w.text("cable");

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

    // Si esta maquina tiene quien controle lo que tocan los aparatos (D8).
    w.text("iommu");
    match hw.iommu {
        None => w.null(),
        Some(i) => w.text(i.kind),
    }

    // Y si dice a que ritmo sube su contador (deuda 17).
    w.text("clock");
    match clock {
        None => w.null(),
        Some(c) => w.uint(c.hz),
    }
}

/// Con que privilegio puede correr el codigo del agente, y como vuelve (D27).
///
/// El kernel ofrece los dos y no elige: elegir es del agente (P6). Lo que si
/// hace es **publicar el acuerdo**, para que no lo tenga horneado (P4).
fn write_exec<P: Platform>(p: &mut P, w: &mut Writer<'_>) {
    w.map(9);

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

    // Y la otra puerta: **pedir un verbo y seguir corriendo**. Es lo que le
    // permite a codigo sin privilegio hablar el protocolo sin nadie del otro
    // lado, y por eso el blob puede correr `supervised` en vez de `raw`.
    //
    // Los cuatro argumentos van por los registros que publica `arguments`: la
    // direccion del pedido, cuanto mide, donde dejar la respuesta y cuanto
    // entra ahi. Vuelve cuanto ocupa la respuesta, o cero si no entro.
    w.text("service");
    w.bytes(P::EXEC_SERVICE);

    // Y de donde sale la pila: del final del mismo reclamo. Se dice porque es
    // memoria que el agente tiene que dejarle libre a su propio codigo.
    w.text("stack");
    w.text("claim-end");

    // Con que modo corre el nucleo que atiende, si no se pide otro nucleo
    // (D29). Se publica en vez de dejar que el agente lo descubra chocandose:
    // que `raw` exista pero no en cualquier lado es justo lo que no se puede
    // adivinar desde afuera (P4).
    w.text("this_core");
    w.text("supervised");

    // Y que registros se pueden poner al arrancar. No son todos los que hay:
    // donde empieza a ejecutar lo dice `off`, la pila la pone el kernel, y el
    // registro de estado es consecuencia de como se entra, no un valor que se
    // cargue. Se publica en vez de que el agente lo descubra chocandose (P4).
    w.text("initial");
    w.array(P::EXEC_INITIAL.len());
    for name in P::EXEC_INITIAL {
        w.text(name);
    }

    // Y por cuales de esos pasan los argumentos, en orden. En el primero llega
    // la direccion de entrada; un blob recibe ademas en el segundo la direccion
    // de la ventanilla con la que le habla al kernel (D18). Van los nombres y no
    // los indices porque un nombre es lo que el agente puede pedir (D3).
    w.text("arguments");
    w.array(P::ARGUMENTS.len());
    for i in P::ARGUMENTS {
        w.text(P::REGISTERS[*i]);
    }

    // Hasta donde llega el kernel para cortar un trabajo que no vuelve, en esta
    // maquina. Cambia lo que el agente puede planear: donde alcanza solo a los
    // que no enmascararon, correr `raw` con las interrupciones tapadas es
    // apostar un nucleo a que el codigo termine (D29).
    w.text("cancel");
    w.text(if P::CAN_STOP_CORES { "even-if-masked" } else { "only-if-unmasked" });

    // Y si se puede declarar un plazo. Depende de que la maquina diga a que
    // ritmo sube su contador: sin eso, "cien milisegundos" no se puede traducir
    // a nada que el silicio entienda (deuda 17).
    w.text("deadline");
    w.bool(p.deadline_ready());
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
    let origin = unsafe { ORIGIN };

    if origin == Origin::Blob {
        // SAFETY: el buffer lo dio el blob en la llamada y vive mientras dure.
        let r = unsafe { &mut *core::ptr::addr_of_mut!(BLOB_REPLY) };
        match r.used.checked_add(bytes.len()) {
            Some(end) if end <= r.cap && !r.overflowed => {
                // SAFETY: recien se comprobo que entra.
                unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), r.at.add(r.used), bytes.len()) };
                r.used = end;
            }
            // Acá no hay a dónde caerse: el blob no tiene otro canal, y el cable
            // todavía no es el protocolo. Se anota y la ventanilla devuelve 0.
            _ => r.overflowed = true,
        }
        return;
    }

    if origin == Origin::Mailbox {
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
    /// En que nucleo reclamado correr, para `exec`. Es el handle que devolvio
    /// `core.claim`. Sin esto, corre en el nucleo que atiende el protocolo.
    core: Option<u64>,
    /// Con que valores arrancan los registros, para `exec`. Se guarda **sin
    /// interpretar**: los nombres son los de esta maquina (D3), y quien sabe
    /// cuales son es la arquitectura, no el protocolo.
    regs: Option<&'a [u8]>,
    /// De a cuantos bytes se toca la memoria, para `mem.read` y `mem.write`.
    ///
    /// Existe porque **un registro de dispositivo no es RAM**: muchos solo
    /// aceptan accesos de su ancho exacto y descartan los mas angostos, asi que
    /// leerlos de a un byte devuelve ceros y escribirlos no hace nada. El kernel
    /// no lo adivina —no sabe que hay del otro lado (D4)— y por omision no
    /// cambia nada: uno.
    width: Option<u64>,
    /// Cuanto puede tardar el codigo antes de que se lo corte, en milisegundos.
    ///
    /// **Lo declara el agente**, igual que `mode`: el kernel no tiene una
    /// opinion sobre cuanto puede tardar su codigo, y poner un plazo por
    /// omision seria justo eso. Sin plazo, un `exec` que no vuelve no vuelve —
    /// y en el nucleo del protocolo eso deja la maquina escuchando sin
    /// contestar.
    deadline_ms: Option<u64>,
    /// Que la interrupcion la dispare el aparato **escribiendo en memoria** en
    /// vez de por un cable, para `irq.install`. El numero lo elige el kernel.
    msi: Option<bool>,
    /// Si `exec {core}` espera la respuesta o vuelve enseguida (deuda 13).
    ///
    /// Este **si** tiene valor por omision, a diferencia de `mode`, y la
    /// diferencia no es de comodidad: `mode` declara con que privilegio corre
    /// el codigo del agente —una propiedad del codigo, que solo el puede
    /// decidir (D27)— mientras que `wait` dice como quiere la respuesta el que
    /// pregunta. El kernel no esta eligiendo nada sobre el agente.
    wait: Option<bool>,
    /// Que aparato, para `dma.allow`. Es el numero con el que **el bus** lo
    /// nombra —en PCIe, bus, dispositivo y funcion juntos—, que es lo que el
    /// silicio ve llegar en cada pedido de DMA.
    device: Option<u64>,
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
        core: None,
        regs: None,
        width: None,
        deadline_ms: None,
        msi: None,
        wait: None,
        device: None,
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
            "core" => a.core = Some(r.uint()?),
            "regs" => a.regs = Some(r.raw()?),
            "width" => a.width = Some(r.uint()?),
            "deadline_ms" => a.deadline_ms = Some(r.uint()?),
            "msi" => a.msi = Some(r.bool()?),
            "wait" => a.wait = Some(r.bool()?),
            "device" => a.device = Some(r.uint()?),
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

    // Un rango que la maquina no informo y que cae mas arriba de lo que las
    // tablas alcanzan no es un "no": es un aparato al que todavia no llegamos.
    // Se mapea y se reintenta **una** vez. Sin esto el kernel seria la razon por
    // la que no se puede usar el aparato (P1), y en aarch64 lo era: los BARs de
    // PCIe caen en 512 GiB y el mapa que da el firmware llega a 257.
    let mut result = claims::claim(m, request);
    if let (Err(claims::Error::Unmapped), Some(at)) = (&result, a.at) {
        let first = at / crate::paging::GIB * crate::paging::GIB;
        let last = at.saturating_add(bytes).div_ceil(crate::paging::GIB) * crate::paging::GIB;
        // SAFETY: el rango esta fuera de lo mapeado —es justo por eso que
        // fallo— asi que no se le pisan los atributos a nada en uso.
        if unsafe { p.map_device(first, last - first) }.is_ok()
            && crate::paging::note_mapped(first, last)
        {
            result = claims::claim(m, request);
        }
    }

    match result {
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

    let width = a.width.unwrap_or(1);
    match claims::range_of(handle, off, len) {
        Err(e) => reply_failure(p, id, e),
        Ok(addr) => {
            if let Err(e) = check_width(width, addr, len) {
                return reply_error(p, id, e);
            }
            let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
            let mut w = Writer::new(out);
            w.array(3);
            w.uint(id);
            w.bool(true);
            w.map(1);
            w.text("bytes");
            // Volatil y del ancho que pidio el agente: esto puede ser el
            // registro de un dispositivo, no RAM. Se lee **una vez por palabra**
            // y se reparte en bytes, porque volver a leer el mismo registro para
            // sacarle el byte siguiente serian varios accesos — y hay registros
            // que cambian de valor con solo mirarlos.
            //
            // Y **con red**: la maquina puede rechazar el acceso, y ahi el
            // fault vuelve como respuesta en vez de dejar el cordon sin nadie
            // del otro lado (P5, deuda 16).
            let mut refused = false;
            let mut word = 0u64;
            w.bytes_by(len as usize, |i| {
                let inside = i as u64 % width;
                if inside == 0 && !refused {
                    // SAFETY: la direccion salio de un reclamo vigente y el
                    // ancho ya se comprobo contra la alineacion.
                    match unsafe { p.guarded_read(addr + i as u64, width) } {
                        Some(v) => word = v,
                        None => refused = true,
                    }
                }
                (word >> (8 * inside)) as u8
            });
            if refused {
                return reply_refused(p, id, addr);
            }
            finish_reply(p, id, w);
        }
    }
}

/// Que el ancho pedido sea uno que la maquina sepa hacer, y que el rango le
/// cierre.
///
/// Un acceso desalineado o de un ancho raro no falla parejo: en algunas
/// maquinas anda, en otras da fault, y en un registro de dispositivo puede
/// hacer media escritura. Se rechaza antes en vez de dejar que la diferencia
/// aparezca como un bug de una sola arquitectura.
fn check_width(width: u64, addr: u64, len: u64) -> Result<(), &'static str> {
    if !matches!(width, 1 | 2 | 4 | 8) {
        return Err("width must be 1, 2, 4 or 8");
    }
    if len % width != 0 {
        return Err("length must be a multiple of width");
    }
    if addr % width != 0 {
        return Err("address must be aligned to width");
    }
    Ok(())
}

/// La maquina rechazo el acceso. Se contesta con el fault que lo dice.
///
/// No es un error del pedido —el rango era legitimo y el ancho valido—: es lo
/// que contesto el silicio, y por eso viaja con la misma forma que cualquier
/// otro fault. Antes de esto, este camino no contestaba nada: en aarch64 se
/// llevaba puesto el cordon (deuda 16).
fn reply_refused<P: Platform>(p: &mut P, id: u64, addr: u64) {
    let fault = p.last_fault();
    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
    let mut w = Writer::new(out);
    w.array(3);
    w.uint(id);
    w.bool(false);
    w.map(5);
    w.text("error");
    w.text("access-refused");
    // Que direccion fue: con una lectura larga, la que corto no es la primera.
    w.text("address");
    w.uint(addr);
    w.text("cause");
    match fault {
        None => w.null(),
        Some(f) => w.text(f.cause.code()),
    }
    // Y los numeros crudos con los que la maquina lo dijo, sin traducir. La
    // causa normalizada no alcanza para distinguir un rango que no existe de un
    // aparato que rechazo el ancho: eso vive en el detalle (P4).
    w.text("raw");
    match fault {
        None => w.null(),
        Some(f) => w.uint(f.raw),
    }
    w.text("detail");
    match fault {
        None => w.null(),
        Some(f) => w.uint(f.detail),
    }
    finish_reply(p, id, w);
}

fn mem_write<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>) {
    let Some(a) = read_args(r) else {
        return reply_error(p, id, "malformed arguments");
    };
    let (Some(handle), Some(data)) = (a.handle, a.data) else {
        return reply_error(p, id, "mem.write needs handle and bytes");
    };
    let off = a.off.unwrap_or(0);

    let width = a.width.unwrap_or(1);
    match claims::range_of(handle, off, data.len() as u64) {
        Err(e) => reply_failure(p, id, e),
        Ok(addr) => {
            if let Err(e) = check_width(width, addr, data.len() as u64) {
                return reply_error(p, id, e);
            }
            // De a una palabra del ancho pedido. Partir en bytes lo que el
            // aparato espera entero no es "casi lo mismo": puede descartarse
            // sin avisar, o tomarse como varias escrituras distintas.
            let mut i = 0usize;
            while i < data.len() {
                let mut word = 0u64;
                for k in 0..width as usize {
                    word |= (data[i + k] as u64) << (8 * k);
                }
                // SAFETY: la direccion salio de un reclamo vigente y el ancho ya
                // se comprobo contra la alineacion.
                if !unsafe { p.guarded_write(addr + i as u64, width, word) } {
                    return reply_refused(p, id, addr + i as u64);
                }
                i += width as usize;
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

fn release<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>, hw: &Hardware) {
    let Some(a) = read_args(r) else {
        return reply_error(p, id, "malformed arguments");
    };
    let Some(handle) = a.handle else {
        return reply_error(p, id, "release needs handle");
    };

    // Un handle puede ser de memoria o de un nucleo, y salen del mismo contador
    // (ver `handles`), asi que no hay ambiguedad: es uno o el otro.
    //
    // Soltar un nucleo no lo apaga: sigue vivo y durmiendo en su buzon, y por
    // eso se lo puede volver a reclamar sin arrancarlo de nuevo. Lo que se
    // devuelve es el derecho a mandarle trabajo.
    if let Some(slot) = cores::slot_of(handle) {
        // Si tiene trabajo en curso se le pide que corte. Es el unico camino
        // que hay para recuperar un nucleo cuyo codigo no vuelve: se le manda
        // una interrupcion y el handler lo desvia al punto de recuperacion de
        // `exec`, igual que un fault.
        //
        // Si no contesta, el `release` **falla y lo dice**. Decir que se
        // recupero un nucleo que sigue corriendo codigo de otro seria lo peor
        // de los dos mundos: el agente lo reclamaria de nuevo creyendo que esta
        // limpio.
        // SAFETY: la ranura es de un nucleo vivo — tiene un handle vigente.
        if work::is_busy(slot) && !unsafe { work::cancel(p, slot) } {
            cores::settle(slot, cores::State::Lost);
            return reply_core_failure(p, id, cores::Error::DidNotStop);
        }
        return match cores::release(handle) {
            Err(e) => reply_core_failure(p, id, e),
            Ok(_) => {
                // Y se vacia el buzon: un resultado viejo que sobreviviera al
                // `release` aparecería en el proximo reclamo como si fuera suyo.
                work::forget(slot);
                let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
                let mut w = Writer::new(out);
                w.array(3);
                w.uint(id);
                w.bool(true);
                w.map(2);
                w.text("released");
                w.uint(handle);
                // Que siga vivo es lo que hace que reclamarlo otra vez sea
                // barato, asi que se dice.
                w.text("core");
                w.bool(true);
                finish_reply(p, id, w);
            }
        };
    }

    // Si era alcanzable sin privilegio, se le saca el permiso antes de soltarla:
    // memoria devuelta que sigue marcada seria un agujero silencioso.
    if let Some(c) = claims::get(handle) {
        if c.user {
            // SAFETY: el rango sigue siendo el del reclamo, alineado al bloque.
            let _ = unsafe { p.set_user_access(c.start, c.bytes, false) };
        }
    }

    // Y lo mismo con los aparatos: lo que se les permitio tocar se les quita
    // aca. Si quedara, el proximo reclamo caeria en memoria que un aparato
    // todavia alcanza, y eso no da fault — da memoria distinta (D8).
    let mut revoked: [Option<dma::Grant>; dma::MAX] = [None; dma::MAX];
    let mut n = 0;
    dma::forget(handle, |g| {
        if n < revoked.len() {
            revoked[n] = Some(g);
            n += 1;
        }
    });
    for g in revoked.iter().flatten() {
        // SAFETY: el rango sigue siendo el del reclamo, que todavia no se solto.
        let _ = unsafe { p.set_dma_access(hw, g.device, g.start, g.bytes, false) };
    }

    // Y si esa memoria era el buzon, el kernel deja de escuchar ahi. Es la
    // misma razon que las dos revocaciones de arriba, y faltaba: un buzon que
    // sobreviviera al `release` seguiria siendo leido **y escrito** por el
    // kernel en memoria que ya no es de nadie — y el proximo `mem.claim` se la
    // daria a otro, que veria sus bytes interpretados como pedidos y sus
    // paginas pisadas con respuestas. `listen` entrega; `release` devuelve.
    //
    // Va por handle y no a ciegas, que es por lo que `forget` lo pide: si el
    // agente entrego un buzon nuevo sin soltar el viejo, soltar el viejo no
    // puede llevarse puesto al que esta en uso.
    channel::forget(handle);

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
    w.map(6);
    w.text("handle");
    w.uint(c.handle);
    w.text("start");
    w.uint(c.start);
    w.text("bytes");
    w.uint(c.bytes);
    // De que clase era la region: el agente decide con el dato a la vista.
    w.text("kind");
    w.text(c.kind.code());
    // Si se puede cachear, segun lo que informo la maquina (deuda 11). Va por
    // separado de la clase porque no se deduce de ella: hay memoria reservada
    // que igual es cacheable. Y el agente lo necesita para escribir un driver —
    // cachear un registro es que el aparato no se entere de la escritura.
    w.text("caching");
    w.text(match c.caching {
        crate::memory::Caching::WriteBack => "write-back",
        crate::memory::Caching::Uncacheable => "uncacheable",
        crate::memory::Caching::Unknown => "unknown",
    });
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
/// **Y donde corre lo elige el agente:** con `core` —el handle que devolvio
/// `core.claim`— el trabajo se le deja en el buzon a ese nucleo y este se queda
/// esperando la respuesta. Sin `core`, corre en el nucleo que atiende el
/// protocolo, que es lo que hacia siempre.
///
/// Lo que todavia no hace: recibir un estado inicial de registros. El codigo
/// recibe en el primer registro de argumento su propia direccion, para poder
/// encontrar sus datos sin depender de donde lo hayan cargado.
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

    // Con que valores arrancan los registros. Los nombres son los de esta
    // maquina, asi que se resuelven contra los que ella publica (D3).
    let mut initial = [None; MAX_REGISTERS];
    if let Some(raw) = a.regs {
        if let Err(e) = read_initial::<P>(raw, &mut initial) {
            return reply_error(p, id, e);
        }
    }

    // **En el nucleo del protocolo manda el kernel** (D29). Para que eso sea
    // verdad y no una intencion, el agente no puede *poder* enmascarar las
    // interrupciones ahi — y enmascarar es privilegiado. Asi que en este nucleo
    // corre supervisado, y si quiere el privilegio entero reclama un nucleo:
    // ahi la prioridad la decide el, incluido no ser molestado.
    //
    // No es el kernel eligiendo por el agente (P6): los dos modos siguen
    // estando y los dos se pueden usar. Lo que cambia es **donde**. Exigirlo
    // antes habria dejado a `raw` sin ningun lugar donde correr, porque `exec`
    // corria siempre aca; desde que elige nucleo, ya no.
    if !supervised && a.core.is_none() {
        return reply_error(p, id, "exec raw needs a core: the protocol core only runs supervised");
    }

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
    let outcome = match a.core {
        // En otro nucleo: se le deja el trabajo en el buzon y se espera. El
        // nucleo del protocolo no corre nada del agente aca, solo espera — y
        // con tope, asi que el cordon no se pierde si el otro no contesta.
        Some(handle) => {
            let job = work::Job { entry, region: (c.start, c.bytes), supervised, initial };

            // Sin esperar: se deja el trabajo y se contesta enseguida (deuda
            // 13). El resultado se busca despues con `describe {what:["cores"]}`
            // — el nucleo corre un trabajo por vez, asi que su handle **ya
            // identifica el trabajo** y no hace falta inventarle otro.
            if !a.wait.unwrap_or(true) {
                return match unsafe { work::submit(p, handle, job) } {
                    Err(e) => reply_error(p, id, e.code()),
                    Ok(_) => reply_exec(p, id, supervised, a.core, None),
                };
            }

            // Esperando **con los timbres abiertos**. En el nucleo del
            // protocolo la interrupcion tiene prioridad (D29), y esperar a otro
            // nucleo no es una excepcion: si se esperara sordo, un handler que
            // el agente instalo no correria mientras dura el trabajo, y el
            // cordon tampoco se atenderia. Lo destapo la propia regla de D29:
            // el codigo que dispara una interrupcion pasa a correr en el nucleo
            // reclamado, y del otro lado no habia quien la atendiera.
            p.set_interrupts(true);
            let r = unsafe { work::run_on(p, handle, job, a.deadline_ms) };
            p.set_interrupts(false);
            match r {
                Err(e) => return reply_error(p, id, e.code()),
                Ok(o) => o,
            }
        }
        // En este mismo. En el nucleo del kernel la interrupcion tiene
        // prioridad sobre el codigo del agente (D29): se prenden los timbres
        // mientras corre, asi un `exec` largo no deja al cordon sin atender. El
        // bucle vuelve a apagarlos al salir porque su diseno depende de eso.
        None => {
            // El plazo que declaro el agente, si declaro uno. Se traduce a
            // pasos del contador aca —donde se sabe a que ritmo sube— y el
            // reloj de este nucleo avisa cuando llega.
            let until = a
                .deadline_ms
                .and_then(|ms| p.clock().map(|c| p.ticks().wrapping_add(c.ticks_for_ms(ms))));
            // SAFETY: la captura de excepciones esta puesta desde el arranque.
            unsafe { p.set_deadline(until) };

            p.set_interrupts(true);
            let o = unsafe { p.exec(entry, (c.start, c.bytes), supervised, &initial) };
            p.set_interrupts(false);

            // Y desarmarlo: un plazo que sobreviva al `exec` avisaria en el
            // medio del bucle del protocolo, donde no hay nada que cortar.
            // SAFETY: idem.
            unsafe { p.set_deadline(None) };
            o
        }
    };

    reply_exec(p, id, supervised, a.core, Some(outcome));
}

/// Lo mas largo que puede ser `REGISTERS`. aarch64 tiene 34; el margen es para
/// que agregar uno no sea un cambio en dos lugares.
pub(crate) const MAX_REGISTERS: usize = 40;

/// Resuelve `{nombre: valor}` contra los registros que informa esta maquina.
///
/// Un nombre que no existe **se rechaza** en vez de ignorarse: el agente pidio
/// algo concreto, y correr su codigo con un registro sin poner seria hacer algo
/// distinto de lo que pidio sin decirselo. Lo mismo con uno que existe pero no
/// se puede poner — la lista de cuales se pueden la publica `describe`.
fn read_initial<P: Platform>(
    raw: &[u8],
    out: &mut [Option<u64>; MAX_REGISTERS],
) -> Result<(), &'static str> {
    let mut r = Reader::new(raw);
    let Some(pairs) = r.map() else {
        return Err("regs must be a map of register name to value");
    };
    for _ in 0..pairs {
        let Some(name) = r.text() else {
            return Err("register names must be text");
        };
        let Some(value) = r.uint() else {
            return Err("register values must be unsigned integers");
        };
        if !P::EXEC_INITIAL.contains(&name) {
            return Err("that register cannot be set: see describe exec");
        }
        let Some(i) = P::REGISTERS.iter().position(|x| *x == name) else {
            return Err("no such register on this machine");
        };
        if i >= MAX_REGISTERS {
            return Err("register out of range");
        }
        out[i] = Some(value);
    }
    Ok(())
}

/// La respuesta de `exec`, igual haya terminado o recien empezado.
///
/// Una sola forma a proposito: el agente no tiene que leer dos respuestas
/// distintas segun como pidio. `state` dice cual de las dos es, y lo que
/// todavia no existe viene en nulo en vez de faltar — un campo ausente y uno
/// vacio se parecen demasiado del lado del que parsea.
fn reply_exec<P: Platform>(
    p: &mut P,
    id: u64,
    supervised: bool,
    core: Option<u64>,
    outcome: Option<Outcome>,
) {
    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
    let mut w = Writer::new(out);
    w.array(3);
    w.uint(id);
    // `true`: el pedido se atendio. Que el codigo haya fallado no es un fallo
    // del pedido — es su resultado, y va adentro.
    w.bool(true);

    w.map(7);

    // Con que privilegio corrio de verdad, y donde. Se devuelven aunque el
    // agente los acabe de mandar: la respuesta tiene que poder leerse sola.
    w.text("mode");
    w.text(if supervised { "supervised" } else { "raw" });

    w.text("core");
    match core {
        Some(h) => w.uint(h),
        None => w.null(),
    }

    w.text("state");
    w.text(if outcome.is_some() { "done" } else { "running" });

    match outcome {
        None => {
            w.text("faulted");
            w.null();
            w.text("cancelled");
            w.null();
            w.text("registers");
            w.null();
            w.text("fault");
            w.null();
        }
        Some(o) => write_outcome::<P>(&mut w, &o),
    }

    finish_reply(p, id, w);
}

/// Lo que dejo un `exec`: si fallo o lo cortaron, los registros y el fault.
///
/// Escribe **cuatro pares** en el mapa que ya empezo quien llama.
fn write_outcome<P: Platform>(w: &mut Writer<'_>, outcome: &Outcome) {
    w.text("faulted");
    w.bool(outcome.faulted);

    // Si no termino solo ni por un fault, sino porque se lo interrumpio. Va
    // aparte de `faulted` a proposito: el codigo no hizo nada mal, se lo
    // cortaron, y para el que depura eso es informacion distinta.
    w.text("cancelled");
    w.bool(outcome.cancelled);

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
}

// ---------------------------------------------------------------------------
// dma.allow
// ---------------------------------------------------------------------------

/// Declara que memoria puede tocar un aparato por su cuenta (D8).
///
/// Es el unico verbo cuyo efecto no se ve desde el CPU: lo que cambia es lo que
/// el **aparato** alcanza cuando escribe solo. Y no es un guardarrail — el
/// kernel no elige nada, hace cumplir lo que el agente declaro (P6). Lo que si
/// hace es no dejarlo abierto por las dudas: sin nada declarado, un aparato no
/// llega a ninguna parte.
///
/// El permiso queda anotado para poder **sacarlo** al soltar la memoria. Un
/// reclamo devuelto que un aparato sigue alcanzando es justo el agujero
/// silencioso que el IOMMU viene a cerrar: el proximo reclamo cae ahi mismo.
fn dma_allow<P: Platform>(p: &mut P, id: u64, r: &mut Reader<'_>, hw: &Hardware) {
    let Some(a) = read_args(r) else {
        return reply_error(p, id, "malformed arguments");
    };
    let (Some(device), Some(handle)) = (a.device, a.handle) else {
        return reply_error(p, id, "dma.allow needs device and handle");
    };
    let Some(c) = claims::get(handle) else {
        return reply_failure(p, id, claims::Error::NoSuchHandle);
    };

    // SAFETY: el rango salio de un reclamo vigente, asi que esta mapeado.
    if let Err(e) =
        unsafe { p.set_dma_access(hw, device as u32, c.start, c.bytes, true) }
    {
        return reply_error(p, id, e);
    }

    match dma::record(dma::Grant { device: device as u32, handle, start: c.start, bytes: c.bytes })
    {
        // Que ya estuviera no es un fallo del pedido: el silicio quedo como el
        // agente pidio, que es lo unico que importa.
        Err(dma::Error::Already) => {}
        Err(e) => return reply_error(p, id, e.code()),
        Ok(()) => {}
    }

    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
    let mut w = Writer::new(out);
    w.array(3);
    w.uint(id);
    w.bool(true);
    w.map(3);
    w.text("device");
    w.uint(device);
    w.text("start");
    w.uint(c.start);
    w.text("bytes");
    w.uint(c.bytes);
    finish_reply(p, id, w);
}

// ---------------------------------------------------------------------------
// core.claim
// ---------------------------------------------------------------------------

/// Cuanto se espera a que un nucleo avise que llego.
///
/// Arrancar un nucleo tarda microsegundos, asi que medio segundo es generoso de
/// sobra — y esperar de mas solo cuesta tiempo la unica vez que no arranca.
/// Antes esto eran doscientos millones de vueltas, un numero que no se podia
/// explicar porque no decia cuanto se estaba dispuesto a esperar.
const WAIT_MS: u64 = 500;
/// Y el tope en vueltas, para la maquina que no diga a que ritmo sube su reloj.
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

    // Si ese nucleo ya arranco —porque se reclamo antes y se solto— **no se
    // vuelve a arrancar**: sigue vivo y durmiendo en su buzon, con sus tablas y
    // su captura de faults puestas. Arrancarlo otra vez seria resetearlo, y lo
    // que el agente pidio es usarlo.
    if cores::has_arrived(slot) {
        cores::settle(slot, cores::State::Idle);
        let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
        let mut w = Writer::new(out);
        w.array(3);
        w.uint(id);
        w.bool(true);
        write_core::<P>(&mut w, &cores::Core { handle, id: request, state: cores::State::Idle });
        return finish_reply(p, id, w);
    }

    // SAFETY: las tablas de paginas y la captura de faults ya estan puestas;
    // el nucleo nuevo copia esa configuracion.
    if let Err(e) = unsafe { p.start_core(hw, request, slot) } {
        cores::settle(slot, cores::State::Failed);
        return reply_core_failure(p, id, e);
    }

    // Que el pedido se haya hecho no significa que el nucleo este vivo: son dos
    // CPUs distintas y una no puede afirmar por la otra. Se espera a que avise.
    crate::platform::wait_until(p, WAIT_MS, WAIT_ROUNDS, || cores::has_arrived(slot));

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
    write_core::<P>(&mut w, &cores::Core { handle, id: request, state: cores::State::Idle });
    finish_reply(p, id, w);
}

/// Un nucleo reclamado, y en que anda el trabajo que se le mando.
///
/// `work` es donde el agente busca el resultado de un `exec {wait:false}`
/// (deuda 13). Va aca y no en un verbo nuevo porque **es estado de la maquina**,
/// y `describe` es donde la maquina cuenta lo que es (P4). Ademas sobrevive a
/// la desconexion, igual que los reclamos (D14): el agente puede mandar algo
/// largo, irse, y volver a buscarlo.
fn write_core<P: Platform>(w: &mut Writer<'_>, c: &cores::Core) {
    w.map(4);
    w.text("handle");
    w.uint(c.handle);
    w.text("id");
    w.uint(c.id);
    w.text("state");
    w.text(c.state.code());

    w.text("work");
    match work::progress(c.handle) {
        // Nunca corrio nada. No es lo mismo que "termino sin resultado".
        None => w.null(),
        Some(work::Progress::Running) => {
            w.map(1);
            w.text("state");
            w.text("running");
        }
        Some(work::Progress::Done(o)) => {
            w.map(5);
            w.text("state");
            w.text("done");
            write_outcome::<P>(w, &o);
        }
    }
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
/// El estado del cordon umbilical.
///
/// Existe por una razon concreta: el kernel puede **perder bytes** si le llegan
/// mas rapido de lo que los saca, y hasta que esto se publico los perdia en
/// silencio. Un pedido al que le falta un byte se ve como CBOR malformado o
/// como una maquina colgada, y sin este numero es un misterio (P4).
fn write_cable(w: &mut Writer<'_>) {
    w.map(2);
    w.text("dropped");
    w.uint(crate::serial::dropped());
    w.text("buffer");
    w.uint(crate::serial::capacity() as u64);
}

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
    let Some(handle) = a.handle else {
        return reply_error(p, id, "irq.install needs handle");
    };
    // Con `msi`, el numero **lo elige el kernel**: es un recurso de la maquina y
    // el agente no tiene como saber cual esta libre. Sin `msi`, el agente dice
    // cual quiere, que es como se pide una interrupcion por cable.
    let by_write = a.msi.unwrap_or(false);
    if !by_write && a.interrupt.is_none() {
        return reply_error(p, id, "irq.install needs interrupt, or msi to let the kernel pick");
    }
    let off = a.off.unwrap_or(0);

    // La entrada tiene que estar adentro de un reclamo vigente. Un byte alcanza:
    // hasta donde llega el handler lo sabe el handler.
    let entry = match claims::range_of(handle, off, 1) {
        Err(e) => return reply_failure(p, id, e),
        Ok(addr) => addr,
    };

    // Con MSI todavia no se sabe el numero —lo devuelve la arquitectura— asi
    // que la ranura se toma con cero y se corrige apenas se sepa.
    let asked = a.interrupt.unwrap_or(0) as u32;
    let slot = match handlers::reserve(asked, entry, raw) {
        Err(e) => return reply_error(p, id, e.code()),
        Ok(s) => s,
    };

    // SAFETY: la entrada esta dentro de un reclamo y el identity map cubre todo.
    let installed = if by_write {
        unsafe { p.install_msi(hw, slot, raw) }
    } else {
        unsafe { p.install_irq(hw, asked, slot, raw) }.map(|d| (asked, d))
    };
    match installed {
        Err(e) => {
            // Si la arquitectura no pudo, la ranura se suelta: dejarla tomada
            // haria que el proximo intento diga "ya instalado" por nada.
            handlers::release_slot(slot);
            reply_error(p, id, e.code())
        }
        Ok((interrupt, trigger)) => {
            // Con MSI el numero lo puso la arquitectura, asi que la ranura se
            // corrige ahora: el reparto la busca por ese numero.
            handlers::set_interrupt(slot, interrupt);
            handlers::set_trigger(slot, trigger);
            let interrupt = interrupt as u64;
            let out = unsafe { &mut *core::ptr::addr_of_mut!(OUTBOX) };
            let mut w = Writer::new(out);
            w.array(3);
            w.uint(id);
            w.bool(true);
            w.map(4);
            w.text("interrupt");
            w.uint(interrupt);
            w.text("entry");
            w.uint(entry);
            w.text("raw");
            w.bool(raw);
            // Y la escritura que la dispara, aca mismo. Con MSI es lo que el
            // agente necesita **para configurar su aparato**, asi que hacerlo ir
            // a buscarla a `describe` seria un viaje de ida y vuelta por un dato
            // que este pedido acaba de decidir.
            w.text("trigger");
            let t = handlers::at(slot).map(|h| h.trigger).unwrap_or_else(channel::Doorbell::blank);
            w.array(t.count);
            for (addr, value, width) in &t.writes[..t.count] {
                w.array(3);
                w.uint(*addr);
                w.uint(*value);
                w.uint(*width as u64);
            }
            finish_reply(p, id, w);
        }
    }
}
