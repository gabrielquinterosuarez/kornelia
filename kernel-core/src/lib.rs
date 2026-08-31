//! El kernel portable.
//!
//! REGLA (D23/D24): en este crate no puede aparecer `#[cfg(target_arch)]` ni
//! nada específico de una arquitectura, ni tampoco nada específico de cómo
//! arrancó la máquina. Todo lo que dependa del silicio o del firmware se pide a
//! través del trait `Platform`. CI falla si esta regla se rompe.

#![no_std]

pub mod memory;
pub mod platform;

pub use memory::{Kind, Machine, Region};
pub use platform::{Platform, Umbilical};

/// Punto de entrada del kernel, una vez que la arquitectura terminó de arrancar
/// y ya le soltó la máquina al firmware (D25).
pub fn main<P: Platform>(p: &mut P) -> ! {
    // Se pide antes de tomar el cordón: `Umbilical` toma prestado `p` en
    // exclusiva.
    let machine = p.machine();

    let mut u = Umbilical::new(p);

    u.line("");
    u.line("== kernel agente-centrico ==");
    u.kv("arquitectura", P::ARCH);
    u.line("");

    describe_memory(&mut u, &machine);

    u.line("");
    u.line("El cordon umbilical esta vivo.");
    u.line("Sin procesos. Sin archivos. Sin shell. Sin usuarios.");
    u.line("");

    p.park()
}

/// Vuelca el mapa de memoria por el cordón umbilical.
///
/// Esto es un anticipo en texto de lo que va a devolver `describe`. Cuando
/// exista el protocolo CBOR (D6), el agente va a recibir estos mismos datos en
/// binario y esta función queda solo para depurar desde una terminal.
fn describe_memory<P: Platform>(u: &mut Umbilical<'_, P>, m: &Machine) {
    use core::fmt::Write;

    if let Some(reason) = m.failure {
        // Un arranque que no pudo describir la máquina no es una muerte: es un
        // dato que hay que poder contar (P5).
        u.line("mapa de memoria: NO SE PUDO OBTENER");
        u.kv("  motivo", reason);
        return;
    }

    let _ = write!(u, "mapa de memoria: {} regiones, ", m.regions.len());
    u.size(m.free_bytes());
    let _ = u.write_str(" libres\r\n");

    for r in m.regions {
        let _ = write!(u, "  {:#018x}  ", r.start);
        u.size(r.bytes);
        let _ = write!(u, "  {}", r.kind.name());
        // Un tipo que este kernel no conoce se informa con su número crudo en
        // vez de inventarle un significado (P4).
        if let Kind::Other(n) = r.kind {
            let _ = write!(u, "({n})");
        }
        let _ = u.write_str("\r\n");
    }
}
