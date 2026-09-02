//! Correr codigo del agente en aarch64 (P3, P5).
//!
//! Misma idea que en x86_64 —un punto de retorno armado, y el handler desvia el
//! regreso en vez de detener el nucleo— pero la mecanica cambia: aca el que
//! vuelve es `eret`, y lo que hay que reescribir es `ELR_EL1`.
//!
//! Y hay una diferencia mas: en x86 el marco de interrupcion lleva RSP, asi que
//! el handler podria devolver el control con otra pila. Aca no: una excepcion
//! tomada en EL1 usa el mismo SP, y `eret` lo deja como estaba. Por eso el punto
//! de recuperacion se recupera la pila el mismo, con lo cual las dos
//! arquitecturas terminan haciendo lo mismo.
//!
//! # La pila del agente esta aparte (P5)
//!
//! ARM tiene dos punteros de pila bancados en EL1: `SP_EL0` y `SP_EL1`, y
//! `SPSel` elige cual se usa como `sp`.
//!
//! El codigo del agente corre con `SPSel=0`, o sea sobre `SP_EL0`, y el kernel
//! vive en `SP_EL1`. Al tomar una excepcion **el hardware pone `SPSel=1` solo**:
//! el handler entra siempre sobre la pila del kernel, aunque el agente haya
//! dejado la suya en cualquier lado. Es lo mismo que consigue la IST en x86_64,
//! pero sin armar nada — la arquitectura ya lo trae.
//!
//! El unico cuidado es al volver: `eret` saca de `SPSR` con que `SPSel` seguir,
//! y ahi todavia dice `SP_EL0`. Por eso el handler, cuando desvia, tambien le
//! prende ese bit; si no, volveria justo a la pila rota.

use kernel_core::fault::Outcome;

/// 64 KiB de pila de agente para cada nucleo.
const AGENT_STACK_SIZE: usize = 64 * 1024;

#[repr(C, align(16))]
struct AgentStacks([[u8; AGENT_STACK_SIZE]; crate::percpu::SLOTS]);

static mut AGENT_STACKS: AgentStacks =
    AgentStacks([[0; AGENT_STACK_SIZE]; crate::percpu::SLOTS]);

/// Los bytes de `svc #0`, que es lo que `describe` publica como la forma de
/// volver desde `supervised` (D27).
///
/// Desde EL0 un `ret` comun no vuelve al kernel: salta adentro del mismo nivel,
/// a donde diga x30, y ahi no hay codigo del kernel alcanzable. Subir de nivel
/// es una excepcion, y `svc` es la que existe para pedirla a proposito.
pub const RETURN_BYTES: &[u8] = &[0x01, 0x00, 0x00, 0xD4];

core::arch::global_asm!(
    r#"
.section .text
.globl exec_trampoline

// x0 = direccion de entrada, x1 = 1 si va supervisado.
// Devuelve 0 si volvio solo, 1 si hubo fault.
//
// Todo lo que toca esta en el bloque privado de este nucleo, que sale de
// TPIDR_EL1. Los offsets son los del struct `PerCpu`, verificados al compilar.
exec_trampoline:
    stp x29, x30, [sp, #-96]!
    stp x19, x20, [sp, #16]
    stp x21, x22, [sp, #32]
    stp x23, x24, [sp, #48]
    stp x25, x26, [sp, #64]
    stp x27, x28, [sp, #80]

    mrs  x20, tpidr_el1               // el bloque de este nucleo

    // Se arma el punto de recuperacion ANTES de saltar.
    adrp x9,  exec_recovery
    add  x9,  x9, :lo12:exec_recovery
    str  x9,  [x20, #8]               // rip
    mov  x9,  sp
    str  x9,  [x20, #16]              // sp del kernel
    mov  x9,  #1
    str  x9,  [x20, #0]               // armado

    // La pila del agente, en SP_EL0. Desde el `msr spsel, #0` hasta el de
    // vuelta, `sp` es la suya; la nuestra queda intacta en SP_EL1.
    ldr  x9,  [x20, #24]
    msr  sp_el0, x9

    cbnz x1,  exec_supervised
    msr  spsel, #0

    // Los registros que pidio el agente. Se cargan **todos** desde el bloque —el
    // lado de Rust ya resolvio cual queda en cero y cual lleva la direccion de
    // entrada— asi que despues de esto no queda ninguno libre, y por eso el
    // salto sale de x30, que se carga al final con la direccion.
    //
    // `sp` no se carga: la pila la pone el kernel y lo publica `describe`.
    add  x30, x20, #32                // la base de regs
    ldp  x0,  x1,  [x30, #(0 * 8)]
    ldp  x2,  x3,  [x30, #(2 * 8)]
    ldp  x4,  x5,  [x30, #(4 * 8)]
    ldp  x6,  x7,  [x30, #(6 * 8)]
    ldp  x8,  x9,  [x30, #(8 * 8)]
    ldp  x10, x11, [x30, #(10 * 8)]
    ldp  x12, x13, [x30, #(12 * 8)]
    ldp  x14, x15, [x30, #(14 * 8)]
    ldp  x16, x17, [x30, #(16 * 8)]
    ldp  x18, x19, [x30, #(18 * 8)]
    ldp  x21, x22, [x30, #(21 * 8)]
    ldp  x23, x24, [x30, #(23 * 8)]
    ldp  x25, x26, [x30, #(25 * 8)]
    ldp  x27, x28, [x30, #(27 * 8)]
    ldr  x29,      [x30, #(29 * 8)]
    // x20 lleva el bloque y se necesita hasta el final; x30 es el puntero a
    // regs. Los dos se cargan ultimos, con la direccion de salto en el medio.
    ldr  x20,      [x30, #(20 * 8)]
    mrs  x30, tpidr_el1
    ldr  x30, [x30, #312]             // entry
    blr  x30

    // Volvio solo. El bloque se relee de tpidr_el1 en vez de confiar en x20:
    // x20 lo cargo el agente con lo que pidio, y despues de correr su codigo
    // vale lo que el haya dejado.
    mrs  x20, tpidr_el1

    // Se fotografian los registros; nada de esto usa la pila.
    add  x10, x20, #32                // donde arranca regs
    stp  x0,  x1,  [x10, #(0 * 8)]
    stp  x2,  x3,  [x10, #(2 * 8)]
    stp  x4,  x5,  [x10, #(4 * 8)]
    stp  x6,  x7,  [x10, #(6 * 8)]
    stp  x8,  x9,  [x10, #(8 * 8)]
    // x10 es el puntero y x20 el bloque: se guarda cero en vez de mentir.
    str  xzr,      [x10, #(10 * 8)]
    stp  x11, x12, [x10, #(11 * 8)]
    stp  x13, x14, [x10, #(13 * 8)]
    stp  x15, x16, [x10, #(15 * 8)]
    stp  x17, x18, [x10, #(17 * 8)]
    stp  x19, x20, [x10, #(19 * 8)]
    stp  x21, x22, [x10, #(21 * 8)]
    stp  x23, x24, [x10, #(23 * 8)]
    stp  x25, x26, [x10, #(25 * 8)]
    stp  x27, x28, [x10, #(27 * 8)]
    stp  x29, x30, [x10, #(29 * 8)]
    mov  x9,  sp
    str  x9,       [x10, #(31 * 8)]   // el sp del agente, que es SP_EL0
    // Ya volvio: no hay un "donde estaba ejecutando" que informar.
    str  xzr,      [x10, #(32 * 8)]   // pc
    mrs  x9,  nzcv
    str  x9,       [x10, #(33 * 8)]   // pstate

    // Recien ahora se vuelve a la pila del kernel y se desarma.
    msr  spsel, #1
    str  xzr, [x20, #0]

    mov  x0,  #0
    b    exec_exit

exec_supervised:
    // Bajar a EL0 es volver de una excepcion que nunca ocurrio: se le dice al
    // hardware donde seguir, con que estado, y se hace `eret` (D27).
    msr  elr_el1, x0
    // El estado: el modo va en los cuatro bits de abajo, y 0b0000 es EL0 con
    // SP_EL0 — que es la pila que se acaba de poner. El resto se toma del DAIF
    // de ahora, o sea las mismas mascaras con las que corre el kernel en este
    // momento: como `exec` se llama con los timbres abiertos (D29), el agente
    // arranca con las interrupciones abiertas y **sin poder cerrarlas**, que es
    // el punto de todo esto.
    mrs  x9,  daif
    msr  spsr_el1, x9

    // Y los registros que pidio el agente, recien ahora: ELR_EL1 ya tiene la
    // direccion de entrada, asi que se pueden pisar todos. El `eret` no usa
    // ninguno — saca de ELR y SPSR, que son registros de sistema.
    add  x30, x20, #32
    ldp  x0,  x1,  [x30, #(0 * 8)]
    ldp  x2,  x3,  [x30, #(2 * 8)]
    ldp  x4,  x5,  [x30, #(4 * 8)]
    ldp  x6,  x7,  [x30, #(6 * 8)]
    ldp  x8,  x9,  [x30, #(8 * 8)]
    ldp  x10, x11, [x30, #(10 * 8)]
    ldp  x12, x13, [x30, #(12 * 8)]
    ldp  x14, x15, [x30, #(14 * 8)]
    ldp  x16, x17, [x30, #(16 * 8)]
    ldp  x18, x19, [x30, #(18 * 8)]
    ldp  x21, x22, [x30, #(21 * 8)]
    ldp  x23, x24, [x30, #(23 * 8)]
    ldp  x25, x26, [x30, #(25 * 8)]
    ldp  x27, x28, [x30, #(27 * 8)]
    ldr  x29,      [x30, #(29 * 8)]
    ldr  x20,      [x30, #(20 * 8)]
    // Y x30 ultimo, que hasta aca era el puntero al bloque del kernel. Se carga
    // de su ranura —cero, porque no esta entre los que se pueden poner— en vez
    // de dejarselo: el agente supervisado no puede leer ahi, pero regalarle la
    // direccion igual no hace falta.
    ldr  x30,      [x30, #(30 * 8)]
    eret

.globl exec_window
// La ventanilla: el codigo del agente hizo `svc` desde EL0.
//
// Llega desde `vec_lower`, que ya distinguio esto de un fault, con los x9 y x10
// originales apilados en SP_EL1. El hardware ya puso SPSel=1, asi que `sp` es
// la del kernel y la del agente quedo intacta en SP_EL0.
exec_window:
    mrs  x10, tpidr_el1
    ldr  x9,  [x10, #0]
    // Sin `exec` en curso no hay a donde volver: puede llegar desde un handler
    // del agente, que corre privilegiado (D27). Se vuelve como si nada.
    cbz  x9,  exec_window_ignore

    // Los originales de x9 y x10 estan en la pila; se copian antes de usarlos.
    ldr  x9,  [sp]
    str  x9,  [x10, #(32 + 9 * 8)]
    ldr  x9,  [sp, #8]
    str  x9,  [x10, #(32 + 10 * 8)]

    add  x10, x10, #32                // ahora x10 es la base de regs
    stp  x0,  x1,  [x10, #(0 * 8)]
    stp  x2,  x3,  [x10, #(2 * 8)]
    stp  x4,  x5,  [x10, #(4 * 8)]
    stp  x6,  x7,  [x10, #(6 * 8)]
    str  x8,       [x10, #(8 * 8)]
    stp  x11, x12, [x10, #(11 * 8)]
    stp  x13, x14, [x10, #(13 * 8)]
    stp  x15, x16, [x10, #(15 * 8)]
    stp  x17, x18, [x10, #(17 * 8)]
    stp  x19, x20, [x10, #(19 * 8)]
    stp  x21, x22, [x10, #(21 * 8)]
    stp  x23, x24, [x10, #(23 * 8)]
    stp  x25, x26, [x10, #(25 * 8)]
    stp  x27, x28, [x10, #(27 * 8)]
    stp  x29, x30, [x10, #(29 * 8)]
    mrs  x9,  sp_el0
    str  x9,       [x10, #(31 * 8)]   // el sp con el que quedo el agente
    str  xzr,      [x10, #(32 * 8)]   // pc: ya volvio
    mrs  x9,  spsr_el1
    str  x9,       [x10, #(33 * 8)]   // el pstate con el que llego al `svc`

    // Y de vuelta a la pila del kernel, que es donde `exec_exit` espera estar.
    sub  x10, x10, #32
    ldr  x9,  [x10, #16]
    mov  sp,  x9
    str  xzr, [x10, #0]
    mov  x0,  #0
    b    exec_exit

exec_window_ignore:
    ldp  x9, x10, [sp], #16
    eret

exec_recovery:
    // Aca aterriza el `eret` del handler cuando hubo fault. Ya llega con
    // SPSel=1 porque el handler se lo prendio al SPSR; falta ponerle el valor.
    mrs  x9,  tpidr_el1
    ldr  x10, [x9, #16]
    mov  sp,  x10
    str  xzr, [x9, #0]
    mov  x0,  #1

exec_exit:
    ldp  x19, x20, [sp, #16]
    ldp  x21, x22, [sp, #32]
    ldp  x23, x24, [sp, #48]
    ldp  x25, x26, [sp, #64]
    ldp  x27, x28, [sp, #80]
    ldp  x29, x30, [sp], #96
    ret
"#
);

extern "C" {
    fn exec_trampoline(entry: u64, supervised: u64) -> u64;
}

/// Salta al codigo y vuelve con lo que haya pasado.
///
/// # Safety
///
/// `entry` tiene que apuntar a memoria mapeada y ejecutable.
/// Los registros que el agente puede poner al arrancar (D3, P4).
///
/// x0 a x29 y nada mas. Quedan afuera `pc` —donde empieza a ejecutar lo dice
/// `off`—, `sp` —la pila la pone el kernel y lo publica `describe`—, `pstate`
/// —no es un valor que se cargue sino consecuencia de como se entra, y en
/// `supervised` lleva las interrupciones prendidas a proposito (D29)— y **x30**,
/// que corriendo `raw` es la direccion a la que vuelve el codigo cuando termina:
/// dejar que el agente lo pise seria que no pueda volver.
pub const INITIAL: &[&str] = &[
    "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13",
    "x14", "x15", "x16", "x17", "x18", "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26",
    "x27", "x28", "x29",
];

/// El registro por el que se pasa el primer argumento, como indice dentro de
/// `REGISTERS`, que es como viene `initial`.
const FIRST_ARGUMENT: usize = 0; // x0

pub unsafe fn run(
    entry: u64,
    region: (u64, u64),
    supervised: bool,
    initial: &[Option<u64>],
) -> Outcome {
    let slot = crate::percpu::slot();
    let block = crate::percpu::block(slot);

    // Los valores con los que arranca. El que no pidio queda en cero, salvo el
    // primer argumento: ahi va la direccion de entrada, para que el codigo
    // pueda encontrar sus datos sin depender de donde lo hayan cargado. Si el
    // agente **si** lo puso, gana el agente: es su codigo (P2).
    for i in 0..(*block).regs.len() {
        (*block).regs[i] = match initial.get(i).copied().flatten() {
            Some(v) => v,
            None if i == FIRST_ARGUMENT => entry,
            None => 0,
        };
    }
    (*block).entry = entry;

    // De donde sale la pila. En EL0 la del kernel no se puede ni escribir, asi
    // que corriendo supervisado la pila es el final del reclamo del propio
    // agente (D27), alineada a 16 como pide la arquitectura.
    (*block).stack = if supervised {
        region.0.saturating_add(region.1) & !0xF
    } else {
        core::ptr::addr_of!(AGENT_STACKS) as u64 + ((slot + 1) * AGENT_STACK_SIZE) as u64
    };

    // Un pedido de corte que quedo de antes no vale para este trabajo: se
    // limpia al empezar, o el primer `exec` nuevo se cortaria solo.
    kernel_core::work::clear_cancel(slot);

    let diverted = exec_trampoline(entry, supervised as u64) != 0;

    let regs = core::slice::from_raw_parts(
        core::ptr::addr_of!((*block).regs) as *const u64,
        34,
    );

    // El desvio es el mismo camino para las dos cosas —un fault y un corte
    // aterrizan en el mismo punto de recuperacion— asi que hay que preguntar
    // cual fue. Se pregunta primero por el corte porque es el que tiene una
    // marca propia: un fault no la deja.
    if diverted && kernel_core::work::was_cancelled(slot) {
        Outcome { faulted: false, cancelled: true, regs, fault: None }
    } else if diverted {
        // El handler ya dejo anotado el fault, con los registros del momento
        // exacto en que fallo — que son mas utiles que los de ahora.
        let f = crate::vectors::last();
        Outcome {
            faulted: true,
            cancelled: false,
            regs: f.map(|f| f.regs).unwrap_or(&[]),
            fault: f,
        }
    } else {
        Outcome { faulted: false, cancelled: false, regs, fault: None }
    }
}

/// Empuja el codigo recien escrito hasta donde lo ve el camino de instrucciones.
///
/// En aarch64 la cache de datos y la de instrucciones **no** son coherentes: lo
/// que se escribio como dato puede seguir en la cache de datos mientras el
/// camino de instrucciones lee de memoria lo que habia antes. Saltar ahi seria
/// ejecutar basura vieja, y el sintoma no se parece en nada a la causa.
///
/// x86_64 no necesita nada de esto: el hardware mantiene la coherencia solo.
///
/// # Safety
///
/// El rango tiene que estar mapeado.
pub unsafe fn sync_cache(start: u64, bytes: u64) {
    // El tamano de linea de cada cache lo informa el propio CPU (P4). No se
    // hornea: varia entre implementaciones.
    let ctr: u64;
    core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack));
    let data_line = 4u64 << ((ctr >> 16) & 0xF);
    let instr_line = 4u64 << (ctr & 0xF);

    let end = start.saturating_add(bytes);

    // Primero limpiar la de datos hasta el punto de unificacion, que es donde
    // las dos se encuentran.
    let mut a = start & !(data_line - 1);
    while a < end {
        core::arch::asm!("dc cvau, {}", in(reg) a, options(nostack));
        a += data_line;
    }
    core::arch::asm!("dsb ish", options(nostack));

    // Y recien despues invalidar la de instrucciones, para que vaya a buscar lo
    // nuevo. Al reves no serviria: invalidaria y volveria a traer lo viejo.
    let mut a = start & !(instr_line - 1);
    while a < end {
        core::arch::asm!("ic ivau, {}", in(reg) a, options(nostack));
        a += instr_line;
    }
    core::arch::asm!("dsb ish", options(nostack));
    core::arch::asm!("isb", options(nostack));
}
