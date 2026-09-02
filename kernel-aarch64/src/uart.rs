//! UART PL011. El cordón umbilical (D5) del lado ARM.
//!
//! A diferencia de x86, ARM no tiene puertos de E/S: los registros del UART son
//! memoria (MMIO). Y la dirección no es fija por arquitectura sino **por
//! placa**, así que no se puede saber sin preguntarle a la máquina.
//!
//! El firmware UEFI ya lo dejó configurado, así que no hace falta init.
//!
//! # La dirección se hereda, pero no se queda
//!
//! Hay una horneada porque el kernel tiene que poder hablar **antes** de leer
//! ninguna tabla: si el arranque se cuelga leyendo ACPI, lo único que queda
//! para contarlo es el cable. Pero es el punto de partida, no la respuesta —
//! apenas la tabla SPCR dice dónde tiene la máquina su consola, el kernel se
//! muda ahí. Con lo cual la dirección horneada deja de ser algo que el kernel
//! **cree** y pasa a ser algo con lo que arranca (P4).

/// Con qué dirección se arranca: la de la máquina `virt` de QEMU.
const BUILT_IN: u64 = 0x0900_0000;

static mut BASE: u64 = BUILT_IN;

/// Si la dirección en uso es la que informó la máquina, o todavía la horneada.
static mut FROM_MACHINE: bool = false;

/// Dónde están los registros del UART ahora mismo.
pub fn base() -> u64 {
    unsafe { BASE }
}

/// Si la dirección en uso salió de la máquina.
///
/// Se publica porque es la diferencia entre "anda" y "anda **porque la máquina
/// dijo dónde**", que es lo único que hace que ande en otra placa (P4).
pub fn from_machine() -> bool {
    unsafe { FROM_MACHINE }
}

/// Se muda a la dirección que informó la máquina.
///
/// Si esa dirección fuera mala, el cordón se pierde acá mismo — por eso quien
/// llama comprueba antes que sea alcanzable, y por eso este es el último
/// momento en que todavía se puede avisar por la vieja.
///
/// # Safety
///
/// `addr` tiene que ser la ventana de registros de un PL011, mapeada como
/// memoria de dispositivo.
pub unsafe fn move_to(addr: u64) {
    BASE = addr;
    FROM_MACHINE = true;
}

fn reg(off: usize) -> usize {
    base() as usize + off
}

const UARTDR: usize = 0x00; // dato
const UARTFR: usize = 0x18; // banderas

const UARTIMSC: usize = 0x38; // que interrupciones estan permitidas
const UARTICR: usize = 0x44; // limpiar interrupciones

const IMSC_RXIM: u32 = 1 << 4; // llego un byte
const IMSC_RTIM: u32 = 1 << 6; // llego algo y dejo de llegar (cola a medias)

const FR_RXFE: u32 = 1 << 4; // cola de recepción vacía
const FR_TXFF: u32 = 1 << 5; // cola de transmisión llena

pub fn write_byte(b: u8) {
    unsafe {
        while core::ptr::read_volatile(reg(UARTFR) as *const u32) & FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile(reg(UARTDR) as *mut u32, b as u32);
    }
}

/// Le dice al UART que levante la mano cuando llegue un byte.
///
/// Se piden las dos: "llego un byte" y "llego algo y dejo de llegar". La segunda
/// hace falta porque el UART junta bytes en una cola y avisa cuando se llena;
/// sin ella, un pedido que no llene la cola se quedaria esperando a un byte que
/// no va a venir.
pub fn enable_rx_interrupt() {
    unsafe { core::ptr::write_volatile(reg(UARTIMSC) as *mut u32, IMSC_RXIM | IMSC_RTIM) };
}

/// Baja la mano del UART. Se llama antes de vaciar la cola.
pub fn clear_rx_interrupt() {
    unsafe { core::ptr::write_volatile(reg(UARTICR) as *mut u32, IMSC_RXIM | IMSC_RTIM) };
}

/// Levanta un byte si hay alguno esperando. No bloquea.
///
/// El registro de datos es de 32 bits, pero solo los 8 de abajo son el byte:
/// los de arriba son banderas de error de la línea (paridad, framing, overrun)
/// que todavía no se miran.
pub fn read_byte() -> Option<u8> {
    unsafe {
        if core::ptr::read_volatile(reg(UARTFR) as *const u32) & FR_RXFE != 0 {
            None
        } else {
            Some(core::ptr::read_volatile(reg(UARTDR) as *const u32) as u8)
        }
    }
}
