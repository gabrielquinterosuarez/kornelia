//! Arranque x86_64 por UEFI.

#![no_std]
#![no_main]

mod exec;
mod gdt;
mod idt;
mod irq;
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

    unsafe fn install_page_tables(&mut self, m: &Machine) -> Result<Mapping, &'static str> {
        paging::install(m)
    }

    const REGISTERS: &'static [&'static str] = idt::REGISTROS;

    unsafe fn install_fault_handlers(&mut self) -> Result<(), &'static str> {
        idt::install(percpu::RANURA_ARRANQUE)
    }

    fn trigger_breakpoint(&mut self) {
        idt::breakpoint();
    }

    fn last_fault(&self) -> Option<Fault> {
        idt::last()
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
        irq::install_agente(hw, interrupt, slot, raw)
    }

    fn set_interrupts(&mut self, on: bool) {
        unsafe {
            if on {
                core::arch::asm!("sti", options(nomem, nostack));
            } else {
                core::arch::asm!("cli", options(nomem, nostack));
            }
        }
    }

    fn sleep(&mut self) {
        irq::sleep();
    }

    fn uart_address(&self) -> Option<u64> {
        None
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
        smp::start(hw, id, slot)
    }

    unsafe fn exec(&mut self, entry: u64, _region: (u64, u64)) -> Outcome {
        // x86_64 mantiene coherente la cache de instrucciones con la de datos:
        // codigo recien escrito se ve sin pedir nada.
        exec::run(entry)
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
    // El UART primero: si lo que sigue falla, hace falta poder contarlo.
    uart::init();

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
    kernel_core::main(&mut X86_64 { machine })
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
        // La cima esta alineada a 16. `call` apila 8 bytes de retorno, con lo
        // que la funcion arranca con RSP%16==8, que es justo lo que pide la ABI
        // de System V.
        "mov rsp, {cima}",
        "call {entrada}",
        // `arrancar` no vuelve. Si algun dia volviera, es un bug y conviene que
        // se detenga acá y no que siga por la pila con basura.
        "ud2",
        cima = in(reg) kernel_core::stack::top(),
        entrada = sym arrancar,
        options(noreturn),
    )
}


#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // D7: los faults son datos. Todavía no hay canal para reportarlos, así que
    // por ahora el núcleo se detiene en silencio.
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) }
    }
}
