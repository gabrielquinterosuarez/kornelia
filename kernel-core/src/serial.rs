//! El buffer entre el timbre del cable y el bucle del protocolo.
//!
//! # Por que hace falta
//!
//! Hasta ahora el bucle del protocolo le preguntaba al UART byte por byte, para
//! siempre. Eso quemaba un nucleo entero.
//!
//! Con el timbre —la interrupcion— el nucleo puede dormir. Pero el que atiende
//! el timbre no es el bucle: es un pedazo de codigo aparte que corre en medio de
//! lo que sea que estuviera pasando, y que tiene que ser cortito. Asi que la
//! division es:
//!
//! - **el que atiende el timbre** vacia la cola del UART y deja los bytes aca
//! - **el bucle** los saca de aca cuando despierta
//!
//! # Por que el que atiende tiene que vaciar la cola
//!
//! Porque el UART **mantiene el timbre sonando mientras haya un byte sin leer**.
//! Si el que atiende solo dijera "ya te oi" y no leyera nada, el timbre volveria
//! a sonar de inmediato, para siempre. La maquina no se cuelga pero no avanza.
//!
//! # Por que no hacen falta atomicos
//!
//! Escribe uno solo (el que atiende) y lee uno solo (el bucle), **en el mismo
//! nucleo**. No hay dos CPUs mirando esto a la vez. Lo unico que hay que cuidar
//! es que el bucle no sea interrumpido a mitad de sacar un byte, y de eso se
//! encarga el bucle corriendo con el timbre apagado salvo justo cuando duerme.

/// Cuantos bytes aguanta sin que el bucle los saque.
///
/// El pedido mas grande son 64 KiB, pero llegan de a poco y el bucle los saca
/// enseguida: lo que tiene que aguantar es una rafaga entre dos despertadas, y
/// la cola del UART en si misma tiene 16 bytes.
const SIZE: usize = 4096;

static mut RING: [u8; SIZE] = [0; SIZE];
/// Donde escribe el que atiende el timbre.
static mut WRITE_AT: usize = 0;
/// Donde lee el bucle.
static mut READ_AT: usize = 0;
/// Cuantos bytes se perdieron por no haber lugar.
///
/// Se cuentan en vez de taparlos: un pedido al que le falta un byte se ve como
/// CBOR malformado, y sin este numero seria un misterio.
static mut DROPPED: u64 = 0;

/// Guarda un byte que acaba de llegar.
///
/// Lo llama el que atiende el timbre.
///
/// # Safety
///
/// Solo desde el nucleo que atiende el cable, y sin que el bucle este a mitad
/// de un `pop`.
pub unsafe fn push(b: u8) {
    let e = WRITE_AT;
    let next_one = (e + 1) % SIZE;
    if next_one == READ_AT {
        // Lleno. Se descarta el que llega y se cuenta.
        DROPPED += 1;
        return;
    }
    (*core::ptr::addr_of_mut!(RING))[e] = b;
    WRITE_AT = next_one;
}

/// Saca el byte mas viejo, si hay.
///
/// # Safety
///
/// Solo desde el bucle, y con el timbre apagado.
pub unsafe fn pop() -> Option<u8> {
    if READ_AT == WRITE_AT {
        return None;
    }
    let b = (*core::ptr::addr_of!(RING))[READ_AT];
    READ_AT = (READ_AT + 1) % SIZE;
    Some(b)
}

/// Cuantos bytes se perdieron por falta de lugar.
pub fn dropped() -> u64 {
    unsafe { DROPPED }
}
