//! El kernel portable.
//!
//! REGLA (D23/D24): en este crate no puede aparecer `#[cfg(target_arch)]` ni
//! nada específico de una arquitectura, ni tampoco nada específico de cómo
//! arrancó la máquina. Todo lo que dependa del silicio o del firmware se pide a
//! través del trait `Platform`. CI falla si esta regla se rompe.

#![no_std]

pub mod memoria;
pub mod platform;

pub use memoria::{Clase, Maquina, Region};
pub use platform::{Cordon, Platform};

/// Punto de entrada del kernel, una vez que la arquitectura terminó de arrancar
/// y ya le soltó la máquina al firmware (D25).
pub fn main<P: Platform>(p: &mut P) -> ! {
    // Se pide antes de tomar el cordón: `Cordon` toma prestado `p` en exclusiva.
    let maquina = p.maquina();

    let mut c = Cordon::new(p);

    c.line("");
    c.line("== kernel agente-centrico ==");
    c.kv("arquitectura", P::ARCH);
    c.line("");

    describir_memoria(&mut c, &maquina);

    c.line("");
    c.line("El cordon umbilical esta vivo.");
    c.line("Sin procesos. Sin archivos. Sin shell. Sin usuarios.");
    c.line("");

    p.park()
}

/// Vuelca el mapa de memoria por el cordón umbilical.
///
/// Esto es un anticipo en texto de lo que va a devolver `describe`. Cuando
/// exista el protocolo CBOR (D6), el agente va a recibir estos mismos datos en
/// binario y esta función queda solo para depurar desde una terminal.
fn describir_memoria<P: Platform>(c: &mut Cordon<'_, P>, m: &Maquina) {
    use core::fmt::Write;

    if let Some(motivo) = m.fallo {
        // Un arranque que no pudo describir la máquina no es una muerte: es un
        // dato que hay que poder contar (P5).
        c.line("mapa de memoria: NO SE PUDO OBTENER");
        c.kv("  motivo", motivo);
        return;
    }

    let _ = write!(c, "mapa de memoria: {} regiones, ", m.regiones.len());
    c.tamano(m.bytes_libres());
    let _ = c.write_str(" libres\r\n");

    for r in m.regiones {
        let _ = write!(c, "  {:#018x}  ", r.inicio);
        c.tamano(r.bytes);
        let _ = write!(c, "  {}", r.clase.nombre());
        // Un tipo que este kernel no conoce se informa con su número crudo en
        // vez de inventarle un significado (P4).
        if let Clase::Otra(n) = r.clase {
            let _ = write!(c, "({n})");
        }
        let _ = c.write_str("\r\n");
    }
}
