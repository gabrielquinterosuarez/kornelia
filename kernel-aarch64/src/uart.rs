//! UART PL011. El cordón umbilical (D5) del lado ARM.
//!
//! A diferencia de x86, ARM no tiene puertos de E/S: los registros del UART son
//! memoria (MMIO). La dirección no es fija por arquitectura sino por placa —
//! esta es la de la máquina `virt` de QEMU. En hardware real sale del device
//! tree, que es lo que hará `describe` cuando exista.
//!
//! El firmware UEFI ya lo dejó configurado, así que no hace falta init.

const PL011_BASE: usize = 0x0900_0000;

const UARTDR: usize = PL011_BASE + 0x00; // dato
const UARTFR: usize = PL011_BASE + 0x18; // banderas

const FR_TXFF: u32 = 1 << 5; // cola de transmisión llena

pub fn write_byte(b: u8) {
    unsafe {
        while core::ptr::read_volatile(UARTFR as *const u32) & FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile(UARTDR as *mut u32, b as u32);
    }
}
