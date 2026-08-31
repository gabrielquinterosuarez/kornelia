//! UART PL011. El cordón umbilical (D5) del lado ARM.
//!
//! A diferencia de x86, ARM no tiene puertos de E/S: los registros del UART son
//! memoria (MMIO). La dirección no es fija por arquitectura sino por placa —
//! esta es la de la máquina `virt` de QEMU. En hardware real sale del device
//! tree, que es lo que hará `describe` cuando exista.
//!
//! El firmware UEFI ya lo dejó configurado, así que no hace falta init.

/// La direccion de la maquina `virt` de QEMU.
///
/// Esta horneada porque el kernel necesita poder hablar antes de leer ninguna
/// tabla de ACPI. Pero **no se da por buena**: el arranque la contrasta contra
/// lo que dice la tabla SPCR y avisa si no coinciden (ver `Platform::uart_address`).
pub const BASE: u64 = 0x0900_0000;

const PL011_BASE: usize = BASE as usize;

const UARTDR: usize = PL011_BASE + 0x00; // dato
const UARTFR: usize = PL011_BASE + 0x18; // banderas

const FR_RXFE: u32 = 1 << 4; // cola de recepción vacía
const FR_TXFF: u32 = 1 << 5; // cola de transmisión llena

pub fn write_byte(b: u8) {
    unsafe {
        while core::ptr::read_volatile(UARTFR as *const u32) & FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile(UARTDR as *mut u32, b as u32);
    }
}

/// Levanta un byte si hay alguno esperando. No bloquea.
///
/// El registro de datos es de 32 bits, pero solo los 8 de abajo son el byte:
/// los de arriba son banderas de error de la línea (paridad, framing, overrun)
/// que todavía no se miran.
pub fn read_byte() -> Option<u8> {
    unsafe {
        if core::ptr::read_volatile(UARTFR as *const u32) & FR_RXFE != 0 {
            None
        } else {
            Some(core::ptr::read_volatile(UARTDR as *const u32) as u8)
        }
    }
}
