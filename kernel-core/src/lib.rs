//! El kernel portable.
//!
//! REGLA (D23/D24): en este crate no puede aparecer `#[cfg(target_arch)]` ni
//! nada específico de una arquitectura, ni tampoco nada específico de cómo
//! arrancó la máquina. Todo lo que dependa del silicio o del firmware se pide a
//! través del trait `Platform`. CI falla si esta regla se rompe.

// `no_std` salvo al testear: el arnés de tests necesita `std` para correr en la
// máquina de desarrollo. El kernel de verdad nunca se compila con `cfg(test)`,
// así que sigue siendo `no_std` en las dos arquitecturas.
#![cfg_attr(not(test), no_std)]

pub mod cbor;
pub mod machine;
pub mod memory;
pub mod platform;
pub mod protocol;
pub mod tables;

#[cfg(test)]
mod tests;

pub use machine::{Machine, Tables};
pub use memory::{Kind, Region};
pub use platform::{Platform, Umbilical};

/// Punto de entrada del kernel, una vez que la arquitectura terminó de arrancar
/// y ya le soltó la máquina al firmware (D25).
pub fn main<P: Platform>(p: &mut P) -> ! {
    // Se pide antes de tomar el cordón: `Umbilical` toma prestado `p` en
    // exclusiva.
    let machine = p.machine();

    saludar(p, &machine);

    // Desde acá manda el protocolo: lo que sale es binario (D6).
    protocol::serve(p, &machine)
}

/// La señal de vida, en texto, antes de que empiece el protocolo.
///
/// Es la única concesión a la legibilidad humana, y existe para que enchufar
/// una terminal alcance para saber si la máquina está viva y qué encontró. El
/// detalle **no** va acá: va por `describe`, que es quien decide cuánto manda
/// según lo que le pidan (D16). Volcar 110 renglones por serie en cada arranque
/// sería el kernel decidiendo por el cliente.
fn saludar<P: Platform>(p: &mut P, m: &Machine) {
    use core::fmt::Write;

    let mut u = Umbilical::new(p);

    u.line("");
    u.line("== kernel agente-centrico ==");
    u.kv("arquitectura", P::ARCH);

    match m.failure {
        // Un arranque que no pudo describir la máquina no es una muerte: es un
        // dato que hay que poder contar (P5).
        Some(reason) => {
            u.line("NO SE PUDO DESCRIBIR LA MAQUINA");
            u.kv("  motivo", reason);
        }
        None => {
            let _ = write!(u, "memoria: {} regiones, ", m.regions.len());
            u.size(m.free_bytes());
            let _ = u.write_str(" libres\r\n");

            let _ = u.write_str("tablas:");
            for (nombre, hay) in [
                ("acpi", m.tables.acpi.is_some()),
                ("device-tree", m.tables.device_tree.is_some()),
                ("smbios", m.tables.smbios.is_some()),
            ] {
                let _ = write!(u, " {nombre}={}", if hay { "si" } else { "no" });
            }
            let _ = u.write_str("\r\n");
        }
    }

    u.line("");
    u.line("Sin procesos. Sin archivos. Sin shell. Sin usuarios.");
    u.line(protocol::MARCA);
}
