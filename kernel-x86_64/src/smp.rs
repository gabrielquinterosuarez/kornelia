//! Arrancar los otros nucleos en x86_64 (D13).
//!
//! # Por que esto es tan feo
//!
//! En aarch64 se le pide al firmware y listo. Aca no hay a quien pedirle: se le
//! manda al nucleo una secuencia de interrupciones por el APIC, y **el nucleo
//! arranca en modo real de 16 bits**, como un 8086 de 1978. Hay que subirlo a
//! mano hasta los 64 bits.
//!
//! El camino es: 16 bits reales -> 32 bits protegidos -> 64 bits largos. Cada
//! escalon necesita su propia tabla de segmentos y su propio salto lejano, y el
//! codigo tiene que vivir **abajo del primer megabyte**, porque en modo real no
//! se puede direccionar mas alto.
//!
//! # Como se le dice donde arrancar
//!
//! La interrupcion de arranque (SIPI) no lleva una direccion: lleva un numero de
//! pagina de 8 bits. El nucleo arranca en `numero << 12`. Por eso el trampolin
//! se copia a una direccion fija y baja.

use kernel_core::acpi::Hardware;
use kernel_core::{claims, cores};

/// Donde se copia el trampolin. Tiene que estar abajo de 1 MiB, alineado a
/// pagina, y coincidir con la constante `TRAMP` del ensamblador de abajo.
const TRAMPOLIN: u64 = 0x8000;

/// 16 KiB de pila para cada nucleo.
const TAM_PILA: usize = 16 * 1024;

#[repr(C, align(16))]
struct Pilas([[u8; TAM_PILA]; cores::MAX]);

static mut PILAS: Pilas = Pilas([[0; TAM_PILA]; cores::MAX]);

core::arch::global_asm!(
    r#"
.section .rodata
.globl AP_TRAMPOLIN_INICIO
.globl AP_TRAMPOLIN_FIN

// La direccion adonde se va a copiar todo esto. Los saltos lejanos necesitan
// direcciones absolutas, asi que el destino no puede ser una incognita.
.set TRAMP, 0x8000

.balign 4096
AP_TRAMPOLIN_INICIO:
.code16
ap16:
    cli
    cld
    // El nucleo arranca con CS apuntando al trampolin y todo lo demas en
    // cualquier cosa. Se pone DS en cero para poder usar direcciones absolutas.
    xorw %ax, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss

    lgdtl TRAMP + (gdt_desc - ap16)

    // Bit 0 de CR0: modo protegido.
    movl %cr0, %eax
    orl  $1, %eax
    movl %eax, %cr0

    // El salto es lo que hace efectivo el cambio de modo: hasta que no se
    // recarga CS, el CPU sigue interpretando como antes.
    ljmpl $0x08, $(TRAMP + (ap32 - ap16))

.code32
ap32:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss

    // PAE, que el modo de 64 bits exige.
    movl %cr4, %eax
    orl  $(1 << 5), %eax
    movl %eax, %cr4

    // Las mismas tablas de paginas que usa el nucleo de arranque.
    movl TRAMP + (params - ap16), %eax
    movl %eax, %cr3

    // EFER.LME: habilitar el modo largo.
    movl $0xC0000080, %ecx
    rdmsr
    orl  $(1 << 8), %eax
    wrmsr

    // Y prender la paginacion, que es lo que activa el modo largo de verdad.
    movl %cr0, %eax
    orl  $(1 << 31), %eax
    movl %eax, %cr0

    ljmpl $0x18, $(TRAMP + (ap64 - ap16))

.code64
ap64:
    // Los parametros que dejo escritos el nucleo de arranque.
    movq $(TRAMP + (params - ap16)), %rax
    movq 8(%rax), %rsp
    movq 24(%rax), %rdi        // la ranura, primer argumento
    movq 16(%rax), %rax        // adonde saltar
    jmp  *%rax

// Tres escalones, tres descriptores: 32 bits de codigo, 32 de datos, 64 de
// codigo. El de datos sirve para los dos modos.
.balign 8
gdt:
    .quad 0
    .quad 0x00CF9A000000FFFF
    .quad 0x00CF92000000FFFF
    .quad 0x00AF9A000000FFFF
gdt_fin:

gdt_desc:
    .word gdt_fin - gdt - 1
    .long TRAMP + (gdt - ap16)

// cr3, pila, entrada y ranura. Los escribe el nucleo de arranque despues de
// copiar el trampolin.
.balign 8
params:
    .quad 0
    .quad 0
    .quad 0
    .quad 0

AP_TRAMPOLIN_FIN:
"#,
    options(att_syntax)
);

extern "C" {
    static AP_TRAMPOLIN_INICIO: u8;
    static AP_TRAMPOLIN_FIN: u8;
}

/// Donde quedan los parametros dentro del trampolin ya copiado.
///
/// Se calcula en vez de horneárse: si alguien agrega una instruccion arriba,
/// el offset cambia solo.
unsafe fn params() -> *mut u64 {
    let inicio = core::ptr::addr_of!(AP_TRAMPOLIN_INICIO) as u64;
    let fin = core::ptr::addr_of!(AP_TRAMPOLIN_FIN) as u64;
    // `params` son los ultimos 32 bytes del blob.
    (TRAMPOLIN + (fin - inicio) - 32) as *mut u64
}

/// Lo primero que corre un nucleo nuevo, ya en 64 bits y con todo puesto.
#[no_mangle]
extern "sysv64" fn ap_main(slot: u64) -> ! {
    // Su propia tabla de excepciones y su GDT: hasta aca corria con las del
    // trampolin, que no tienen ni TSS ni handlers.
    unsafe {
        let _ = crate::idt::install(slot as usize);
    }

    cores::arrived(slot as usize, this_core());

    // Y a esperar trabajo. Todavia no hay forma de darselo: `exec` corre en el
    // nucleo que atiende el protocolo. Esa es la parte que sigue.
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) }
    }
}

/// El identificador de este nucleo, tal como lo nombra la maquina.
pub fn this_core() -> u64 {
    // Hoja 1 de CPUID: los 8 bits de arriba de EBX son el APIC ID, que es el
    // mismo numero con el que la MADT nombra a cada nucleo.
    let r = core::arch::x86_64::__cpuid(1);
    (r.ebx >> 24) as u64
}

// --- El APIC ----------------------------------------------------------------

/// Registro de control de interrupciones: la parte de abajo dispara el envio.
const ICR_BAJO: u64 = 0x300;
/// La parte de arriba, donde va a quien se le manda.
const ICR_ALTO: u64 = 0x310;
/// Registro de interrupcion espuria: su bit 8 prende el APIC.
const SVR: u64 = 0xF0;
/// Bit 12 del ICR: todavia no se entrego lo anterior.
const OCUPADO: u32 = 1 << 12;

unsafe fn apic_leer(base: u64, reg: u64) -> u32 {
    core::ptr::read_volatile((base + reg) as *const u32)
}

unsafe fn apic_escribir(base: u64, reg: u64, v: u32) {
    core::ptr::write_volatile((base + reg) as *mut u32, v);
}

/// Espera a que el APIC termine de entregar lo anterior.
unsafe fn esperar_entrega(base: u64) {
    let mut vueltas = 0;
    while apic_leer(base, ICR_BAJO) & OCUPADO != 0 && vueltas < 1_000_000 {
        core::hint::spin_loop();
        vueltas += 1;
    }
}

/// Una espera a ojo. Todavia no hay reloj: se cuenta en vueltas.
fn demorar(vueltas: u64) {
    for _ in 0..vueltas {
        core::hint::spin_loop();
    }
}

/// Manda la secuencia de arranque a un nucleo.
///
/// # Safety
///
/// `base` tiene que ser la direccion del APIC local, mapeada no cacheable.
unsafe fn init_sipi(base: u64, apic_id: u64, vector: u8) {
    let destino = (apic_id as u32) << 24;

    // INIT: lo deja en un estado conocido.
    apic_escribir(base, ICR_ALTO, destino);
    apic_escribir(base, ICR_BAJO, 0x0000_4500);
    esperar_entrega(base);
    demorar(10_000_000);

    // SIPI: le dice en que pagina arrancar. Se manda dos veces porque asi lo
    // pide Intel — la primera se puede perder si el nucleo todavia estaba
    // saliendo del INIT.
    for _ in 0..2 {
        apic_escribir(base, ICR_ALTO, destino);
        apic_escribir(base, ICR_BAJO, 0x0000_4600 | vector as u32);
        esperar_entrega(base);
        demorar(1_000_000);
    }
}

/// Copia el trampolin a memoria baja y arranca el nucleo.
///
/// # Safety
///
/// Las tablas de paginas y la captura de faults tienen que estar puestas.
pub unsafe fn start(hw: &Hardware, id: u64, slot: usize) -> Result<(), cores::Error> {
    let Some(apic) = hw.interrupts.filter(|i| i.kind == "apic") else {
        return Err(cores::Error::NoMechanism);
    };
    if slot >= cores::MAX {
        return Err(cores::Error::TableFull);
    }

    // El trampolin va a una direccion fija y baja. Si el agente reclamo
    // justo esa pagina, se avisa en vez de pisarsela.
    let fin = TRAMPOLIN + 4096;
    if claims::all().any(|c| TRAMPOLIN < c.end() && fin > c.start) {
        return Err(cores::Error::NoMechanism);
    }

    let inicio = core::ptr::addr_of!(AP_TRAMPOLIN_INICIO) as u64;
    let largo = core::ptr::addr_of!(AP_TRAMPOLIN_FIN) as u64 - inicio;
    core::ptr::copy_nonoverlapping(inicio as *const u8, TRAMPOLIN as *mut u8, largo as usize);

    // Los parametros: las mismas tablas de paginas que este nucleo, una pila
    // propia, adonde saltar, y cual es su ranura.
    let cr3: u64;
    core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));

    let pila = core::ptr::addr_of!(PILAS) as u64 + ((slot + 1) * TAM_PILA) as u64;

    let p = params();
    p.write(cr3);
    p.add(1).write(pila);
    p.add(2).write(ap_main as *const () as u64);
    p.add(3).write(slot as u64);

    // Prender el APIC por si el firmware lo dejo apagado.
    let svr = apic_leer(apic.address, SVR);
    apic_escribir(apic.address, SVR, svr | (1 << 8));

    init_sipi(apic.address, id, (TRAMPOLIN >> 12) as u8);
    Ok(())
}
