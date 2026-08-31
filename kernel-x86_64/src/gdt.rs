//! GDT y TSS propias, para tener una pila de excepcion aparte (P5).
//!
//! # El problema que resuelve
//!
//! Cuando el CPU toma una excepcion en x86_64 apila el marco **sobre la pila que
//! esta usando en ese momento**. Si el codigo del agente rompio RSP y despues
//! falla, ese apilado escribe en cualquier lado, falla tambien, y eso escala a
//! doble y triple fault: la maquina se reinicia y no queda nada que capturar.
//!
//! O sea que sin esto, un `exec` con un puntero de pila roto se lleva puesto el
//! kernel — justo el caso para el que existe P5.
//!
//! # Como se resuelve
//!
//! x86_64 tiene la IST: siete punteros a pila guardados en el TSS. Una entrada
//! de la IDT puede pedir que el CPU cambie a una de esas pilas **antes** de
//! apilar nada. Con eso, la pila del agente puede estar donde sea: la excepcion
//! entra en una pila conocida y sana.
//!
//! El TSS solo se alcanza a traves de un descriptor en la GDT, y la GDT que
//! dejo UEFI no tiene ninguno. Asi que hay que armar una propia.

/// Selector del segmento de codigo. Es el indice 1 de la GDT, por 8 bytes.
pub const CODE: u16 = 0x08;
/// Selector del segmento de datos.
pub const DATA: u16 = 0x10;
/// El primer selector de TSS. Cada nucleo tiene el suyo, y cada descriptor
/// ocupa dos entradas porque en 64 bits mide 16 bytes.
const TSS_BASE: u16 = 0x18;

/// El selector del TSS de esa ranura.
fn tss_selector(slot: usize) -> u16 {
    TSS_BASE + (slot as u16) * 16
}

/// Cual de las siete pilas de la IST usan las excepciones. La 1.
pub const IST_FAULTS: u8 = 1;

/// Y cual usan los timbres de aparato. La 2, aparte de la de los faults.
///
/// Aparte por dos razones. Una: durante un `exec` la pila en uso es la del
/// agente, y si la rompio el timbre se estrellaria al entrar. Dos: compartirla
/// con la de los faults seria que un timbre que llega mientras se atiende un
/// fault le pise el marco.
pub const IST_IRQ: u8 = 2;

/// Cuantas pilas de la IST se usan.
const IST_STACKS: usize = 2;

/// 16 KiB. Solo tiene que aguantar el marco de excepcion y lo que use el
/// handler, que trabaja sobre buffers estaticos y no sobre la pila.
const STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct Stacks([[u8; STACK_SIZE]; crate::percpu::SLOTS * IST_STACKS]);

/// Una pila de excepcion por nucleo. Compartirlas seria que dos nucleos que
/// fallan a la vez se pisen el marco de excepcion — corrupcion adentro del
/// mecanismo que existe para que nada se corrompa en silencio.
static mut EXCEPTION_STACKS: Stacks = Stacks([[0; STACK_SIZE]; crate::percpu::SLOTS * IST_STACKS]);

/// El TSS de 64 bits. De todo lo que tiene, lo unico que se usa es `ist[0]`.
///
/// El resto de los campos existen porque el hardware espera esta forma exacta:
/// no se pueden omitir aunque no se usen.
#[repr(C, packed)]
struct Tss {
    _reservado0: u32,
    /// Pilas para cuando se baja de nivel de privilegio. No se usan: aca no hay
    /// anillo 3 (D10: sin procesos, sin usuarios).
    rsp: [u64; 3],
    _reservado1: u64,
    /// Las siete pilas de la IST. La primera es la nuestra.
    ist: [u64; 7],
    _reservado2: u64,
    _reservado3: u16,
    /// Donde empieza el mapa de permisos de puertos de E/S. Apuntando mas alla
    /// del propio TSS, queda vacio: sin mapa, ningun puerto se permite desde
    /// anillo 3, que es donde no hay nadie.
    iomap: u16,
}

static mut TSS_PER_CORE: [Tss; crate::percpu::SLOTS] = [const {
    Tss {
        _reservado0: 0,
        rsp: [0; 3],
        _reservado1: 0,
        ist: [0; 7],
        _reservado2: 0,
        _reservado3: 0,
        iomap: core::mem::size_of::<Tss>() as u16,
    }
}; crate::percpu::SLOTS];

/// Nulo, codigo, datos, y dos huecos por cada TSS.
const GDT_ENTRIES: usize = 3 + 2 * crate::percpu::SLOTS;

#[repr(C, align(16))]
struct Gdt([u64; GDT_ENTRIES]);

/// La tabla es una sola y la comparten todos los nucleos: los descriptores son
/// de solo lectura una vez armados. Lo que cambia por nucleo es **cual TSS
/// carga cada uno**.
static mut GDT: Gdt = Gdt([0; GDT_ENTRIES]);

#[repr(C, packed)]
struct Descriptor {
    limit: u16,
    base: u64,
}

/// Arma la GDT con un TSS, la carga, y engancha la pila de excepcion.
///
/// # Safety
///
/// Solo despues de `ExitBootServices`: se reemplaza la GDT del firmware.
pub unsafe fn install(slot: usize) -> Result<(), &'static str> {
    if slot >= crate::percpu::SLOTS {
        return Err("ranura fuera de rango");
    }

    let tss = &mut (*core::ptr::addr_of_mut!(TSS_PER_CORE))[slot];

    // Dos pilas por nucleo, y la IST apunta al final de cada una porque crecen
    // hacia abajo.
    let stacks = core::ptr::addr_of!(EXCEPTION_STACKS) as u64;
    let mine = slot * IST_STACKS;
    tss.ist[(IST_FAULTS - 1) as usize] = stacks + ((mine + 1) * STACK_SIZE) as u64;
    tss.ist[(IST_IRQ - 1) as usize] = stacks + ((mine + 2) * STACK_SIZE) as u64;

    let gdt = &mut *core::ptr::addr_of_mut!(GDT);
    gdt.0[0] = 0;
    // Codigo de 64 bits: presente, anillo 0, ejecutable, bit L.
    gdt.0[1] = 0x00AF_9A00_0000_FFFF;
    // Datos: presente, anillo 0, escribible.
    gdt.0[2] = 0x00CF_9200_0000_FFFF;

    // Un descriptor por TSS. Son 16 bytes y llevan la direccion partida en
    // cuatro pedazos salteados: herencia de cuando las direcciones eran de 24
    // bits y el formato se fue estirando sin mover lo que ya estaba.
    //
    // Se arman todos, no solo el propio: la tabla es compartida y el nucleo que
    // llegue despues necesita encontrar el suyo ya puesto.
    for i in 0..crate::percpu::SLOTS {
        let other_one = core::ptr::addr_of!((*core::ptr::addr_of!(TSS_PER_CORE))[i]) as u64;
        let limit = (core::mem::size_of::<Tss>() - 1) as u64;
        gdt.0[3 + i * 2] = limit & 0xFFFF
            | (other_one & 0xFF_FFFF) << 16
            // 0x89: presente, anillo 0, TSS de 64 bits disponible.
            | 0x89 << 40
            | ((limit >> 16) & 0xF) << 48
            | ((other_one >> 24) & 0xFF) << 56;
        gdt.0[4 + i * 2] = other_one >> 32;
    }

    let d = Descriptor {
        limit: (core::mem::size_of::<Gdt>() - 1) as u16,
        base: core::ptr::addr_of!(*gdt) as u64,
    };

    core::arch::asm!(
        "lgdt [{descriptor}]",

        // Cargar la GDT no recarga CS: sigue con el selector viejo hasta que se
        // haga un salto lejano. En 64 bits no hay `ljmp` con destino inmediato,
        // asi que se arma el salto a mano — se apila el selector nuevo y la
        // direccion de vuelta, y `retfq` los usa como si volviera de una
        // llamada lejana.
        "push {code_bytes}",
        "lea {tmp}, [rip + 2f]",
        "push {tmp}",
        "retfq",
        "2:",

        // Los de datos si se recargan directo.
        "mov ss, {data:x}",
        "mov ds, {data:x}",
        "mov es, {data:x}",

        // Y por ultimo el TSS, que es lo que hace visible la IST.
        "ltr {tss:x}",

        descriptor = in(reg) &d,
        code_bytes = in(reg) CODE as u64,
        data = in(reg) DATA as u64,
        tss = in(reg) tss_selector(slot) as u64,
        tmp = out(reg) _,
        // Sin `nostack`: este bloque apila para el salto lejano.
        options(preserves_flags),
    );

    // Se relee, por el mismo motivo de siempre: un install que no hiciera nada
    // se veria igual que uno que anduvo.
    let cs: u16;
    core::arch::asm!("mov {0:x}, cs", out(reg) cs, options(nomem, nostack));
    if cs != CODE {
        return Err("CS no quedo en el segmento de codigo nuestro");
    }
    Ok(())
}
