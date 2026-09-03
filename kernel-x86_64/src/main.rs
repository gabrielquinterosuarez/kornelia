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

    const ARGUMENTS: &'static [usize] = exec::ARGUMENTS;

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

    fn clock(&self) -> Option<kernel_core::platform::Clock> {
        clock::describe()
    }

    fn ticks(&self) -> u64 {
        clock::ticks()
    }

    unsafe fn calibrate_clock(&mut self, hw: &kernel_core::acpi::Hardware) {
        clock::calibrate(hw);
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
            return Err("this machine does not report an IOMMU");
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
            return Err("this machine does not report an IOMMU");
        };
        iommu::set_access(&unit, device, start, bytes, allow)
    }

    fn wake_core(&mut self, id: u64) {
        irq::wake(id);
    }

    fn stop_core(&mut self, id: u64) -> bool {
        irq::stop(id)
    }

    unsafe fn set_deadline(&mut self, at: Option<u64>) {
        irq::set_deadline(at);
    }

    unsafe fn install_msi(
        &mut self,
        hw: &kernel_core::acpi::Hardware,
        slot: usize,
        raw: bool,
    ) -> Result<(u32, kernel_core::channel::Doorbell), kernel_core::handlers::Error> {
        let _ = hw;
        irq::install_msi(slot, raw)
    }

    unsafe fn install_deadline(&mut self) -> Result<(), &'static str> {
        irq::install_deadline(clock::hz())
    }

    fn deadline_ready(&self) -> bool {
        irq::deadline_ready()
    }

    /// El NMI: `cli` no lo puede tapar.
    const CAN_STOP_CORES: bool = true;

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
static mut MACHINE: Machine = Machine::mute("the machine was never described");

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

/// El contador de la arquitectura y su frecuencia (deuda 17).
///
/// Aca esta la mitad dificil. El TSC cuenta **ciclos**, no tiempo, asi que hace
/// falta saber cuantos ciclos son un segundo — y eso el CPU lo informa en dos
/// lugares distintos, ninguno obligatorio:
///
/// - la hoja 0x15 de CPUID da el cristal del que cuelga el TSC y la proporcion;
/// - la 0x16 da la frecuencia base en MHz, que es menos exacta pero alcanza.
///
/// Si ninguna dice nada, se devuelve `None` en vez de inventar un numero: un
/// tiempo mal calculado es peor que no tener tiempo (P4).
pub mod clock {
    use kernel_core::platform::Clock;

    /// Le pregunta al CPU por una de sus hojas de informacion.
    pub fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
        let (mut a, mut b, mut c, mut d): (u32, u32, u32, u32);
        unsafe {
            core::arch::asm!(
                // `rbx` lo pide el ABI de vuelta como estaba, asi que se guarda.
                "xchg {b:e}, ebx",
                "cpuid",
                "xchg {b:e}, ebx",
                b = out(reg) b,
                inout("eax") leaf => a,
                inout("ecx") 0u32 => c,
                out("edx") d,
                options(nostack, preserves_flags),
            );
        }
        (a, b, c, d)
    }

    /// Cuantas hojas de CPUID tiene este CPU. Preguntar por una que no existe
    /// devuelve basura de otra, asi que se comprueba primero.
    fn max_leaf() -> u32 {
        cpuid(0).0
    }

    /// Lo que dio la calibracion, si hubo que calibrar. Se mide una sola vez.
    static mut MEASURED_HZ: u64 = 0;

    /// Mide el TSC contra el contador de frecuencia fija que informa ACPI.
    ///
    /// El de ACPI sube siempre a 3.579545 MHz, en cualquier maquina, y eso es lo
    /// que lo hace util: se cuentan los ciclos del TSC que caben en un pedazo
    /// conocido de ese otro contador.
    ///
    /// # Safety
    ///
    /// El puerto tiene que ser el que informo la FADT.
    pub unsafe fn calibrate(hw: &kernel_core::acpi::Hardware) {
        // Si el CPU ya dice su frecuencia, no hay nada que medir.
        if describe().is_some() {
            return;
        }
        let Some(timer) = hw.timer else { return };

        // El contador de ACPI puede ser de 24 bits, asi que se envuelve antes de
        // lo que uno espera: se mide un pedazo corto y se comparan solo los bits
        // que seguro existen.
        let mask: u32 = if timer.wide { u32::MAX } else { 0x00FF_FFFF };
        // Un cuarto de vuelta de un contador de 24 bits son ~4.7 ms. Alcanza
        // para medir con precision de sobra y no demora el arranque.
        let span = kernel_core::acpi::TIMER_HZ as u32 / 200;

        let start_timer = read_port(timer.port) & mask;
        let start_tsc = ticks();

        // Se espera a que el contador de ACPI avance `span`. La cuenta va con
        // resta enmascarada para que dar la vuelta no la rompa.
        let mut rounds = 0u64;
        loop {
            let now = read_port(timer.port) & mask;
            if now.wrapping_sub(start_timer) & mask >= span {
                break;
            }
            rounds += 1;
            // Red: si el contador no avanza —porque el puerto no era ese— esto
            // no puede quedarse girando en el arranque.
            if rounds > 200_000_000 {
                return;
            }
            core::hint::spin_loop();
        }

        let elapsed_tsc = ticks().wrapping_sub(start_tsc);
        let elapsed_timer = (read_port(timer.port) & mask).wrapping_sub(start_timer) & mask;
        if elapsed_timer == 0 || elapsed_tsc == 0 {
            return;
        }
        MEASURED_HZ =
            elapsed_tsc * kernel_core::acpi::TIMER_HZ / elapsed_timer as u64;
    }

    /// Lee un puerto de E/S de 32 bits. Los puertos no son memoria: son otro
    /// espacio de direcciones, con sus propias instrucciones.
    unsafe fn read_port(port: u32) -> u32 {
        let value: u32;
        core::arch::asm!("in eax, dx", out("eax") value, in("dx") port as u16,
                         options(nomem, nostack, preserves_flags));
        value
    }

    pub fn describe() -> Option<Clock> {
        // Lo medido gana: si hubo que calibrar es porque el CPU no lo dijo.
        let measured = unsafe { MEASURED_HZ };
        if measured != 0 {
            return Some(Clock { kind: "tsc", hz: measured });
        }
        let top = max_leaf();

        // Hoja 0x15: el cristal y la proporcion. `ecx` es el cristal en Hz,
        // `ebx`/`eax` la proporcion entre el TSC y ese cristal. Es la exacta.
        if top >= 0x15 {
            let (denom, numer, crystal, _) = cpuid(0x15);
            if denom != 0 && numer != 0 && crystal != 0 {
                let hz = crystal as u64 * numer as u64 / denom as u64;
                return Some(Clock { kind: "tsc", hz });
            }
        }

        // Hoja 0x16: la frecuencia base en MHz. Redondeada, pero para una
        // ventana de rescate la diferencia no cambia nada.
        if top >= 0x16 {
            let (base_mhz, _, _, _) = cpuid(0x16);
            if base_mhz != 0 {
                return Some(Clock { kind: "tsc", hz: base_mhz as u64 * 1_000_000 });
            }
        }

        None
    }

    /// A que ritmo sube el TSC, o cero si no se sabe. Lo necesita el reloj del
    /// APIC para traducir un plazo de un contador al otro.
    pub fn hz() -> u64 {
        describe().map(|c| c.hz).unwrap_or(0)
    }

    pub fn ticks() -> u64 {
        let (low, high): (u32, u32);
        unsafe {
            // `lfence` antes de `rdtsc`: sin eso el procesador puede adelantar
            // la lectura y dos medidas seguidas salen al reves.
            //
            // Y `lfence; rdtsc` en vez de `rdtscp`, que hace lo mismo en una
            // instruccion: `rdtscp` es **opcional** y en un CPU que no lo tiene
            // es un opcode invalido — o sea un fault en el arranque, antes de
            // que haya con que contarlo. `lfence` viene con SSE2, que en x86_64
            // es obligatorio.
            core::arch::asm!(
                "lfence",
                "rdtsc",
                out("eax") low,
                out("edx") high,
                options(nomem, nostack),
            );
        }
        ((high as u64) << 32) | low as u64
    }
}
