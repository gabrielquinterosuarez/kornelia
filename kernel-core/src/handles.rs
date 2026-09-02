//! Los numeros con los que el agente nombra lo que reclamo (D14).
//!
//! # Por que hay un solo contador
//!
//! Un handle tiene que identificar **una** cosa. Con un contador por clase de
//! reclamo, la memoria y los nucleos empezaban los dos en uno, asi que el
//! handle 1 era a la vez un reclamo de memoria y un nucleo — y `release`, que
//! recibe un handle solo, no tenia forma de saber cual de los dos le pidieron.
//! Soltaba la memoria.
//!
//! Nadie lo habia visto porque hasta ahora los nucleos no se soltaban. Pero era
//! un pedido legitimo que hacia algo distinto de lo que decia.
//!
//! # Por que no se reusan
//!
//! Un handle liberado no vuelve nunca. Un pedido que llega tarde con uno viejo
//! —el agente se desconecto, reconecto y reintento— tiene que **fallar**, no
//! tocar lo que otro reclamo puso despues en el mismo lugar. Es la contracara de
//! que los handles sean de la maquina y no de la conexion (D14).

use core::sync::atomic::{AtomicU64, Ordering};

/// Arranca en uno para que el cero pueda seguir significando "ninguno".
static NEXT: AtomicU64 = AtomicU64::new(1);

/// El proximo handle. Nunca devuelve dos veces el mismo.
pub fn next() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Vuelve a empezar. Solo para los tests.
#[cfg(test)]
pub fn reset() {
    NEXT.store(1, Ordering::Relaxed);
}
