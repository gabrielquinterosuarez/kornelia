//! El trabajo que se le manda a un nucleo reclamado (D13, D29).
//!
//! # Por que hace falta
//!
//! `core.claim` arranca un nucleo, pero hasta que existio esto el nucleo
//! arrancaba, se configuraba solo y se quedaba esperando algo que nadie le
//! podia mandar: `exec` corria siempre en el nucleo que atiende el protocolo.
//! Reclamar un nucleo era reservarlo, no usarlo.
//!
//! # La forma
//!
//! Un buzon por nucleo, con un solo escritor y un solo lector cada uno: el
//! nucleo del protocolo deja el pedido, el nucleo reclamado lo levanta, corre,
//! y deja la respuesta. Nadie mas toca esa ranura, asi que no hace falta
//! candado — alcanza con **decir explicitamente que se sincroniza**.
//!
//! Y eso ultimo no es formalidad. En x86_64 el modelo de memoria es fuerte y
//! esto andaria igual escrito mal; en aarch64 el procesador reordena y no. Es
//! la misma razon por la que D22 pide las dos arquitecturas desde el principio.
//!
//! # El nucleo duerme
//!
//! Entre pedido y pedido el nucleo reclamado **no gira**: se duerme y lo
//! despierta un timbre que le manda el nucleo del protocolo. Girar seria quemar
//! un nucleo entero esperando, que es exactamente lo que el proyecto le saco al
//! nucleo del protocolo cuando el cable serie tuvo timbre.

use crate::cores;
use crate::fault::{Fault, Outcome};
use crate::platform::Platform;
use core::sync::atomic::{AtomicU64, Ordering};

/// En que anda el buzon de una ranura.
///
/// Se mueve en un solo sentido —vacio, pedido, corriendo, contestado, vacio— y
/// cada paso lo da un nucleo distinto, lo que hace que no haya carrera: nadie
/// escribe el estado que el otro esta por leer.
const EMPTY: u64 = 0;
const PENDING: u64 = 1;
const RUNNING: u64 = 2;
const ANSWERED: u64 = 3;

static STATE: [AtomicU64; cores::MAX] = [const { AtomicU64::new(EMPTY) }; cores::MAX];

/// Si hay que interrumpir el codigo que corre en esa ranura.
///
/// Tres estados y no dos, y el tercero es el que hace que se pueda contar: nadie
/// pidio nada, se pidio y todavia no se aplico, y **se aplico** — que es lo que
/// permite distinguir un `exec` cortado de uno que fallo, cuando el handler ya
/// desvio y quien armo la respuesta pregunta que paso.
const CANCEL_NONE: u64 = 0;
const CANCEL_ASKED: u64 = 1;
const CANCEL_DONE: u64 = 2;

/// Una ranura mas que las de `cores`, y esa de mas es la del nucleo que atiende
/// el protocolo.
///
/// Ese nucleo **no se reclama** (es el que esta contestando), asi que no tiene
/// ranura en la tabla de nucleos: su indice es `cores::MAX`, justo el primero
/// que se sale del arreglo. Y ahi hay que poder anotar un corte igual, porque el
/// plazo de `exec {deadline_ms}` corre sobre todo en ese nucleo — es donde un
/// codigo que no vuelve deja la maquina escuchando sin contestar.
///
/// **Esto costo encontrarlo.** Con el arreglo del tamano de `cores::MAX`, la
/// marca del corte se descartaba por indice sin decir nada: el desvio ocurria,
/// el codigo se cortaba, y la respuesta salia con el fault viejo del arranque
/// porque nadie habia anotado que fue un corte.
const SLOTS: usize = cores::MAX + 1;

static CANCEL: [AtomicU64; SLOTS] = [const { AtomicU64::new(CANCEL_NONE) }; SLOTS];

/// Pide que se interrumpa lo que corre en esa ranura.
///
/// Solo deja la marca. Quien la hace efectiva es el handler de la interrupcion
/// **en el nucleo objetivo**: desde afuera no se puede desviar la ejecucion de
/// otro nucleo, solo pedirle que se desvie solo.
pub fn ask_cancel(slot: usize) {
    if slot < SLOTS {
        CANCEL[slot].store(CANCEL_ASKED, Ordering::Release);
    }
}

/// Lo llama el handler, en el nucleo objetivo: si habia que cortar, lo anota
/// como hecho y contesta `true` para que el handler desvie.
pub fn take_cancel(slot: usize) -> bool {
    slot < SLOTS
        && CANCEL[slot]
            .compare_exchange(CANCEL_ASKED, CANCEL_DONE, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
}

/// Si el ultimo `exec` de esa ranura termino porque se lo cortaron. Lo consume:
/// la respuesta se arma una sola vez.
pub fn was_cancelled(slot: usize) -> bool {
    slot < SLOTS
        && CANCEL[slot]
            .compare_exchange(CANCEL_DONE, CANCEL_NONE, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
}

/// Anota que a esa ranura la cortaron, sin que nadie lo haya pedido de afuera.
///
/// Lo llama el handler del reloj cuando vence un plazo: no hubo un `release`
/// esperando la respuesta, pero el `exec` termino cortado igual y quien arme la
/// respuesta tiene que poder decirlo.
pub fn mark_cancelled(slot: usize) {
    if slot < SLOTS {
        CANCEL[slot].store(CANCEL_DONE, Ordering::Release);
    }
}

/// Deja la ranura sin pedidos de corte pendientes.
pub fn clear_cancel(slot: usize) {
    if slot < SLOTS {
        CANCEL[slot].store(CANCEL_NONE, Ordering::Release);
    }
}

/// Lo que hay que correr. Es lo mismo que recibe `Platform::exec`.
#[derive(Clone, Copy)]
pub struct Job {
    pub entry: u64,
    pub region: (u64, u64),
    /// Con que privilegio lo declaro el agente (D27).
    pub supervised: bool,
    /// Con que valores arrancan los registros, en el orden de `REGISTERS`.
    ///
    /// Va copiado y no prestado a proposito: el buzon sobrevive al pedido que
    /// lo lleno —de eso se trata `wait:false`— asi que apuntar al buffer de
    /// entrada seria apuntar a algo que el proximo pedido pisa.
    pub initial: [Option<u64>; crate::protocol::MAX_REGISTERS],
}

/// Lo que contesto el nucleo que lo corrio.
#[derive(Clone, Copy)]
struct Answer {
    faulted: bool,
    cancelled: bool,
    /// Apuntan a los arreglos por nucleo del que corrio: son `'static` y solo
    /// los escribe el, asi que se pueden leer de este lado despues de la
    /// sincronizacion.
    regs: &'static [u64],
    fault: Option<Fault>,
}

static mut JOBS: [Job; cores::MAX] =
    [const { Job { entry: 0, region: (0, 0), supervised: false,
        initial: [None; crate::protocol::MAX_REGISTERS] } }; cores::MAX];
static mut ANSWERS: [Answer; cores::MAX] =
    [const { Answer { faulted: false, cancelled: false, regs: &[], fault: None } }; cores::MAX];

/// Por que no se pudo mandar trabajo.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub enum Error {
    /// Ese handle no es de un nucleo reclamado.
    NoSuchCore,
    /// El nucleo esta reclamado pero todavia no llego, o no llego nunca.
    NotReady,
    /// Ya tiene un pedido en curso. No se encola: el agente sabe lo que mando.
    Busy,
    /// Se le mando y no contesto. El nucleo queda marcado y no se le manda mas.
    NoAnswer,
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::NoSuchCore => "no such core",
            Error::NotReady => "core not ready",
            Error::Busy => "core busy",
            Error::NoAnswer => "core did not answer",
        }
    }
}

/// Cuantas vueltas se espera una respuesta antes de darla por perdida.
///
/// Mismo criterio que la espera de `core.claim`: no hay reloj todavia, asi que
/// se cuenta en iteraciones. Es generoso a proposito — lo unico que cuesta
/// esperar de mas es tiempo, y solo la vez que el nucleo no contesta.
///
/// Que exista un tope es lo que hace que un `exec` en otro nucleo no pueda
/// colgar al que atiende el protocolo: el cordon umbilical nunca se pierde por
/// algo que hizo el agente (D5, D17).
const WAIT_ROUNDS: u64 = 200_000_000;

/// Le deja el trabajo a un nucleo y **vuelve enseguida** (deuda 13).
///
/// Es la mitad de abajo de `run_on`, y tambien la forma asincronica completa:
/// el agente manda y despues pregunta con `progress`. Un nucleo corre un trabajo
/// por vez, asi que **el handle del nucleo ya identifica el trabajo** — no hace
/// falta inventarle un handle propio.
///
/// # Safety
///
/// Lo mismo que `Platform::exec`: `job.entry` tiene que apuntar a memoria
/// mapeada y ejecutable. Lo que haya ahi puede ser cualquier cosa (P2).
pub unsafe fn submit<P: Platform>(p: &mut P, handle: u64, job: Job) -> Result<usize, Error> {
    let Some(slot) = cores::slot_of(handle) else {
        return Err(Error::NoSuchCore);
    };
    if !cores::has_arrived(slot) {
        return Err(Error::NotReady);
    }
    reserve(slot)?;

    (*core::ptr::addr_of_mut!(JOBS))[slot] = job;
    // `Release`: el pedido tiene que estar escrito **antes** de que el otro
    // nucleo pueda ver que hay algo. Sin esto, en aarch64 podria ver el estado
    // nuevo y el pedido viejo.
    STATE[slot].store(PENDING, Ordering::Release);

    p.wake_core(cores::id_of(slot).unwrap_or(0));
    Ok(slot)
}

/// Toma la ranura para un pedido nuevo.
///
/// Vale tanto la vacia como la que tiene una respuesta que nadie recogio: un
/// resultado viejo no reserva el nucleo para siempre. Lo que no vale es pisar
/// un trabajo en curso — no se encola, porque el agente ya sabe lo que mando.
fn reserve(slot: usize) -> Result<(), Error> {
    for from in [EMPTY, ANSWERED] {
        if STATE[slot]
            .compare_exchange(from, RUNNING, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return Ok(());
        }
    }
    Err(Error::Busy)
}

/// En que anda el trabajo de ese nucleo, sin consumirlo.
///
/// `None` si nunca corrio nada. Si termino, el resultado **queda ahi** hasta
/// que se le mande otro trabajo: asi el agente puede desconectarse y volver a
/// buscarlo, que es lo mismo que D14 hace con los reclamos.
pub fn progress(handle: u64) -> Option<Progress> {
    let slot = cores::slot_of(handle)?;
    match STATE[slot].load(Ordering::Acquire) {
        EMPTY => None,
        ANSWERED => Some(Progress::Done(answer_of(slot))),
        _ => Some(Progress::Running),
    }
}

/// Lo que se sabe del trabajo de un nucleo.
pub enum Progress {
    /// Se le mando y todavia no contesto.
    Running,
    /// Contesto esto. Sigue disponible hasta el proximo trabajo.
    Done(Outcome),
}

/// La respuesta que dejo el nucleo de esa ranura.
///
/// # Panics
///
/// No: solo se llama con la ranura en `ANSWERED`, y ahi la respuesta ya esta
/// escrita entera — lo garantiza el `Release` del que la escribio.
fn answer_of(slot: usize) -> Outcome {
    let a = unsafe { (*core::ptr::addr_of!(ANSWERS))[slot] };
    Outcome { faulted: a.faulted, regs: a.regs, fault: a.fault, cancelled: a.cancelled }
}

/// Le manda trabajo a un nucleo reclamado y **espera** la respuesta.
///
/// # Safety
///
/// Lo mismo que `submit`.
pub unsafe fn run_on<P: Platform>(p: &mut P, handle: u64, job: Job) -> Result<Outcome, Error> {
    let slot = submit(p, handle, job)?;

    let mut rounds = 0u64;
    while STATE[slot].load(Ordering::Acquire) != ANSWERED {
        rounds += 1;
        if rounds > WAIT_ROUNDS {
            // No se limpia el buzon: el nucleo podria despertarse tarde y
            // escribir una respuesta encima. Queda marcado como perdido, y esa
            // ranura no recibe mas trabajo.
            cores::settle(slot, cores::State::Failed);
            return Err(Error::NoAnswer);
        }
        core::hint::spin_loop();
    }

    // La ranura queda en `ANSWERED` y no se vacia: el resultado tiene que
    // seguir estando para el que pregunte despues por `describe`. La libera el
    // proximo trabajo, no el que la lee.
    Ok(answer_of(slot))
}

/// El bucle de un nucleo reclamado: dormir, despertarse, correr, contestar.
///
/// No vuelve. Es lo ultimo que llama un nucleo despues de configurarse.
///
/// # Safety
///
/// Corre en el nucleo de esa ranura y en ningun otro, con la captura de faults
/// y el bloque privado ya instalados.
pub unsafe fn serve<P: Platform>(p: &mut P, slot: usize) -> ! {
    loop {
        // Se mira el buzon con los timbres cerrados, y si no hay nada se
        // duerme sin abrirlos en el medio. Ese hueco es justo donde se
        // perderia un despertador: llega el timbre entre el "no hay nada" y el
        // "me duermo", y el nucleo se duerme para siempre con trabajo puesto.
        p.set_interrupts(false);

        if STATE[slot].load(Ordering::Acquire) == PENDING {
            let job = (*core::ptr::addr_of!(JOBS))[slot];
            STATE[slot].store(RUNNING, Ordering::Relaxed);

            // Con los timbres abiertos: este es un nucleo del agente, no el del
            // protocolo, asi que si su codigo los cierra esta en su derecho
            // (D29). El precio tambien es suyo: un nucleo que se queda sordo no
            // recibe el proximo trabajo.
            p.set_interrupts(true);
            let outcome = p.exec(job.entry, job.region, job.supervised, &job.initial);

            (*core::ptr::addr_of_mut!(ANSWERS))[slot] =
                Answer {
                    faulted: outcome.faulted,
                    cancelled: outcome.cancelled,
                    regs: outcome.regs,
                    fault: outcome.fault,
                };
            // `Release`: la respuesta entera tiene que estar escrita antes de
            // que el otro nucleo pueda verla contestada.
            STATE[slot].store(ANSWERED, Ordering::Release);
            continue;
        }

        p.sleep();
    }
}

/// Cuanto se espera a que un nucleo se deje cortar, en milisegundos.
///
/// Es el mismo criterio que usa Linux para lo mismo —un segundo para el IPI
/// normal, diez milisegundos para el que no se puede enmascarar— pero mas corto:
/// aca el que espera es el nucleo que sostiene el cordon umbilical, y hacerlo
/// esperar un segundo es dejar al agente sin respuesta todo ese rato.
pub const CANCEL_MS: u64 = 50;

/// Le pide a un nucleo que corte lo que esta corriendo, y espera.
///
/// Devuelve `true` si el nucleo se detuvo. `false` si no contesto — y eso no es
/// un fallo del kernel: si el codigo del agente enmascaro las interrupciones,
/// **no hay nada que se pueda hacer desde afuera**, y D29 dice que en su nucleo
/// eso lo decide el. El precio de haber declarado `raw` es suyo.
///
/// # Safety
///
/// El nucleo de esa ranura tiene que estar vivo.
pub unsafe fn cancel<P: Platform>(p: &mut P, slot: usize) -> bool {
    if !is_busy(slot) {
        return true;
    }
    let id = cores::id_of(slot).unwrap_or(0);
    ask_cancel(slot);

    // **Se escala, igual que Linux.** Primero el timbre normal, que alcanza para
    // todo lo que no se tapo los oidos — que es casi todo, porque enmascarar hay
    // que quererlo. Si no contesta, la linea que la mascara comun no tapa.
    p.wake_core(id);
    if wait_until_quiet(p, slot) {
        return true;
    }

    // Segundo escalon. Donde no exista, `stop_core` dice que no en vez de
    // prometer un corte que no llega, y no tiene sentido volver a esperar.
    if !p.stop_core(id) {
        return false;
    }
    wait_until_quiet(p, slot)
}

/// Espera a que la ranura deje de estar ocupada, o se cansa.
///
/// Con reloj se espera un tiempo; sin reloj, vueltas. Igual que la ventana del
/// blob: un plazo que no se sabe cuanto dura no es un plazo.
fn wait_until_quiet<P: Platform>(p: &mut P, slot: usize) -> bool {
    let deadline = p.clock().map(|c| p.ticks().wrapping_add(c.ticks_for_ms(CANCEL_MS)));
    let mut rounds = 0u64;
    while is_busy(slot) {
        match deadline {
            Some(until) if p.ticks() >= until => return false,
            None if rounds > 20_000_000 => return false,
            _ => {}
        }
        rounds += 1;
        core::hint::spin_loop();
    }
    true
}

/// Vacia el buzon de una ranura.
///
/// Se llama al soltar un nucleo: un resultado viejo que sobreviviera al
/// `release` aparecería en el proximo reclamo como si fuera suyo, y el agente
/// leeria la respuesta de un trabajo que no mando.
pub fn forget(slot: usize) {
    if slot < cores::MAX {
        STATE[slot].store(EMPTY, Ordering::Release);
    }
}

/// Deja todos los buzones vacios. Solo para los tests.
#[cfg(test)]
pub fn reset() {
    for s in STATE.iter() {
        s.store(EMPTY, Ordering::Relaxed);
    }
}

/// Si esa ranura tiene un pedido **en curso**. Lo informa `describe`.
///
/// Una respuesta que nadie recogio no cuenta: el nucleo ya termino y esta libre
/// para recibir otra cosa.
pub fn is_busy(slot: usize) -> bool {
    if slot >= cores::MAX {
        return false;
    }
    matches!(STATE[slot].load(Ordering::Relaxed), PENDING | RUNNING)
}
