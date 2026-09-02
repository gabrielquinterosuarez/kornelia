//! Arranque x86_64 por UEFI.

#![no_std]
#![no_main]

mod exec;
mod guarded;
mod gdt;
mod idt;
mod iommu;
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

    const REGISTERS: &'static [&'static str] = idt::REGISTERS;

    unsafe fn install_fault_handlers(&mut self) -> Result<(), &'static str> {
        idt::install(percpu::BOOT_SLOT)
    }

    fn trigger_breakpoint(&mut self) {
        idt::breakpoint();
    }

    unsafe fn guarded_read(&mut self, addr: u64, width: u64) -> Option<u64> {
        guarded::read(addr, width)
    }

    unsafe fn guarded_write(&mut self, addr: u64, width: u64, value: u64) -> bool {
        guarded::write(addr, width, value)
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
                core::arch::asm!("sti", options(nomem, nostack));
            } else {
                core::arch::asm!("cli", options(nomem, nostack));
            }
        }
    }

    fn sleep(&mut self) {
        irq::sleep();
    }

    /// En x86_64 el UART esta en puertos de E/S, que no son direcciones de
    /// memoria: no hay a donde mudarse y decirlo es la respuesta correcta.
    unsafe fn use_serial_at(&mut self, _addr: u64) -> bool {
        false
    }

    fn serial_from_machine(&self) -> bool {
        false
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

    unsafe fn enable_iommu(
        &mut self,
        hw: &kernel_core::acpi::Hardware,
    ) -> Result<&'static str, &'static str> {
        let Some(unit) = hw.iommu else {
            return Err("esta maquina no informa un IOMMU");
        };
        iommu::install(&unit)?;
        Ok(unit.kind)
    }

    fn iommu_enabled(&self) -> bool {
        iommu::is_on()
    }

    fn dma_faults(&self) -> Option<u64> {
        iommu::faults()
    }

    unsafe fn set_dma_access(
        &mut self,
        hw: &kernel_core::acpi::Hardware,
        device: u32,
        start: u64,
        bytes: u64,
        allow: bool,
    ) -> Result<(), &'static str> {
        let Some(unit) = hw.iommu else {
            return Err("esta maquina no informa un IOMMU");
        };
        iommu::set_access(&unit, device, start, bytes, allow)
    }

    fn wake_core(&mut self, id: u64) {
        irq::wake(id);
    }

    const EXEC_INITIAL: &'static [&'static str] = exec::INITIAL;

    unsafe fn exec(
        &mut self,
        entry: u64,
        region: (u64, u64),
        supervised: bool,
        initial: &[Option<u64>],
    ) -> Outcome {
        // x86_64 mantiene coherente la cache de instrucciones con la de datos:
        // codigo recien escrito se ve sin pedir nada. La region hace falta
        // igual: de ahi sale la pila cuando corre supervisado (D27).
        exec::run(entry, region, supervised, initial)
    }

    /// `int 0x80`. El agente no tiene que saber que es un `int`: los recibe
    /// como bytes por `describe` y los pega al final de lo que emite (D3).
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
    // El UART primero: si lo que sigue falla, hace falta poder contarlo.
    uart::init();

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

/// La plataforma de este nucleo.
///
/// La arma cada nucleo por su cuenta: no es estado compartido sino la puerta a
/// lo que ya esta puesto. Un nucleo reclamado la necesita para poder correr el
/// codigo del agente que le dejaron en el buzon.
pub fn platform() -> impl Platform {
    // SAFETY: `MACHINE` se escribe una sola vez, en el arranque, antes de que
    // exista cualquier otro nucleo.
    X86_64 { machine: unsafe { MACHINE } }
}

/// Corre ya sobre la pila propia del kernel.
extern "C" fn boot_core() -> ! {
    kernel_core::main(&mut platform())
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
        // La cima esta alineada a 16. `call` apila 8 bytes de retorno, con lo
        // que la funcion arranca con RSP%16==8, que es justo lo que pide la ABI
        // de System V.
        "mov rsp, {top_of}",
        "call {entry}",
        // `arrancar` no vuelve. Si algun dia volviera, es un bug y conviene que
        // se detenga acá y no que siga por la pila con basura.
        "ud2",
        top_of = in(reg) kernel_core::stack::top(),
        entry = sym boot_core,
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
