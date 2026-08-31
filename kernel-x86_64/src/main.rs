//! Arranque x86_64 por UEFI.

#![no_std]
#![no_main]

mod uart;

use core::ffi::c_void;
use core::panic::PanicInfo;
use kernel_core::{Machine, Platform};

struct X86_64 {
    machine: Machine,
}

impl Platform for X86_64 {
    const ARCH: &'static str = "x86_64";

    fn uart_write_byte(&mut self, b: u8) {
        uart::write_byte(b);
    }

    fn uart_read_byte(&mut self) -> Option<u8> {
        uart::read_byte()
    }

    fn park(&mut self) -> ! {
        loop {
            unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) }
        }
    }

    fn machine(&self) -> Machine {
        self.machine
    }
}

/// Entrada que llama el firmware UEFI.
#[no_mangle]
pub extern "efiapi" fn efi_main(image: *mut c_void, systab: *mut c_void) -> usize {
    // El UART primero: si lo que sigue falla, hace falta poder contarlo.
    uart::init();

    // La única ventana para preguntarle al firmware, y se cierra sola (D25).
    // Al volver de acá la máquina es nuestra y los Boot Services ya no existen.
    let machine = unsafe { boot_uefi::take_machine(image, systab.cast()) };

    kernel_core::main(&mut X86_64 { machine })
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // D7: los faults son datos. Todavía no hay canal para reportarlos, así que
    // por ahora el núcleo se detiene en silencio.
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) }
    }
}
