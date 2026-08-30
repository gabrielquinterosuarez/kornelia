//! Arranque x86_64 por UEFI.

#![no_std]
#![no_main]

mod uart;

use core::ffi::c_void;
use core::panic::PanicInfo;
use kernel_core::Platform;

struct X86_64;

impl Platform for X86_64 {
    const ARCH: &'static str = "x86_64";

    fn uart_write_byte(&mut self, b: u8) {
        uart::write_byte(b);
    }

    fn park(&mut self) -> ! {
        loop {
            unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) }
        }
    }
}

/// Entrada que llama el firmware UEFI.
#[no_mangle]
pub extern "efiapi" fn efi_main(_image: *mut c_void, _systab: *mut c_void) -> usize {
    uart::init();
    kernel_core::main(&mut X86_64)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // D7: los faults son datos. Todavía no hay canal para reportarlos, así que
    // por ahora el núcleo se detiene en silencio.
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) }
    }
}
