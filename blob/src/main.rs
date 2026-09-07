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

/// Cuanta memoria le pide al kernel, para demostrar que la ventanilla anda.
///
/// Es un numero raro **a proposito**, y distinto del que pide el blob de prueba
/// que arma `client.py`: asi, al mirar los reclamos de la maquina desde afuera,
/// no hay duda de quien lo pidio. Un reclamo de este tamano solo puede haberlo
/// hecho este codigo, y no paso por el cable.
const CLAIM_BYTES: u64 = 0x9000;

/// Cuanto lugar deja para la respuesta del kernel.
const REPLY_ROOM: usize = 256;

/// Con que identifica su pedido. Cualquier numero sirve: el kernel lo devuelve
/// tal cual para que quien pregunto reconozca la respuesta.
const REQUEST_ID: u64 = 0x0B10;

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

fn run(base: u64) -> ! {
    // Que la direccion propia haya llegado se comprueba, no se supone: si el
    // acuerdo de entrada se rompiera, todo lo que sigue apuntaria a cualquier
    // lado y el sintoma no se pareceria a la causa.
    if base == 0 || base % 4096 != 0 {
        gate::finish(BAD_ENTRY);
    }

    // Los dos buffers van en la **pila**, que sale del final de la memoria que
    // el kernel reclamo para el blob (D27). No pueden ser estaticos: un binario
    // plano no trae `.bss`, asi que un estatico mutable arrancaria con lo que
    // hubiera en esa memoria — y eso se ve como un blob que a veces anda.
    let mut request = [0u8; 64];
    // Sin inicializar **a proposito**: lo llena el kernel, y ponerlo en cero
    // haria que el compilador llame a `memset` — que es una llamada a otro
    // objeto, o sea una entrada en la GOT, o sea un blob que no anda (ver
    // `blob.ld`). Lo que el kernel no escriba no se lee: `service` dice cuanto
    // dejo.
    let mut reply = core::mem::MaybeUninit::<[u8; REPLY_ROOM]>::uninit();

    // El pedido: reclamar memoria. Es el mas simple que deja una huella visible
    // desde afuera, que es lo que hace que esto se pueda comprobar sin creerle
    // al blob.
    let mut w = cbor::Writer::new(&mut request);
    w.array(3);
    w.uint(REQUEST_ID);
    w.text("mem.claim");
    w.map(2);
    w.text("bytes");
    w.uint(CLAIM_BYTES);
    w.text("align");
    w.uint(4096);
    let len = w.done();
    if len == 0 {
        gate::finish(BAD_REQUEST);
    }

    // SAFETY: los dos buffers son suyos y viven hasta que vuelva.
    let used = unsafe {
        gate::service(request.as_ptr(), len, reply.as_mut_ptr() as *mut u8, REPLY_ROOM)
    };
    if used == 0 {
        gate::finish(NO_ANSWER);
    }

    // Que la respuesta diga lo correcto no se comprueba aca: se comprueba desde
    // afuera, mirando si el reclamo quedo hecho. El blob no es el testigo de si
    // mismo.
    gate::finish(OK);
}

/// Un panic no puede contar nada —no hay por donde—, asi que sale por la misma
/// puerta con un valor distinto. Que el arranque siga es cosa del kernel: un
/// blob que falla vuelve como fault y la maquina sigue hasta el protocolo (P5).
#[panic_handler]
fn panicked(_: &PanicInfo) -> ! {
    gate::finish(0xDEAD)
}
