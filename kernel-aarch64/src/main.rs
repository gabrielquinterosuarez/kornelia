//! Arranque aarch64 por UEFI.

#![no_std]
#![no_main]

mod vectors;
mod paging;
mod uart;

use core::ffi::c_void;
use core::panic::PanicInfo;
use kernel_core::fault::Fault;
use kernel_core::machine::Machine;
use kernel_core::paging::Mapping;
use kernel_core::Platform;

struct AArch64 {
    machine: Machine,
}

impl Platform for AArch64 {
    const ARCH: &'static str = "aarch64";

    fn uart_write_byte(&mut self, b: u8) {
        uart::write_byte(b);
    }

    fn uart_read_byte(&mut self) -> Option<u8> {
        uart::read_byte()
    }

    fn park(&mut self) -> ! {
        loop {
            unsafe { core::arch::asm!("msr daifset, #0xf; wfi", options(nomem, nostack)) }
        }
    }

    fn machine(&self) -> Machine {
        self.machine
    }

    unsafe fn install_page_tables(&mut self, m: &Machine) -> Result<Mapping, &'static str> {
        paging::install(m)
    }

    const REGISTERS: &'static [&'static str] = vectors::REGISTROS;

    unsafe fn install_fault_handlers(&mut self) -> Result<(), &'static str> {
        vectors::install()
    }

    fn trigger_breakpoint(&mut self) {
        vectors::breakpoint();
    }

    fn last_fault(&self) -> Option<Fault> {
        vectors::last()
    }
}

/// Escritor sobre el UART pelado, sin pasar por `Platform`.
///
/// Lo usa el handler de excepciones: ahí no hay una `Platform` a mano, y
/// tampoco conviene depender de una estructura que puede ser justo la que se
/// rompió.
pub struct Serie;

impl core::fmt::Write for Serie {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.as_bytes() {
            uart::write_byte(*b);
        }
        Ok(())
    }
}

/// Entrada que llama el firmware UEFI.
#[no_mangle]
pub extern "efiapi" fn efi_main(image: *mut c_void, systab: *mut c_void) -> usize {
    // La única ventana para preguntarle al firmware, y se cierra sola (D25).
    // Al volver de acá la máquina es nuestra y los Boot Services ya no existen.
    let machine = unsafe { boot_uefi::take_machine(image, systab.cast()) };

    // Ya no queda nada por pedirle al firmware, asi que se abandona su pila.
    unsafe {
        MACHINE = machine;
        saltar_a_la_pila_propia()
    }
}

/// Donde queda la maquina entre que se la describe y que arranca el kernel.
///
/// Se pasa por un estatico y no por argumento porque el salto de aca abajo
/// cambia la pila: cualquier cosa que estuviera en la pila vieja deja de ser
/// alcanzable en el momento en que SP se mueve.
static mut MACHINE: Machine = Machine::mute("no se llego a describir la maquina");

/// Corre ya sobre la pila propia del kernel.
extern "C" fn arrancar() -> ! {
    let machine = unsafe { MACHINE };
    kernel_core::main(&mut AArch64 { machine })
}

/// Se muda a la pila del kernel y salta.
///
/// Hasta este punto se corria sobre la pila que dio el firmware, que vive en
/// memoria que `ExitBootServices` acaba de convertir en RAM libre. Seguir ahi
/// significaria que `mem.claim` puede entregarle al agente la pila de abajo de
/// nuestros pies.
///
/// # Safety
///
/// Solo se puede llamar cuando ya no queda nada por hacer con el firmware: al
/// mover SP se pierde todo lo que hubiera en la pila vieja, incluida la
/// direccion de retorno a quien nos llamo.
unsafe fn saltar_a_la_pila_propia() -> ! {
    core::arch::asm!(
        // AArch64 no apila la direccion de retorno (va en X30), asi que SP
        // queda alineado a 16 tal cual, que es lo que exige el hardware.
        "mov sp, {cima}",
        "bl {entrada}",
        // `arrancar` no vuelve. Si algun dia volviera, es un bug.
        "brk #0",
        cima = in(reg) kernel_core::stack::top(),
        entrada = sym arrancar,
        options(noreturn),
    )
}


#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe { core::arch::asm!("msr daifset, #0xf; wfi", options(nomem, nostack)) }
    }
}
