//! Captura de excepciones en x86_64 (P5, D7).
//!
//! # Como avisa x86 que algo salio mal
//!
//! Hay una tabla de 256 entradas, la IDT, y cada excepcion tiene su numero de
//! vector: 0 es division por cero, 6 es instruccion invalida, 14 es page fault.
//! Cuando pasa algo, el CPU busca la entrada, apila el estado y salta ahi.
//!
//! # Por que los stubs van en ensamblador
//!
//! El CPU no salva los registros: entra al handler con el estado del codigo que
//! fallo todavia en ellos. Cualquier cosa que haga Rust —hasta armar un marco de
//! pila— los pisa. Asi que hay un stub por vector que apila todo tal cual estaba
//! y recien despues llama a Rust.
//!
//! Y hay una arruga: **algunas excepciones apilan un codigo de error y otras
//! no**. Si no se empareja, el marco queda corrido un lugar y todo lo que se lea
//! despues es basura plausible. Los stubs de las que no lo apilan meten un cero
//! para que el marco tenga siempre la misma forma.

use core::fmt::Write;
use kernel_core::fault::{Cause, Fault};

/// Los 32 primeros vectores son las excepciones del CPU. Del 32 para arriba son
/// interrupciones de dispositivo, que le tocan al agente (D9).
const EXCEPTIONS: usize = 32;

/// El estado que ve el handler, en el mismo orden en que quedo en la pila.
///
/// El orden importa y no es negociable: lo fija la secuencia de `push` del stub
/// y lo que el CPU apila solo.
#[repr(C)]
pub struct Frame {
    // Lo que apila el stub, del ultimo al primero.
    rax: u64,
    rbx: u64,
    rcx: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
    rbp: u64,
    r8: u64,
    r9: u64,
    r10: u64,
    r11: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    // Lo que agrega el stub para identificar la excepcion.
    vector: u64,
    error: u64,
    // Y lo que apila el CPU solo.
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

/// Los nombres, en el mismo orden en que `copiar_registros` deja los valores.
pub const REGISTERS: &[&str] = &[
    "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "r8", "r9", "r10", "r11", "r12",
    "r13", "r14", "r15", "rip", "rflags",
];

/// Uno por nucleo: dos que fallan a la vez no se pisan el reporte.
static mut VALUES: [[u64; REGISTERS.len()]; crate::percpu::SLOTS] =
    [[0; REGISTERS.len()]; crate::percpu::SLOTS];
static mut LAST: [Option<Fault>; crate::percpu::SLOTS] = [None; crate::percpu::SLOTS];

core::arch::global_asm!(
    r#"
.section .text

// Vector sin codigo de error: se mete un cero para emparejar el marco.
.macro STUB_WITHOUT n
stub_\n:
    push 0
    push \n
    jmp fault_common
.endm

// Vector con codigo de error: ya lo apilo el CPU.
.macro STUB_WITH n
stub_\n:
    push \n
    jmp fault_common
.endm

STUB_WITHOUT 0
STUB_WITHOUT 1
STUB_WITHOUT 2
STUB_WITHOUT 3
STUB_WITHOUT 4
STUB_WITHOUT 5
STUB_WITHOUT 6
STUB_WITHOUT 7
STUB_WITH 8
STUB_WITHOUT 9
STUB_WITH 10
STUB_WITH 11
STUB_WITH 12
STUB_WITH 13
STUB_WITH 14
STUB_WITHOUT 15
STUB_WITHOUT 16
STUB_WITH 17
STUB_WITHOUT 18
STUB_WITHOUT 19
STUB_WITHOUT 20
STUB_WITH 21
STUB_WITHOUT 22
STUB_WITHOUT 23
STUB_WITHOUT 24
STUB_WITHOUT 25
STUB_WITHOUT 26
STUB_WITHOUT 27
STUB_WITHOUT 28
STUB_WITH 29
STUB_WITH 30
STUB_WITHOUT 31

fault_common:
    // Los registros, tal cual estaban cuando fallo.
    push r15
    push r14
    push r13
    push r12
    push r11
    push r10
    push r9
    push r8
    push rbp
    push rdi
    push rsi
    push rdx
    push rcx
    push rbx
    push rax

    // La pila apunta al marco entero: ese es el argumento.
    mov rdi, rsp
    // rbx ya quedo salvado arriba, asi que sirve de andamio para alinear la
    // pila a 16, que es lo que pide la ABI antes de un `call`.
    mov rbx, rsp
    and rsp, -16
    call fault_rust
    mov rsp, rbx

    pop rax
    pop rbx
    pop rcx
    pop rdx
    pop rsi
    pop rdi
    pop rbp
    pop r8
    pop r9
    pop r10
    pop r11
    pop r12
    pop r13
    pop r14
    pop r15

    // El vector y el codigo de error los pusimos nosotros: los saca el iretq.
    add rsp, 16
    iretq

// La tabla de direcciones, para que Rust arme la IDT sin declarar 32 externs.
.section .rodata
.globl STUBS
.balign 8
STUBS:
    .quad stub_0,  stub_1,  stub_2,  stub_3,  stub_4,  stub_5,  stub_6,  stub_7
    .quad stub_8,  stub_9,  stub_10, stub_11, stub_12, stub_13, stub_14, stub_15
    .quad stub_16, stub_17, stub_18, stub_19, stub_20, stub_21, stub_22, stub_23
    .quad stub_24, stub_25, stub_26, stub_27, stub_28, stub_29, stub_30, stub_31
"#
);

extern "C" {
    static STUBS: [u64; EXCEPTIONS];
}

/// Lo que llama el stub. Corre con los registros ya a salvo.
#[no_mangle]
extern "sysv64" fn fault_rust(m: &mut Frame) {
    // El segundo escalon para cortar un nucleo colgado.
    //
    // El vector 2 es el NMI, y **`cli` no lo puede tapar** — de ahi el nombre.
    // Es lo que alcanza a un codigo que enmascaro las interrupciones normales,
    // que con el timbre comun quedaba fuera de alcance. Entra por la tabla de
    // excepciones como cualquier otra, asi que el desvio ya existia: lo unico
    // que falta es no contarlo como fault, porque no lo es.
    //
    // Va antes de armar el `Fault` a proposito: si se anotara, `exec` lo leeria
    // como si el codigo del agente hubiera fallado.
    if m.vector == 2 && crate::percpu::armed() != 0 {
        let slot = crate::percpu::slot();
        if kernel_core::work::take_cancel(slot) {
            divert(m);
            return;
        }
    }

    let cause = translate(m.vector);

    // CR2 guarda la direccion que se quiso tocar, y solo tiene sentido en un
    // page fault: en cualquier otra excepcion es lo que haya quedado de antes.
    let address = if cause == Cause::PageFault {
        let cr2: u64;
        unsafe { core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack)) };
        Some(cr2)
    } else {
        None
    };

    // De quien es este fault. Una sola lectura de un registro del CPU: adentro
    // de un handler no se puede depender de memoria compartida.
    let slot = crate::percpu::slot();

    let regs = unsafe {
        let v = &mut (*core::ptr::addr_of_mut!(VALUES))[slot];
        *v = [
            m.rax, m.rbx, m.rcx, m.rdx, m.rsi, m.rdi, m.rbp, m.rsp, m.r8, m.r9, m.r10, m.r11,
            m.r12, m.r13, m.r14, m.r15, m.rip, m.rflags,
        ];
        &(*core::ptr::addr_of!(VALUES))[slot]
    };

    let f = Fault {
        cause,
        raw: m.vector,
        detail: m.error,
        pc: m.rip,
        address,
        regs,
    };
    unsafe { (*core::ptr::addr_of_mut!(LAST))[slot] = Some(f) };

    if cause.resumable() {
        // En x86 un `int3` es un trap: RIP ya quedo apuntando a la instruccion
        // siguiente, asi que volver alcanza.
        return;
    }

    // Si hay un `exec` en curso, hay a donde volver: se le cambia el destino al
    // `iretq`. En vez de devolverle el control al codigo que fallo, lo devuelve
    // al punto de recuperacion, que le contesta al agente con este fault como
    // dato (P5). Esto es lo que hace que el codigo del agente no pueda matar al
    // kernel.
    if crate::percpu::armed() != 0 {
        divert(m);
        return;
    }

    // Sin `exec` en curso, un fault es un bug del kernel: la misma instruccion
    // volveria a fallar para siempre. Se cuenta y se para — pero se cuenta.
    let mut s = crate::SerialText;
    let _ = s.write_str("\r\n");
    kernel_core::fault::report(&f, REGISTERS, &mut s);
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) }
    }
}

/// Le cambia el destino al `iretq`: en vez de volver al codigo, vuelve al punto
/// de recuperacion de `exec`.
///
/// Lo usan las dos cosas que sacan a un nucleo de donde estaba —un fault y un
/// corte pedido desde afuera— porque el camino de vuelta es el mismo.
fn divert(m: &mut Frame) {
    m.rip = crate::percpu::return_point();
    // Los dos bits de abajo de CS son el anillo desde el que se entro. Si el
    // codigo venia de anillo 3 (D27), reescribir solo RIP no alcanza: el
    // `iretq` volveria **a anillo 3** con una direccion del kernel, que no es
    // alcanzable desde ahi, y fallaria de nuevo — un fault adentro del
    // mecanismo que existe para capturar faults.
    //
    // Se lee del marco y no de una bandera nuestra: es el hardware diciendo de
    // donde vino (P4).
    if m.cs & 3 != 0 {
        m.cs = crate::gdt::CODE as u64;
        m.ss = crate::gdt::DATA as u64;
        m.rsp = crate::percpu::kernel_stack();
    }
}

fn translate(vector: u64) -> Cause {
    match vector {
        0 => Cause::DivideByZero,
        3 => Cause::Breakpoint,
        6 => Cause::InvalidOpcode,
        8 => Cause::Double,
        13 => Cause::Protection,
        14 => Cause::PageFault,
        17 => Cause::Alignment,
        _ => Cause::Unknown,
    }
}

// --- La tabla ---------------------------------------------------------------

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Entry {
    off_low: u16,
    selector: u16,
    ist: u8,
    kind: u8,
    off_mid: u16,
    off_high: u32,
    zero: u32,
}

impl Entry {
    const fn blank() -> Self {
        Self { off_low: 0, selector: 0, ist: 0, kind: 0, off_mid: 0, off_high: 0, zero: 0 }
    }
}

#[repr(C, align(16))]
struct Idt([Entry; 256]);

static mut IDT: Idt = Idt([Entry::blank(); 256]);

#[repr(C, packed)]
struct Descriptor {
    limit: u16,
    base: u64,
}

/// Arma la IDT y la carga.
///
/// # Safety
///
/// Los stubs tienen que estar mapeados y ejecutables, que lo estan porque son
/// parte de la imagen del kernel.
pub unsafe fn install(slot: usize) -> Result<(), &'static str> {
    // Primero el bloque privado: el handler lo lee para saber de quien es el
    // fault, asi que tiene que estar puesto antes de que pueda haber uno.
    crate::percpu::install_block(slot);

    // Despues la GDT: sin un TSS ahi adentro no existe la pila de excepcion, y
    // las entradas de abajo la piden.
    crate::gdt::install(slot)?;

    let idt = &mut *core::ptr::addr_of_mut!(IDT);

    for v in 0..EXCEPTIONS {
        let addr = STUBS[v];
        idt.0[v] = Entry {
            off_low: addr as u16,
            selector: crate::gdt::CODE,
            // La pila propia: el CPU cambia a ella ANTES de apilar el marco,
            // asi que da igual que el codigo del agente haya roto RSP (P5).
            ist: crate::gdt::IST_FAULTS,
            // 0x8E: presente, privilegio 0, compuerta de interrupcion de 64 bits.
            kind: 0x8E,
            off_mid: (addr >> 16) as u16,
            off_high: (addr >> 32) as u32,
            zero: 0,
        };
    }

    // La ventanilla por la que vuelve el codigo `supervised` (D27). Es la
    // unica entrada de la tabla con `DPL=3`: las demas son de anillo 0, asi
    // que el agente no puede invocarlas — un `int 3` desde anillo 3 le da
    // proteccion, que vuelve como fault y no como breakpoint.
    //
    // Y **sin IST**: entra por `RSP0`, que es el camino que el hardware usa
    // para cualquier trap que sube de privilegio. Asi esa pila se ejercita en
    // cada `exec supervised` en vez de ser un campo del TSS que nadie mira.
    let window = crate::exec::exec_window as *const () as u64;
    idt.0[crate::exec::WINDOW_VECTOR] = Entry {
        off_low: window as u16,
        selector: crate::gdt::CODE,
        ist: 0,
        // 0xEE: presente, privilegio 3, compuerta de interrupcion de 64 bits.
        // El 0x8E de las demas con el DPL corrido; sigue siendo de
        // interrupcion y no de trap, asi que entra con los timbres cerrados.
        kind: 0xEE,
        off_mid: (window >> 16) as u16,
        off_high: (window >> 32) as u32,
        zero: 0,
    };

    let d = Descriptor {
        limit: (core::mem::size_of::<Idt>() - 1) as u16,
        base: core::ptr::addr_of!(*idt) as u64,
    };
    core::arch::asm!("lidt [{}]", in(reg) &d, options(readonly, nostack));

    Ok(())
}

/// Pone una entrada de la tabla para un aparato.
///
/// Los vectores del 0 al 31 son las excepciones del CPU y los pone `install`;
/// del 32 para arriba quedan libres para los timbres de los aparatos.
///
/// # Safety
///
/// `handler` tiene que apuntar a codigo que termine en `iretq`.
pub unsafe fn set_gate(vector: usize, handler: u64) -> Result<(), &'static str> {
    if vector < EXCEPTIONS || vector >= 256 {
        return Err("vector outside the device range");
    }
    let idt = &mut *core::ptr::addr_of_mut!(IDT);
    idt.0[vector] = Entry {
        off_low: handler as u16,
        selector: crate::gdt::CODE,
        // Con pila propia: durante un `exec` la pila en uso es la del agente, y
        // si la rompio el timbre se estrellaria justo al entrar (D29: en este
        // nucleo la interrupcion tiene prioridad, asi que tiene que poder
        // entrar siempre).
        ist: crate::gdt::IST_IRQ,
        kind: 0x8E,
        off_mid: (handler >> 16) as u16,
        off_high: (handler >> 32) as u32,
        zero: 0,
    };
    Ok(())
}

/// Provoca un breakpoint a proposito, para probar que todo esto anda.
pub fn breakpoint() {
    unsafe { core::arch::asm!("int3", options(nomem, nostack)) }
}

/// El ultimo fault de **este** nucleo.
pub fn last() -> Option<Fault> {
    unsafe { (*core::ptr::addr_of!(LAST))[crate::percpu::slot()] }
}
