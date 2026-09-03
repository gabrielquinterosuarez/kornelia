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
pub mod fdt;
pub mod handlers;
pub mod handles;
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

    // Y a qué ritmo sube el contador de esta máquina (deuda 17). Va acá porque
    // en x86_64 hay que medirlo contra un reloj que informa ACPI, así que
    // necesita la descripción ya leída — y antes de la ventana de rescate del
    // blob, que es quien lo usa.
    // SAFETY: el firmware ya soltó la máquina, y esto se llama una sola vez.
    unsafe { p.calibrate_clock(&hw) };

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

    // Y el aviso de plazo, que es lo que hace cumplir `exec {deadline_ms}`.
    // SAFETY: la captura de excepciones y el controlador ya están puestos.
    let deadline = unsafe { p.install_deadline() };
    report_deadline(p, deadline);

    // El blob, lo último antes de escuchar el cable (D18). Va acá y no antes
    // porque necesita todo lo de arriba: tablas de páginas para poder tocar
    // memoria, captura de faults para que un blob roto sea un dato y no una
    // máquina muerta, y la descripción de la máquina para que lo que haga tenga
    // sentido.
    run_blob(p, &machine, &hw, with_doorbell);

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
        let _ = write!(u, "  serial: the machine puts it at {:#x}, outside the map\r\n", sp.address);
        return;
    }

    {
        let mut u = Umbilical::new(p);
        let _ = write!(u, "  serial: moving to {:#x}, which is where the machine puts it\r\n",
                       sp.address);
    }
    // SAFETY: la dirección la informó la máquina y el identity map la cubre.
    unsafe { p.use_serial_at(sp.address) };
    {
        let mut u = Umbilical::new(p);
        let _ = u.line("  serial: moved. This line comes out of the one the machine reported");
    }
}

/// Cuánto dura la ventana de rescate del blob, en vueltas de espera.
///
/// **No hay reloj todavía**, así que no se puede decir "dos segundos": se cuenta
/// en iteraciones, como la espera de `core.claim`. Queda anotado que la ventana
/// no se puede expresar en tiempo, que es lo que un humano necesita para saber
/// si va a llegar a apretar una tecla.
///
/// Es lo único que queda cuando la máquina **no dice** a qué ritmo sube su
/// contador: ahí la ventana dura lo que duren, y eso se avisa.
const RESCUE_ROUNDS: u64 = 3_000_000;

/// El tope de vueltas cuando **sí** hay reloj, como red por si el reloj no
/// avanza.
///
/// Tiene que estar **muy** por encima de lo que tarda `RESCUE_MS`, o la red se
/// dispara antes que lo que protege y la ventana dura menos de lo prometido. Ya
/// pasó: con el mismo tope que el caso sin reloj, el kernel avisaba dos segundos
/// y cortaba a los mil doscientos milisegundos.
const RESCUE_ROUNDS_WITH_CLOCK: u64 = 200_000_000;

/// Cuánto dura la ventana de rescate cuando hay reloj.
///
/// Dos segundos: alcanzan para que un cliente conectado mande un byte y para que
/// alguien que está mirando la terminal llegue a apretar una tecla, y no son
/// tantos como para que moleste en cada arranque.
const RESCUE_MS: u64 = 2000;

/// Cada cuántas vueltas se mira el cable.
///
/// Preguntarle al UART en **cada** vuelta hacía la ventana inusablemente lenta:
/// leer un puerto es un acceso al aparato, no a memoria, y sesenta millones de
/// esos son minutos. Girar es barato; preguntar no.
const RESCUE_POLL: u64 = 4096;

/// Corre el blob que el arranque trajo del disco (D18, D19, D20).
///
/// # Por qué hay una ventana de rescate
///
/// El blob es código del agente, y el agente puede equivocarse: un blob que se
/// cuelga o que pisa el kernel deja la máquina inútil **en cada arranque**, y
/// arreglarlo requeriría sacar el disco. Así que antes de saltar, el kernel
/// avisa y escucha: cualquier byte por el cable lo cancela.
///
/// No es un guardarraíl (P6) — no juzga el blob ni le pone condiciones. Es la
/// diferencia entre una decisión reversible y una que no.
///
/// # Por qué corre con la maquinaria de `exec`
///
/// Porque es exactamente lo que el agente hubiera subido (D20), y ya existe la
/// red para eso: si falla, el fault vuelve como dato (P5) y el arranque sigue
/// hasta el protocolo. Un blob roto tiene que dejar la máquina **contestando**,
/// que es lo único que permite reemplazarlo.
fn run_blob<P: Platform>(p: &mut P, m: &Machine, hw: &acpi::Hardware, with_doorbell: bool) {
    use core::fmt::Write;

    let bytes = match m.blob {
        machine::Blob::Absent => return,
        machine::Blob::Failed(reason) => {
            let mut u = Umbilical::new(p);
            u.kv("blob", "could not be loaded");
            u.kv("  reason", reason);
            return;
        }
        machine::Blob::Loaded(bytes) => bytes,
    };

    let clock = p.clock();
    {
        let mut u = Umbilical::new(p);
        let _ = write!(u, "blob: {} bytes loaded from blob.bin\r\n", bytes.len());
        match clock {
            Some(_) => {
                let _ = write!(
                    u,
                    "  send any byte within {} ms to NOT run it\r\n",
                    RESCUE_MS
                );
            }
            // Sin reloj no se puede prometer un tiempo, asi que se dice lo que
            // hay: una ventana en vueltas, que dura lo que dure.
            None => u.line("  send any byte to NOT run it (no clock: no deadline)"),
        }
    }

    // La ventana. Con reloj se mide en tiempo; sin reloj, en vueltas — y en ese
    // caso se dice, porque una ventana que no se sabe cuánto dura es lo que hace
    // la diferencia entre poder rescatar la máquina y no (deuda 17).
    let deadline = clock.map(|c| p.ticks().wrapping_add(c.ticks_for_ms(RESCUE_MS)));
    //
    // **Hay que mirar los dos lugares donde puede caer el byte**, y esto costó
    // encontrarlo: en este punto el timbre del cable ya está instalado, así que
    // un byte que llega lo levanta el handler y lo deja en el anillo — al UART
    // no le queda nada, y preguntarle a él daba siempre "no vino nadie". El
    // rescate no se ejecutaba y la ventana parecía andar.
    //
    // Si el byte llegó **antes** de que el timbre estuviera puesto, en cambio,
    // sigue en la cola del UART. Los dos casos son reales, así que se miran los
    // dos.
    let rounds = if clock.is_some() { RESCUE_ROUNDS_WITH_CLOCK } else { RESCUE_ROUNDS };
    for round in 0..rounds {
        if round % RESCUE_POLL == 0 {
            // Con reloj, lo que termina la ventana es el tiempo y no las
            // vueltas: el tope de vueltas queda como red para que un reloj que
            // no avanza no deje el arranque colgado para siempre.
            if let Some(until) = deadline {
                if p.ticks() >= until {
                    break;
                }
            }
            // SAFETY: acá no corre nada más que pueda estar en el anillo.
            let from_ring = if with_doorbell { unsafe { serial::pop() } } else { None };
            if from_ring.is_some() || p.uart_read_byte().is_some() {
                let mut u = Umbilical::new(p);
                u.line("  cancelled: someone is on the other side");
                return;
            }
        }
        core::hint::spin_loop();
    }

    let entry = bytes.as_ptr() as u64;
    let region = (entry, bytes.len() as u64);
    {
        let mut u = Umbilical::new(p);
        let _ = write!(u, "  running at {:#x}\r\n", entry);
    }

    // `raw`: el blob es un cargador de drivers, y un driver toca registros de
    // dispositivo y tablas de páginas. Bajarlo a `supervised` sería el kernel
    // decidiendo con qué privilegio corre el código del agente, que es justo lo
    // que D27 le devuelve al agente.
    //
    // Y con qué le habla al kernel. El blob corre antes de que el protocolo
    // exista, así que si no fuera por esto lo único que tendría es la máquina
    // cruda: alcanza para un cargador (D19), no para algo que quiera reclamar
    // memoria o instalar un handler. Como corre privilegiado y en el mismo
    // espacio de direcciones, la ventanilla es literalmente una función que
    // puede llamar — no hizo falta un verbo nuevo ni un mecanismo nuevo.
    //
    // SAFETY: se cierra apenas el blob vuelve, unas líneas más abajo.
    let gate = unsafe { protocol::open_blob_gate(p, m, hw) };

    // El primer argumento lo pone `exec`: la dirección de entrada. El segundo lo
    // ponemos acá. Cuáles son esos dos registros lo dice la máquina (P4), y lo
    // mismo que se usa acá se publica en `describe exec`.
    let mut initial = [None; 64];
    let second = P::ARGUMENTS.get(1).copied();
    if let Some(i) = second {
        initial[i] = Some(gate);
    }
    let initial = &initial[..P::REGISTERS.len().min(initial.len())];

    // SAFETY: los bytes están en la imagen del kernel, que el identity map
    // cubre. Lo que haya ahí puede ser cualquier cosa — de eso se trata (P2).
    let outcome = unsafe { p.exec(entry, region, false, initial) };

    protocol::close_blob_gate();

    let mut u = Umbilical::new(p);
    if outcome.faulted {
        // Un blob que falla no detiene el arranque: se cuenta y se sigue hasta
        // el protocolo, que es de donde va a salir el reemplazo.
        match outcome.fault {
            Some(f) => {
                let _ = write!(u, "  the blob faulted: {} at {:#x}\r\n", f.cause.code(), f.pc);
            }
            None => u.line("  the blob faulted"),
        }
    } else {
        // Lo que dejó en el primer registro. Es lo único que el kernel puede
        // contar sin saber qué hace el blob, y alcanza para comprobar desde
        // afuera que corrió de verdad.
        let first = outcome.regs.first().copied().unwrap_or(0);
        let _ = write!(u, "  the blob returned, leaving {:#x}\r\n", first);
    }
}

/// Cuenta si se puede declarar un plazo para el código del agente.
///
/// Que no se pueda no es fatal: la máquina anda igual, y un `exec` sin plazo es
/// lo que había siempre. Pero cambia lo que el kernel puede prometer — sin esto,
/// un código que no vuelve en el núcleo del protocolo deja la máquina
/// escuchando sin contestar, y solo se sale reiniciando.
fn report_deadline<P: Platform>(p: &mut P, r: Result<(), &'static str>) {
    let mut u = Umbilical::new(p);
    match r {
        Ok(()) => u.line("deadline: the agent can declare how long its code takes"),
        Err(reason) => {
            u.line("deadline: CANNOT be declared");
            u.kv("  reason", reason);
        }
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
            u.line("  on. with nothing declared, no device reaches memory");
        }
        Err(reason) => {
            u.kv("iommu", "no");
            u.kv("  reason", reason);
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
            let _ = write!(u, "serial: doorbell {v}, the core sleeps between requests\r\n");
            true
        }
        Err(reason) => {
            u.line("serial: NO DOORBELL, still polling byte by byte");
            u.kv("  reason", reason);
            u.line("  this burns a whole core.");
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
    u.line("== agent-centric kernel ==");
    u.kv("architecture", P::ARCH);

    match m.failure {
        // Un arranque que no pudo describir la máquina no es una muerte: es un
        // dato que hay que poder contar (P5).
        Some(reason) => {
            u.line("COULD NOT DESCRIBE THE MACHINE");
            u.kv("  reason", reason);
        }
        None => {
            let _ = write!(u, "memory: {} regions, ", m.regions.len());
            u.size(m.free_bytes());
            let _ = u.write_str(" free\r\n");

            let _ = u.write_str("tables:");
            for (name, hay) in [
                ("acpi", m.tables.acpi.is_some()),
                ("device-tree", m.tables.device_tree.is_some()),
                ("smbios", m.tables.smbios.is_some()),
            ] {
                let _ = write!(u, " {name}={}", if hay { "yes" } else { "no" });
            }
            let _ = u.write_str("\r\n");
        }
    }

    check_stack(&mut u, m);

    u.line("");
    u.line("No processes. No files. No shell. No users.");
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
                "tables: {} GiB identity-mapped ({} cacheable, {} device)\r\n",
                t.gib,
                t.gib - t.device_gib,
                t.device_gib
            );
            // Si el hardware hace cumplir que la memoria del agente no sea
            // ejecutable con privilegio (D27). Sin esto el permiso se puede
            // poner pero no separa nada, y una garantía que no se cumple es
            // peor que no tenerla.
            if t.isolation {
                u.line("  kernel/agent separation: the hardware enforces it");
            } else {
                u.line("  kernel/agent separation: the hardware does NOT enforce it");
                u.line("    the permission is marked but the kernel can still execute there.");
            }
            // Mismo cuidado que con la pila: si la raíz cayera en memoria
            // reclamable, `mem.claim` podría entregársela al agente y la
            // traducción se rompería en cualquier parte.
            if maq.is_ours(t.root, 4096) {
                let _ = write!(u, "  root at {:#x}, in kernel memory\r\n", t.root);
            } else {
                let _ = write!(u, "  root at {:#x} OUTSIDE KERNEL MEMORY\r\n", t.root);
                u.line("  mem.claim cannot be enabled like this.");
            }
        }
        Err(reason) => {
            u.line("tables: COULD NOT BE BUILT, continuing with the firmware ones");
            u.kv("  reason", reason);
            u.line("  mem.claim cannot be enabled like this.");
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
        let _ = write!(u, "stack: {base:#x}, no map to verify it\r\n");
    } else if m.is_ours(base, stack::size()) {
        let _ = write!(u, "stack: {base:#x}, in kernel memory\r\n");
    } else {
        // No es fatal todavía porque nadie puede reclamar memoria: `mem.claim`
        // no existe. Cuando exista, esto sí lo es.
        let _ = write!(u, "stack: {base:#x} OUTSIDE KERNEL MEMORY\r\n");
        u.line("  mem.claim could hand out this memory. Do NOT enable it like this.");
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
        u.line("faults: COULD NOT INSTALL THE HANDLERS");
        u.kv("  reason", reason);
        u.line("  any error will silently reboot the machine.");
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
                "faults: captured. selftest ok (breakpoint at {:#x}, {} registers)\r\n",
                f.pc,
                f.regs.len()
            );
        }
        Some(f) => {
            // Volvió de la excepción, pero mal traducida.
            let _ = write!(u, "faults: the selftest returned '{}' instead of breakpoint\r\n", f.cause.code());
        }
        None => {
            // Volvió del breakpoint sin haber registrado nada: el handler corrió
            // pero no dejó el dato donde tenía que dejarlo.
            u.line("faults: the selftest recorded nothing");
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

    // SAFETY: las direcciones las dio el firmware en su tabla de configuracion,
    // y el identity map de D12 cubre toda la memoria de la máquina.
    let hw = unsafe { tables::describe(&m.tables) };

    // De cuál de los dos dialectos salió lo de abajo. Se dice porque son dos
    // formatos distintos y la diferencia importa para quien depura: el mismo
    // dato faltante significa cosas distintas en cada uno.
    let dialect = if m.tables.acpi.is_some() { "acpi" } else { "device tree" };

    let mut u = Umbilical::new(p);
    if hw.signatures.is_empty() {
        let _ = write!(u, "machine: does not describe itself (neither acpi nor device tree)\r\n");
        return hw;
    }

    let _ = write!(
        u,
        "machine: {} {} nodes, {} cores ({} usable)\r\n",
        dialect,
        hw.signatures.len(),
        hw.cpus.len(),
        hw.usable_cpus()
    );
    if let Some(i) = hw.interrupts {
        let _ = write!(u, "  interrupts: {} at {:#x}", i.kind, i.address);
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
                "  serial: THE MACHINE SAYS {:#x} AND WE USE {:#x}\r\n",
                sp.address, ours
            );
        }
        (Some(sp), Some(_)) => {
            let _ = write!(u, "  serial: {:#x}, the machine says so", sp.address);
            if sp.gsi != 0 {
                let _ = write!(u, ", interrupt {}", sp.gsi);
            }
            let _ = u.write_str("\r\n");
        }
        (Some(sp), None) => {
            let _ = write!(u, "  serial: the machine puts it at {:#x}\r\n", sp.address);
        }
        (None, _) => {}
    }

    if let Some(x) = hw.pcie {
        let _ = write!(
            u,
            "  pcie: config at {:#x}, buses {}-{}\r\n",
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
                "mailbox: doorbell {}, {} write(s) to ring it\r\n",
                d.id, d.count
            );
        }
        Err(reason) => {
            u.line("mailbox: NO DOORBELL OF ITS OWN");
            u.kv("  reason", reason);
            u.line("  a request arriving only there waits for the cable.");
        }
    }
}
