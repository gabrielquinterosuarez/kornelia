//! UART 16550 en COM1. El cordón umbilical (D5).
//!
//! Puertos de E/S fijos, sin enumeración PCI: es el driver más chico que existe
//! y por eso es el único que el kernel lleva adentro (D4).

const COM1: u16 = 0x3F8;

const DATA: u16 = COM1; // + Divisor Latch Low cuando DLAB=1
const IER: u16 = COM1 + 1; // + Divisor Latch High cuando DLAB=1
const FCR: u16 = COM1 + 2;
const LCR: u16 = COM1 + 3;
const MCR: u16 = COM1 + 4;
const LSR: u16 = COM1 + 5;

const LSR_DATA_READY: u8 = 1 << 0; // hay un byte esperando en la FIFO de entrada
const LSR_THR_EMPTY: u8 = 1 << 5; // el registro de salida quedo libre

unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack));
    value
}

pub fn init() {
    unsafe {
        outb(IER, 0x00); // sin interrupciones: por ahora se sondea
        outb(LCR, 0x80); // DLAB=1 para fijar la velocidad
        outb(DATA, 0x01); // divisor = 1  ->  115200 baud
        outb(IER, 0x00);
        outb(LCR, 0x03); // 8 bits, sin paridad, 1 stop; DLAB=0
        outb(FCR, 0xC7); // FIFO encendida y vaciada, umbral 14 bytes
        outb(MCR, 0x0B); // DTR + RTS + OUT2
    }
}

pub fn write_byte(b: u8) {
    unsafe {
        while inb(LSR) & LSR_THR_EMPTY == 0 {
            core::hint::spin_loop();
        }
        outb(DATA, b);
    }
}

/// Le dice al UART que levante la mano cuando llegue un byte.
///
/// Hasta que no se llama esto, el UART recibe en silencio y hay que preguntarle.
pub fn enable_rx_interrupt() {
    // Bit 0 del registro de habilitacion: avisar cuando haya dato disponible.
    unsafe { outb(IER, 0x01) };
}

/// Levanta un byte si hay alguno esperando. No bloquea.
pub fn read_byte() -> Option<u8> {
    unsafe {
        if inb(LSR) & LSR_DATA_READY == 0 {
            None
        } else {
            Some(inb(DATA))
        }
    }
}
