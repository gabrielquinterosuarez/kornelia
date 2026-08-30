//! El kernel portable.
//!
//! REGLA (D23): en este crate no puede aparecer `#[cfg(target_arch)]` ni nada
//! específico de una arquitectura. Todo lo que dependa del silicio se pide a
//! través del trait `Platform`. CI falla si esta regla se rompe.

#![no_std]

pub mod platform;

pub use platform::{Cordon, Platform};

/// Punto de entrada del kernel, una vez que la arquitectura terminó de arrancar.
pub fn main<P: Platform>(p: &mut P) -> ! {
    let mut c = Cordon::new(p);

    c.line("");
    c.line("== kernel agente-centrico ==");
    c.kv("arquitectura", P::ARCH);
    c.line("");
    c.line("El cordon umbilical esta vivo.");
    c.line("Sin procesos. Sin archivos. Sin shell. Sin usuarios.");
    c.line("");

    p.park()
}
