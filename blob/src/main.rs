//! `blob.bin`: la distribucion reemplazable de D19/D20.
//!
//! # Que es esto y que no
//!
//! **No es el kernel.** Es codigo del agente pre-armado — exactamente lo que un
//! agente hubiera subido por el cable, pero ya compilado y puesto en la
//! particion, para que la maquina arranque sola sin que haya nadie del otro
//! lado (D20). Por eso no comparte una sola linea con los crates del kernel: lo
//! unico que tiene para pedirle a la maquina son los once verbos.
//!
//! # Como lo llama el kernel
//!
//! El firmware lo trae de la particion (D25: adentro de la ventana, porque solo
//! el firmware sabe leer FAT32), el kernel lo copia a memoria que reclamo y
//! **salta al byte cero**, sin privilegio (D29). El acuerdo es corto:
//!
//! - Entra con **su propia direccion** en el primer registro de argumento. Es
//!   lo unico que recibe, y alcanza: todo lo demas lo pide por la ventanilla.
//! - Para pedirle algo al kernel usa la puerta de servicio, que atiende **un**
//!   pedido y devuelve el control.
//! - Para terminar usa la otra puerta, con el valor que quiera dejar.
//!
//! Ojo con una confusion que quedo escrita en la documentacion vieja: el blob
//! **no** recibe un puntero a una funcion del kernel. Eso era cierto cuando
//! corria con privilegio completo; desde que corre sin privilegio (D29) no
//! puede llamar a una funcion del kernel ni aunque supiera donde esta, y el
//! camino es la puerta.

#![no_std]
#![no_main]

mod cbor;
mod gate;
mod kernel;
mod nvme;

use core::panic::PanicInfo;

/// Lo que deja cuando todo salio bien. Un numero reconocible: si aparece, corrio
/// **este** codigo y no lo que hubiera en esa memoria.
const OK: u64 = 0xB10B;
/// Lo que deja si su propia direccion no tiene sentido. Distinguirlo de `OK`
/// importa: significa que el acuerdo de entrada cambio, y eso no se ve como una
/// falla — se ve como un blob que no hace nada.
const BAD_ENTRY: u64 = 0xBADE;
/// Lo que deja si el pedido no entro en su buffer. No deberia pasar nunca: el
/// pedido es fijo y el buffer se eligio para el. Vale distinguirlo igual, porque
/// si algun dia el pedido crece el sintoma seria "el kernel no contesto".
const BAD_REQUEST: u64 = 0xBADC;
/// Y lo que deja si el kernel no contesto nada.
const NO_ANSWER: u64 = 0xBAD5;

/// Lo que deja si la maquina no informa donde se configura PCIe.
const NO_PCIE: u64 = 0xBADB;
/// Y lo que deja si recorrio el bus y no habia ningun NVMe.
const NO_DISK: u64 = 0xBADD;
/// Lo que deja si el controlador no llego a estar listo.
const NO_START: u64 = 0xBAD7;
/// Y si no dijo de que tamano es.
const NO_SIZE: u64 = 0xBAD6;

/// Como se identifica un controlador NVMe en el bus: **no** por fabricante y
/// modelo, sino por lo que hace. Los tres bytes son clase, subclase e interfaz
/// de programacion, y juntos quieren decir "almacenamiento / no volatil / NVMe".
///
/// Buscar asi es lo que hace que este cargador ande contra cualquier NVMe y no
/// contra el de QEMU (P4). Con una placa de red no se puede —`02.00.00` solo
/// dice "ethernet" y cada modelo tiene sus registros—, y por eso D19 pone el
/// disco primero: el driver de disco es el unico que sirve para todos.
const NVME_CLASS: u32 = 0x01_08_02;

/// Cuanto ocupa la ventana de configuracion de un bus.
const BUS_WINDOW: u64 = 1 << 20;

/// Por donde entra. Tiene que ser lo primero del binario porque el kernel salta
/// al byte cero; de eso se encarga `blob.ld`.
///
/// En x86_64 va `win64` **a proposito**: el kernel se compila para UEFI, donde
/// la ABI de C es la de Windows, y el primer argumento viaja en `rcx`. Compilado
/// para bare-metal, un `extern "C"` lo buscaria en `rdi` y leeria basura. La
/// convencion la pone el *target*, no el silicio.
#[cfg(target_arch = "x86_64")]
#[no_mangle]
#[link_section = ".text.entry"]
pub extern "win64" fn blob_entry(base: u64) -> ! {
    run(base)
}

/// En aarch64 hay una sola convencion y el primer argumento va en `x0`.
#[cfg(target_arch = "aarch64")]
#[no_mangle]
#[link_section = ".text.entry"]
pub extern "C" fn blob_entry(base: u64) -> ! {
    run(base)
}

/// Recorre el bus y devuelve el `bdf` del primer NVMe que encuentre.
///
/// El `bdf` es como el bus lo nombra —bus, dispositivo y funcion, todo junto—, y
/// es lo que hay que decirle al kernel para declararle DMA.
fn find_nvme(k: &mut kernel::Session, ecam: u64) -> Option<u64> {
    let cfg = k.claim_at(ecam, BUS_WINDOW)?;
    let mut found = None;

    for device in 0..32u64 {
        let at = device << 15;
        // Un lugar vacio del bus se lee como todos unos: no hay nadie que
        // conteste y el bus devuelve eso en vez de fallar.
        let who = match k.read_u32(cfg.handle, at) {
            Some(v) => v,
            None => continue,
        };
        if who == 0xFFFF_FFFF || who == 0 {
            continue;
        }
        let class = match k.read_u32(cfg.handle, at + 8) {
            Some(v) => v,
            None => continue,
        };
        // El byte de abajo es la revision; los tres de arriba, la clase.
        if (class >> 8) != NVME_CLASS {
            continue;
        }

        // Que responda a accesos de memoria y que pueda ser maestro del bus: sin
        // lo segundo no puede leer sus propias colas, que viven en RAM.
        k.write_u16(cfg.handle, at + 4, 0x0006);
        found = Some(device << 3);
        break;
    }

    // La ventana de configuracion se devuelve siempre: encontrar o no encontrar
    // no cambia que se la habia pedido prestada.
    k.release(cfg.handle);
    found
}

/// Donde tiene sus registros el aparato: su primer BAR.
///
/// Un BAR de 64 bits ocupa **dos** ranuras —los bits 2:1 en `10` dicen que la
/// mitad de arriba esta en la siguiente— y leer solo la primera da una direccion
/// truncada, que es peor que ninguna.
fn bar0(k: &mut kernel::Session, ecam: u64, bdf: u64) -> Option<u64> {
    let cfg = k.claim_at(ecam, BUS_WINDOW)?;
    let at = (bdf >> 3) << 15;
    let low = k.read_u32(cfg.handle, at + 0x10)?;
    let wide = (low & 0x6) == 0x4;
    let high = if wide { k.read_u32(cfg.handle, at + 0x14)? } else { 0 };
    k.release(cfg.handle);
    Some(((high as u64) << 32) | ((low & !0xF) as u64))
}

fn run(base: u64) -> ! {
    // Que la direccion propia haya llegado se comprueba, no se supone: si el
    // acuerdo de entrada se rompiera, todo lo que sigue apuntaria a cualquier
    // lado y el sintoma no se pareceria a la causa.
    if base == 0 || base % 4096 != 0 {
        gate::finish(BAD_ENTRY);
    }

    let mut k = kernel::Session::new();

    // Lo primero que necesita cualquier driver: donde preguntarle al bus. El
    // kernel publica la ventana de configuracion, no lo que hay conectado (D4).
    let Some(ecam) = k.pcie_base() else {
        gate::finish(NO_PCIE);
    };

    let Some(bdf) = find_nvme(&mut k, ecam) else {
        gate::finish(NO_DISK);
    };

    // Donde estan sus registros. Se vuelve a mirar el bus porque `find_nvme`
    // devolvio la ventana de configuracion antes de salir: lo que se toma se
    // devuelve, y despues se pide de nuevo lo que haga falta.
    let Some(window) = bar0(&mut k, ecam, bdf) else {
        gate::finish(NO_DISK);
    };

    let Some(mut disk) = nvme::Nvme::start(&mut k, bdf, window) else {
        gate::finish(NO_START);
    };

    let Some(ns) = disk.namespace(&mut k, 1) else {
        gate::finish(NO_SIZE);
    };

    // Se devuelve **cuantos bloques tiene el disco**, que es un dato de la
    // maquina y no del blob: sale de la ficha que el controlador escribio por
    // DMA. El disco de prueba mide 16 MiB, asi que tienen que ser 32768 de 512
    // bytes — y eso se puede comprobar contra el `dd` que lo creo, que es un
    // lugar completamente distinto.
    let _ = ns.block_bytes;
    gate::finish(OK | (ns.blocks << 16));
}

/// Un panic no puede contar nada —no hay por donde—, asi que sale por la misma
/// puerta con un valor distinto. Que el arranque siga es cosa del kernel: un
/// blob que falla vuelve como fault y la maquina sigue hasta el protocolo (P5).
#[panic_handler]
fn panicked(_: &PanicInfo) -> ! {
    gate::finish(0xDEAD)
}
