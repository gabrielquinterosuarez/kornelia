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
pub mod fault;
pub mod machine;
pub mod memory;
pub mod paging;
pub mod platform;
pub mod protocol;
pub mod stack;
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

    // Después del banner y antes de la marca: si esto colgara, se sabe dónde.
    // SAFETY: el firmware ya soltó la máquina (D25).
    let tablas = unsafe { p.install_page_tables(&machine) };
    reportar_tablas(p, tablas, &machine);

    // SAFETY: los handlers son parte de la imagen del kernel, que las tablas de
    // arriba acaban de mapear.
    let handlers = unsafe { p.install_fault_handlers() };
    probar_los_faults(p, handlers);

    {
        let mut u = Umbilical::new(p);
        u.line(protocol::MARCA);
    }

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

    verificar_la_pila(&mut u, m);

    u.line("");
    u.line("Sin procesos. Sin archivos. Sin shell. Sin usuarios.");
}

/// Cuenta cómo salió el mapeo (D12).
///
/// Que falle no es fatal hoy: se sigue con las tablas del firmware y el kernel
/// anda. Pero `mem.claim` no se puede habilitar así, porque esas tablas viven
/// en memoria que el mapa informa como libre — y por eso se dice fuerte.
fn reportar_tablas<P: Platform>(
    p: &mut P,
    r: Result<paging::Mapping, &'static str>,
    maq: &Machine,
) {
    use core::fmt::Write;
    let mut u = Umbilical::new(p);

    match r {
        Ok(t) => {
            let _ = write!(
                u,
                "tablas: {} GiB identity-mapeados ({} cacheables, {} de dispositivo)\r\n",
                t.gib,
                t.gib - t.device_gib,
                t.device_gib
            );
            // Mismo cuidado que con la pila: si la raíz cayera en memoria
            // reclamable, `mem.claim` podría entregársela al agente y la
            // traducción se rompería en cualquier parte.
            if maq.is_ours(t.root, 4096) {
                let _ = write!(u, "  raiz en {:#x}, en memoria del kernel\r\n", t.root);
            } else {
                let _ = write!(u, "  raiz en {:#x} FUERA DE LA MEMORIA DEL KERNEL\r\n", t.root);
                u.line("  mem.claim NO se puede habilitar asi.");
            }
        }
        Err(motivo) => {
            u.line("tablas: NO SE PUDIERON ARMAR, se sigue con las del firmware");
            u.kv("  motivo", motivo);
            u.line("  mem.claim NO se puede habilitar asi.");
        }
    }
}

/// Comprueba contra el mapa real que la pila esté en memoria del kernel.
///
/// El razonamiento dice que tiene que estarlo: la pila es un arreglo estático,
/// y por lo tanto vive dentro de la imagen que UEFI cargó como `LoaderData`.
/// Pero el que reparte las clases es el firmware, y esto se comprueba en vez de
/// suponerse — si algún día no se cumple, el síntoma sería que el agente
/// reclama memoria legítimamente libre y le pisa la pila al kernel, que es la
/// clase de falla que aparece lejos de su causa.
fn verificar_la_pila<P: Platform>(u: &mut Umbilical<'_, P>, m: &Machine) {
    use core::fmt::Write;

    let base = stack::base();

    if m.regions.is_empty() {
        // Sin mapa no hay contra qué comprobar. Se dice, en vez de dar por
        // bueno lo que no se miró.
        let _ = write!(u, "pila: {base:#x}, sin mapa para verificarla\r\n");
    } else if m.is_ours(base, stack::size()) {
        let _ = write!(u, "pila: {base:#x}, en memoria del kernel\r\n");
    } else {
        // No es fatal todavía porque nadie puede reclamar memoria: `mem.claim`
        // no existe. Cuando exista, esto sí lo es.
        let _ = write!(u, "pila: {base:#x} FUERA DE LA MEMORIA DEL KERNEL\r\n");
        u.line("  mem.claim podria entregar esta memoria. NO habilitarlo asi.");
    }
}

/// Instala la captura de faults y **comprueba que funcione** (P5, D7).
///
/// No alcanza con instalar la tabla y darla por buena: si los stubs guardaran
/// los registros corridos, o el marco no coincidiera con lo que espera el
/// handler, el síntoma aparecería recién con el primer fault de verdad — que es
/// exactamente el peor momento para descubrirlo, porque ahí ya no hay forma de
/// ver nada.
///
/// Así que el arranque provoca un breakpoint a propósito y comprueba que haya
/// vuelto con la causa correcta. Es el mismo criterio que con `CR3`: se relee
/// en vez de suponer.
fn probar_los_faults<P: Platform>(p: &mut P, r: Result<(), &'static str>) {
    use core::fmt::Write;

    if let Err(motivo) = r {
        let mut u = Umbilical::new(p);
        u.line("faults: NO SE PUDO INSTALAR LA CAPTURA");
        u.kv("  motivo", motivo);
        u.line("  cualquier error va a reiniciar la maquina en silencio.");
        return;
    }

    // Si esto no vuelve, no vuelve nada: es la prueba.
    p.trigger_breakpoint();

    let capturado = p.last_fault();
    let mut u = Umbilical::new(p);

    match capturado {
        Some(f) if f.cause == fault::Cause::Breakpoint => {
            let _ = write!(
                u,
                "faults: capturados. autotest ok (breakpoint en {:#x}, {} registros)\r\n",
                f.pc,
                f.regs.len()
            );
        }
        Some(f) => {
            // Volvió de la excepción, pero mal traducida.
            let _ = write!(u, "faults: el autotest devolvio '{}' en vez de breakpoint\r\n", f.cause.code());
        }
        None => {
            // Volvió del breakpoint sin haber registrado nada: el handler corrió
            // pero no dejó el dato donde tenía que dejarlo.
            u.line("faults: el autotest no registro nada");
        }
    }
}
