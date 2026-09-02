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

pub mod acpi;
pub mod cbor;
pub mod channel;
pub mod claims;
pub mod cores;
pub mod dma;
pub mod fault;
pub mod handlers;
pub mod machine;
pub mod memory;
pub mod paging;
pub mod platform;
pub mod protocol;
pub mod serial;
pub mod stack;
pub mod tables;
pub mod work;

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

    greet(p, &machine);

    // Después del banner y antes de la marca: si esto colgara, se sabe dónde.
    // SAFETY: el firmware ya soltó la máquina (D25).
    let tables = unsafe { p.install_page_tables(&machine) };
    report_tables(p, tables, &machine);

    // SAFETY: los handlers son parte de la imagen del kernel, que las tablas de
    // arriba acaban de mapear.
    let handlers = unsafe { p.install_fault_handlers() };
    test_faults(p, handlers);

    // Las tablas de ACPI, ya con el identity map puesto: recorrerlas es leer
    // memoria física por todos lados.
    let hw = read_hardware(p, &machine);

    // Y con eso, mudarse al puerto serie que la máquina dijo que tiene. Va acá
    // y no más tarde porque de acá en adelante todo lo que se cuenta sale por
    // el cable: si la mudanza sale mal, conviene que sea con la menor cantidad
    // posible de cosas ya hechas.
    move_to_reported_serial(p, &machine, &hw);

    // El IOMMU, antes que nada del agente: encendido y sin nada declarado,
    // ningún aparato llega a ninguna parte (D8). Es el punto de partida contra
    // el que `dma.allow` significa algo.
    // SAFETY: el firmware ya soltó la máquina, así que su DMA no nos importa.
    let iommu = unsafe { p.enable_iommu(&hw) };
    report_iommu(p, iommu);

    // El timbre del cable, para que el núcleo pueda dormir en vez de girar.
    // SAFETY: las tablas de páginas y la captura de excepciones ya están.
    let doorbell = unsafe { p.install_serial_interrupt(&hw) };
    let with_doorbell = report_doorbell(p, doorbell);

    // Y el timbre del buzón, para que el agente pueda despertar al núcleo sin
    // pasar por el cable. SAFETY: el del cable ya está, y comparten controlador.
    if with_doorbell {
        let bell = unsafe { p.install_doorbell(&hw) };
        report_bell(p, bell);
    }

    // La marca va última: de acá en adelante lo que sale es binario, así que
    // cualquier texto después la convierte en basura para el cliente.
    {
        let mut u = Umbilical::new(p);
        u.line(protocol::MARKER);
    }

    // Desde acá manda el protocolo: lo que sale es binario (D6).
    // El bucle corre con los timbres apagados. Se **establece** acá en vez de
    // heredar lo que dejó el firmware: el diseño del bucle depende de esto y una
    // dependencia heredada es una suposición sin dueño.
    p.set_interrupts(false);

    protocol::serve(p, &machine, &hw, with_doorbell)
}

/// Se muda al puerto serie que informó la máquina (deuda 2).
///
/// El kernel arranca con una dirección horneada porque tiene que poder hablar
/// antes de leer nada. Pero quedarse con ella una vez que la máquina dijo dónde
/// tiene su consola sería preferir una suposición a un dato (P4) — y es lo
/// único que ata este kernel a una placa concreta.
///
/// **El aviso va antes de mudarse, y a propósito.** Si la dirección nueva no
/// fuera un UART, la primera escritura se pierde y no habría con qué contarlo:
/// la última línea que sale por el cable viejo tiene que decir a dónde se fue.
fn move_to_reported_serial<P: Platform>(p: &mut P, m: &Machine, hw: &acpi::Hardware) {
    use core::fmt::Write;

    let Some(sp) = hw.serial else { return };
    // Si esta arquitectura no habla por memoria, no hay a dónde mudarse.
    if p.uart_address().is_none() {
        return;
    }
    if p.uart_address() == Some(sp.address) {
        // Ya estamos ahí. Igual se marca como venida de la máquina: la
        // dirección **es** la que ella informó, y que además coincida con la
        // horneada es una casualidad de esta placa, no un mérito del kernel.
        // SAFETY: es la dirección que ya se está usando.
        unsafe { p.use_serial_at(sp.address) };
        return;
    }

    // Tiene que ser alcanzable. El identity map cubre hasta el tope del mapa;
    // más arriba, escribir ahí no llegaría a ninguna parte.
    let reach = paging::span_gib(m).saturating_mul(paging::GIB);
    if sp.address >= reach {
        let mut u = Umbilical::new(p);
        let _ = write!(u, "  serie: la maquina lo pone en {:#x}, fuera del mapa\r\n", sp.address);
        return;
    }

    {
        let mut u = Umbilical::new(p);
        let _ = write!(u, "  serie: mudandose a {:#x}, que es donde la maquina lo pone\r\n",
                       sp.address);
    }
    // SAFETY: la dirección la informó la máquina y el identity map la cubre.
    unsafe { p.use_serial_at(sp.address) };
    {
        let mut u = Umbilical::new(p);
        let _ = u.line("  serie: mudado. Esta linea sale por el que dijo la maquina");
    }
}

/// Cuenta si la máquina quedó con el IOMMU encendido (D8).
///
/// Que no haya no es fatal: la máquina anda igual. Pero cambia lo que el kernel
/// puede prometer, así que se dice en vez de callarlo — sin IOMMU, un DMA mal
/// apuntado sigue siendo corrupción silenciosa y `dma.allow` no tiene con qué
/// hacerse cumplir (P4).
fn report_iommu<P: Platform>(p: &mut P, r: Result<&'static str, &'static str>) {
    let mut u = Umbilical::new(p);
    match r {
        Ok(kind) => {
            u.kv("iommu", kind);
            u.line("  encendido. sin declarar nada, ningun aparato llega a la memoria");
        }
        Err(reason) => {
            u.kv("iommu", "no");
            u.kv("  motivo", reason);
        }
    }
}

/// Cuenta si el cable serie quedó con timbre (D5, D17).
///
/// Que no se pueda instalar no es fatal: se vuelve a preguntarle al UART byte
/// por byte, que es lo que se hacía hasta ahora. Anda igual, pero quema un
/// núcleo entero — así que se dice.
fn report_doorbell<P: Platform>(p: &mut P, r: Result<u8, &'static str>) -> bool {
    use core::fmt::Write;
    let mut u = Umbilical::new(p);

    match r {
        Ok(v) => {
            let _ = write!(u, "serie: timbre {v}, el nucleo duerme entre pedidos\r\n");
            true
        }
        Err(reason) => {
            u.line("serie: SIN TIMBRE, se sigue preguntando byte por byte");
            u.kv("  motivo", reason);
            u.line("  esto quema un nucleo entero.");
            false
        }
    }
}

/// La señal de vida, en texto, antes de que empiece el protocolo.
///
/// Es la única concesión a la legibilidad humana, y existe para que enchufar
/// una terminal alcance para saber si la máquina está viva y qué encontró. El
/// detalle **no** va acá: va por `describe`, que es quien decide cuánto manda
/// según lo que le pidan (D16). Volcar 110 renglones por serie en cada arranque
/// sería el kernel decidiendo por el cliente.
fn greet<P: Platform>(p: &mut P, m: &Machine) {
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
            for (name, hay) in [
                ("acpi", m.tables.acpi.is_some()),
                ("device-tree", m.tables.device_tree.is_some()),
                ("smbios", m.tables.smbios.is_some()),
            ] {
                let _ = write!(u, " {name}={}", if hay { "si" } else { "no" });
            }
            let _ = u.write_str("\r\n");
        }
    }

    check_stack(&mut u, m);

    u.line("");
    u.line("Sin procesos. Sin archivos. Sin shell. Sin usuarios.");
}

/// Cuenta cómo salió el mapeo (D12).
///
/// Que falle no es fatal hoy: se sigue con las tablas del firmware y el kernel
/// anda. Pero `mem.claim` no se puede habilitar así, porque esas tablas viven
/// en memoria que el mapa informa como libre — y por eso se dice fuerte.
fn report_tables<P: Platform>(
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
            // Si el hardware hace cumplir que la memoria del agente no sea
            // ejecutable con privilegio (D27). Sin esto el permiso se puede
            // poner pero no separa nada, y una garantía que no se cumple es
            // peor que no tenerla.
            if t.isolation {
                u.line("  separacion kernel/agente: la hace cumplir el hardware");
            } else {
                u.line("  separacion kernel/agente: NO la hace cumplir el hardware");
                u.line("    el permiso se marca pero el kernel igual puede ejecutar ahi.");
            }
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
        Err(reason) => {
            u.line("tablas: NO SE PUDIERON ARMAR, se sigue con las del firmware");
            u.kv("  motivo", reason);
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
fn check_stack<P: Platform>(u: &mut Umbilical<'_, P>, m: &Machine) {
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
fn test_faults<P: Platform>(p: &mut P, r: Result<(), &'static str>) {
    use core::fmt::Write;

    if let Err(reason) = r {
        let mut u = Umbilical::new(p);
        u.line("faults: NO SE PUDO INSTALAR LA CAPTURA");
        u.kv("  motivo", reason);
        u.line("  cualquier error va a reiniciar la maquina en silencio.");
        return;
    }

    // Si esto no vuelve, no vuelve nada: es la prueba.
    p.trigger_breakpoint();

    let captured = p.last_fault();
    let mut u = Umbilical::new(p);

    match captured {
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

/// Recorre las tablas de ACPI y cuenta lo que encontró.
///
/// Que no haya ACPI no es un error: una placa embebida se describe con device
/// tree y no tiene ninguna. Se dice y se sigue.
fn read_hardware<P: Platform>(p: &mut P, m: &Machine) -> acpi::Hardware {
    use core::fmt::Write;

    // Se pide antes de tomar el cordón: `Umbilical` toma prestado `p`.
    let p_uart = p.uart_address();

    let hw = match m.tables.acpi {
        None => acpi::Hardware::blank(),
        // SAFETY: el RSDP ya se verificó por firma y checksum, y el identity map
        // de D12 cubre toda la memoria de la máquina.
        Some(addr) => match unsafe { tables::read_acpi(addr) } {
            None => acpi::Hardware::blank(),
            Some(rsdp) => unsafe { acpi::read(&rsdp) },
        },
    };

    let mut u = Umbilical::new(p);
    if hw.signatures.is_empty() {
        u.line("acpi: sin tablas");
        return hw;
    }

    let _ = write!(
        u,
        "acpi: {} tablas, {} nucleos ({} usables)\r\n",
        hw.signatures.len(),
        hw.cpus.len(),
        hw.usable_cpus()
    );
    if let Some(i) = hw.interrupts {
        let _ = write!(u, "  interrupciones: {} en {:#x}", i.kind, i.address);
        if i.version != 0 {
            let _ = write!(u, " v{}", i.version);
        }
        let _ = u.write_str("\r\n");
    }
    // Lo que la maquina dice del puerto serie, contra lo que teniamos horneado.
    match (hw.serial, p_uart) {
        (Some(sp), Some(ours)) if sp.address != ours => {
            let _ = write!(
                u,
                "  serie: LA MAQUINA DICE {:#x} Y USAMOS {:#x}\r\n",
                sp.address, ours
            );
        }
        (Some(sp), Some(_)) => {
            let _ = write!(u, "  serie: {:#x} lo dice la maquina", sp.address);
            if sp.gsi != 0 {
                let _ = write!(u, ", interrupcion {}", sp.gsi);
            }
            let _ = u.write_str("\r\n");
        }
        (Some(sp), None) => {
            let _ = write!(u, "  serie: la maquina lo pone en {:#x}\r\n", sp.address);
        }
        (None, _) => {}
    }

    if let Some(x) = hw.pcie {
        let _ = write!(
            u,
            "  pcie: config en {:#x}, buses {}-{}\r\n",
            x.base, x.bus_start, x.bus_end
        );
    }
    hw
}

/// Cuenta si el buzón quedó con timbre propio (D17).
fn report_bell<P: Platform>(p: &mut P, r: Result<channel::Doorbell, &'static str>) {
    use core::fmt::Write;
    let mut u = Umbilical::new(p);

    match r {
        Ok(d) => {
            channel::set_doorbell(d);
            let _ = write!(
                u,
                "buzon: timbre {}, {} escritura(s) para tocarlo\r\n",
                d.id, d.count
            );
        }
        Err(reason) => {
            u.line("buzon: SIN TIMBRE PROPIO");
            u.kv("  motivo", reason);
            u.line("  un pedido que llegue solo por ahi espera al cable.");
        }
    }
}
