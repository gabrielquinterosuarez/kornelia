//! Arranque aarch64 por UEFI.

#![no_std]
#![no_main]

mod exec;
mod irq;
mod vectors;
mod paging;
mod percpu;
mod smp;
mod uart;

use core::ffi::c_void;
use core::panic::PanicInfo;
use kernel_core::fault::{Fault, Outcome};
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

    const REGISTERS: &'static [&'static str] = vectors::REGISTERS;

    unsafe fn install_fault_handlers(&mut self) -> Result<(), &'static str> {
        vectors::install(percpu::BOOT_SLOT)
    }

    fn trigger_breakpoint(&mut self) {
        vectors::breakpoint();
    }

    fn last_fault(&self) -> Option<Fault> {
        vectors::last()
    }

    unsafe fn install_serial_interrupt(
        &mut self,
        hw: &kernel_core::acpi::Hardware,
    ) -> Result<u8, &'static str> {
        irq::install(hw)
    }

    unsafe fn install_doorbell(
        &mut self,
        _hw: &kernel_core::acpi::Hardware,
    ) -> Result<kernel_core::channel::Doorbell, &'static str> {
        irq::install_doorbell()
    }

    unsafe fn install_irq(
        &mut self,
        hw: &kernel_core::acpi::Hardware,
        interrupt: u32,
        slot: usize,
        raw: bool,
    ) -> Result<kernel_core::channel::Doorbell, kernel_core::handlers::Error> {
        irq::install_agent(hw, interrupt, slot, raw)
    }

    unsafe fn set_user_access(
        &mut self,
        start: u64,
        bytes: u64,
        user: bool,
    ) -> Result<(), &'static str> {
        paging::set_user_access(&self.machine, start, bytes, user)
    }

    fn set_interrupts(&mut self, on: bool) {
        unsafe {
            if on {
                core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
            } else {
                core::arch::asm!("msr daifset, #2", options(nomem, nostack));
            }
        }
    }

    fn sleep(&mut self) {
        irq::sleep();
    }

    fn uart_address(&self) -> Option<u64> {
        Some(uart::BASE)
    }

    fn this_core(&self) -> u64 {
        smp::this_core()
    }

    unsafe fn start_core(
        &mut self,
        hw: &kernel_core::acpi::Hardware,
        id: u64,
        slot: usize,
    ) -> Result<(), kernel_core::cores::Error> {
        // Idempotente y barato: deja anotada la configuracion que va a copiar
        // el nucleo nuevo.
        smp::prepare();
        smp::start(hw.psci, id, slot)
    }

    unsafe fn exec(&mut self, entry: u64, region: (u64, u64), supervised: bool) -> Outcome {
        // En aarch64 las dos caches NO son coherentes: hay que empujar lo
        // escrito hasta donde lo ve el camino de instrucciones.
        exec::sync_cache(region.0, region.1);
        exec::run(entry, region, supervised)
    }

    /// `svc #0`. El agente no tiene que saber que es un `svc`: los recibe como
    /// bytes por `describe` y los pega al final de lo que emite (D3).
    const EXEC_RETURN: &'static [u8] = exec::RETURN_BYTES;
}

/// Escritor sobre el UART pelado, sin pasar por `Platform`.
///
/// Lo usa el handler de excepciones: ahí no hay una `Platform` a mano, y
/// tampoco conviene depender de una estructura que puede ser justo la que se
/// rompió.
pub struct SerialText;

impl core::fmt::Write for SerialText {
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
        jump_to_own_stack()
    }
}

/// Donde queda la maquina entre que se la describe y que arranca el kernel.
///
/// Se pasa por un estatico y no por argumento porque el salto de aca abajo
/// cambia la pila: cualquier cosa que estuviera en la pila vieja deja de ser
/// alcanzable en el momento en que SP se mueve.
static mut MACHINE: Machine = Machine::mute("no se llego a describir la maquina");

/// Corre ya sobre la pila propia del kernel.
extern "C" fn boot_core() -> ! {
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
unsafe fn jump_to_own_stack() -> ! {
    core::arch::asm!(
        // AArch64 no apila la direccion de retorno (va en X30), asi que SP
        // queda alineado a 16 tal cual, que es lo que exige el hardware.
        "mov sp, {top_of}",
        "bl {entry}",
        // `arrancar` no vuelve. Si algun dia volviera, es un bug.
        "brk #0",
        top_of = in(reg) kernel_core::stack::top(),
        entry = sym boot_core,
        options(noreturn),
    )
}


#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe { core::arch::asm!("msr daifset, #0xf; wfi", options(nomem, nostack)) }
    }
}
