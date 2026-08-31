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
pub const CODIGO: u16 = 0x08;
/// Selector del segmento de datos.
pub const DATOS: u16 = 0x10;
/// Selector del TSS. Ocupa dos entradas porque en 64 bits el descriptor mide 16.
const TSS: u16 = 0x18;

/// Cual de las siete pilas de la IST usan las excepciones. La 1.
pub const IST_FAULTS: u8 = 1;

/// 16 KiB. Solo tiene que aguantar el marco de excepcion y lo que use el
/// handler, que trabaja sobre buffers estaticos y no sobre la pila.
const TAM_PILA: usize = 16 * 1024;

#[repr(C, align(16))]
struct Pila([u8; TAM_PILA]);

static mut PILA_EXCEPCION: Pila = Pila([0; TAM_PILA]);

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

static mut TSS_ACTUAL: Tss = Tss {
    _reservado0: 0,
    rsp: [0; 3],
    _reservado1: 0,
    ist: [0; 7],
    _reservado2: 0,
    _reservado3: 0,
    iomap: core::mem::size_of::<Tss>() as u16,
};

/// Cinco huecos de 8 bytes: nulo, codigo, datos y dos para el TSS.
#[repr(C, align(16))]
struct Gdt([u64; 5]);

static mut GDT: Gdt = Gdt([0; 5]);

#[repr(C, packed)]
struct Descriptor {
    limite: u16,
    base: u64,
}

/// Arma la GDT con un TSS, la carga, y engancha la pila de excepcion.
///
/// # Safety
///
/// Solo despues de `ExitBootServices`: se reemplaza la GDT del firmware.
pub unsafe fn install() -> Result<(), &'static str> {
    let tss = &mut *core::ptr::addr_of_mut!(TSS_ACTUAL);

    // La pila crece hacia abajo, asi que la IST apunta al final.
    let pila = core::ptr::addr_of!(PILA_EXCEPCION) as u64 + TAM_PILA as u64;
    tss.ist[(IST_FAULTS - 1) as usize] = pila;

    let gdt = &mut *core::ptr::addr_of_mut!(GDT);
    gdt.0[0] = 0;
    // Codigo de 64 bits: presente, anillo 0, ejecutable, bit L.
    gdt.0[1] = 0x00AF_9A00_0000_FFFF;
    // Datos: presente, anillo 0, escribible.
    gdt.0[2] = 0x00CF_9200_0000_FFFF;

    // El descriptor del TSS son 16 bytes y lleva la direccion partida en cuatro
    // pedazos salteados. Es una herencia de cuando las direcciones eran de 24
    // bits y se fue estirando sin mover lo que ya estaba.
    let base = core::ptr::addr_of!(*tss) as u64;
    let limite = (core::mem::size_of::<Tss>() - 1) as u64;
    gdt.0[3] = limite & 0xFFFF
        | (base & 0xFF_FFFF) << 16
        // 0x89: presente, anillo 0, TSS de 64 bits disponible.
        | 0x89 << 40
        | ((limite >> 16) & 0xF) << 48
        | ((base >> 24) & 0xFF) << 56;
    gdt.0[4] = base >> 32;

    let d = Descriptor {
        limite: (core::mem::size_of::<Gdt>() - 1) as u16,
        base: core::ptr::addr_of!(*gdt) as u64,
    };

    core::arch::asm!(
        "lgdt [{descriptor}]",

        // Cargar la GDT no recarga CS: sigue con el selector viejo hasta que se
        // haga un salto lejano. En 64 bits no hay `ljmp` con destino inmediato,
        // asi que se arma el salto a mano — se apila el selector nuevo y la
        // direccion de vuelta, y `retfq` los usa como si volviera de una
        // llamada lejana.
        "push {codigo}",
        "lea {tmp}, [rip + 2f]",
        "push {tmp}",
        "retfq",
        "2:",

        // Los de datos si se recargan directo.
        "mov ss, {datos:x}",
        "mov ds, {datos:x}",
        "mov es, {datos:x}",

        // Y por ultimo el TSS, que es lo que hace visible la IST.
        "ltr {tss:x}",

        descriptor = in(reg) &d,
        codigo = in(reg) CODIGO as u64,
        datos = in(reg) DATOS as u64,
        tss = in(reg) TSS as u64,
        tmp = out(reg) _,
        // Sin `nostack`: este bloque apila para el salto lejano.
        options(preserves_flags),
    );

    // Se relee, por el mismo motivo de siempre: un install que no hiciera nada
    // se veria igual que uno que anduvo.
    let cs: u16;
    core::arch::asm!("mov {0:x}, cs", out(reg) cs, options(nomem, nostack));
    if cs != CODIGO {
        return Err("CS no quedo en el segmento de codigo nuestro");
    }
    Ok(())
}
