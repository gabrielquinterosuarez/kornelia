//! Arranque aarch64 por UEFI.

#![no_std]
#![no_main]

mod uart;

use core::ffi::c_void;
use core::panic::PanicInfo;
use kernel_core::Platform;

struct AArch64;

impl Platform for AArch64 {
    const ARCH: &'static str = "aarch64";

    fn uart_write_byte(&mut self, b: u8) {
        uart::write_byte(b);
    }

    fn park(&mut self) -> ! {
        loop {
            unsafe { core::arch::asm!("msr daifset, #0xf; wfi", options(nomem, nostack)) }
        }
    }
}

/// Entrada que llama el firmware UEFI.
#[no_mangle]
pub extern "efiapi" fn efi_main(_image: *mut c_void, _systab: *mut c_void) -> usize {
    kernel_core::main(&mut AArch64)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe { core::arch::asm!("msr daifset, #0xf; wfi", options(nomem, nostack)) }
    }
}
