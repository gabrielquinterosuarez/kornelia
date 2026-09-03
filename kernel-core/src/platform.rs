//! La frontera de portabilidad.
//!
//! Todo lo que una arquitectura debe proveer vive en este trait. El resto del
//! kernel no sabe sobre qué silicio corre.

use crate::acpi::Hardware;
use crate::channel::Doorbell;
use crate::handlers;
use crate::cores;
use crate::fault::{Fault, Outcome};
use crate::machine::Machine;
use crate::paging::Mapping;
use core::fmt::{self, Write};

/// El reloj que la máquina ya trae andando (deuda 17).
///
/// No se programa nada: el contador existe desde antes que el kernel y se lee
/// con una instrucción. Lo que hace falta saber es **a qué ritmo sube**, y eso
/// lo dice la máquina: en aarch64 hay un registro que lo informa
/// (`CNTFRQ_EL0`); en x86_64 el TSC cuenta ciclos y la frecuencia sale de
/// preguntarle al CPU por su cristal.
#[derive(Clone, Copy)]
pub struct Clock {
    /// Cómo lo llama la máquina, sin traducir (P4).
    pub kind: &'static str,
    /// Cuántas veces sube el contador por segundo.
    pub hz: u64,
}

impl Clock {
    /// Cuántos pasos del contador son esos milisegundos.
    ///
    /// Se multiplica antes de dividir para no perder los milisegundos cortos con
    /// un reloj lento: con `hz` de 62500, dividir primero daría cero.
    pub fn ticks_for_ms(&self, ms: u64) -> u64 {
        self.hz.saturating_mul(ms) / 1000
    }
}

/// Espera a que algo pase, con un plazo **en tiempo** si la máquina tiene reloj
/// y en vueltas si no.
///
/// Existe porque el kernel espera en varios lugares —que un núcleo arranque, que
/// conteste, que suelte lo que está corriendo— y hasta que hubo reloj todos
/// contaban iteraciones. Un tope en vueltas no se puede explicar: las mismas
/// vueltas son milisegundos o minutos según la máquina, y el número no dice
/// cuánto se está dispuesto a esperar.
///
/// Devuelve `true` si la condición se cumplió, `false` si se agotó el plazo.
pub fn wait_until<P: Platform>(p: &mut P, ms: u64, rounds: u64, mut done: impl FnMut() -> bool) -> bool {
    let deadline = p.clock().map(|c| p.ticks().wrapping_add(c.ticks_for_ms(ms)));
    let mut spun = 0u64;
    while !done() {
        match deadline {
            // Con reloj, lo que corta es el tiempo. El tope de vueltas sigue
            // como red por si el reloj no avanza, y por eso tiene que estar muy
            // por encima: una red que se dispara antes que lo que cuida no es
            // una red.
            Some(until) if p.ticks() >= until => return false,
            None if spun >= rounds => return false,
            _ => {}
        }
        spun += 1;
        core::hint::spin_loop();
    }
    true
}

pub trait Platform {
    /// Nombre de la arquitectura. El agente lo recibe en `describe`; el
    /// protocolo nunca lleva nombres de registros horneados (D3).
    const ARCH: &'static str;

    /// Emite un byte por el cordón umbilical (D5: el UART nunca se abandona).
    fn uart_write_byte(&mut self, b: u8);

    /// Levanta un byte del cordón umbilical si hay alguno esperando.
    ///
    /// **No bloquea.** Devuelve `None` si no llegó nada. Es a propósito: el
    /// kernel va a tener que escuchar por el UART y por el transporte que
    /// escriba el agente al mismo tiempo (D17), y un `read` que bloquea deja
    /// sordo al otro canal.
    fn uart_read_byte(&mut self) -> Option<u8>;

    /// Detiene este núcleo para siempre, con el menor consumo posible.
    fn park(&mut self) -> !;

    /// Lo que se averiguó de la máquina durante el arranque.
    ///
    /// Devuelve `'static` a propósito: el mapa se captura en la única ventana
    /// que hay para preguntarle al firmware (D25), y desde ahí es un hecho fijo
    /// de la máquina y no algo atado a esta llamada. El núcleo no sabe si vino
    /// de UEFI, de un device tree o de una ROM de arranque (D24).
    fn machine(&self) -> Machine;

    /// Arma las tablas de páginas del kernel y las carga (D12).
    ///
    /// El **qué** mapear lo decide `paging`, que es portable; el cómo escribir
    /// una tabla es lo menos portable que hay y por eso cruza la frontera.
    ///
    /// # Safety
    ///
    /// Solo se puede llamar después de `ExitBootServices`: cambiar la
    /// traducción con el firmware todavía vivo le saca el piso a sus propias
    /// estructuras.
    unsafe fn install_page_tables(&mut self, m: &Machine) -> Result<Mapping, &'static str>;

    /// Los nombres de los registros de esta máquina, en el mismo orden en que
    /// vienen los valores de un `Fault`.
    ///
    /// El protocolo nunca lleva nombres de registros horneados (D3): x86_64
    /// tiene RAX, aarch64 tiene X0–X30, RISC-V tiene x0–x31. La máquina informa
    /// cuáles tiene (P4).
    const REGISTERS: &'static [&'static str];

    /// Por qué registros pasan los argumentos en esta máquina, como índices
    /// dentro de `REGISTERS`.
    ///
    /// Existe porque hay dos lugares donde el kernel le pasa algo a código que
    /// no escribió: `exec` le pone en el primero su propia dirección, y el
    /// arranque le pone al blob en el segundo la ventanilla del protocolo.
    ///
    /// Son los de la ABI de C **con la que se compila este kernel**, para que el
    /// código del agente pueda ser una función compilada para el mismo target.
    /// Cuáles son no se puede tener horneado (D3) — y no basta con saber la
    /// arquitectura: en x86_64 son RCX y RDX y no RDI y RSI, porque el target es
    /// UEFI y ahí la ABI de C es la de Windows. Se publica en `describe exec`
    /// para que el agente lo lea en vez de suponerlo (P4).
    const ARGUMENTS: &'static [usize];

    /// Extiende el identity map para alcanzar un rango que la maquina no
    /// informo, como dispositivo (no cacheable).
    ///
    /// Existe por P1: el mapa que da el firmware no cubre todo lo que hay, y en
    /// aarch64 los BARs de PCIe caen muy por encima de la region mas alta. Si
    /// el kernel dijera "no llego", seria el kernel la razon por la que no se
    /// puede usar el aparato.
    ///
    /// No se supone que haya RAM ahi: se mapea como dispositivo porque no se
    /// sabe que hay, que es el mismo criterio que usa el arranque (D12).
    ///
    /// # Safety
    ///
    /// Cambia las tablas de paginas vivas. El rango tiene que estar fuera de lo
    /// que ya se mapeo, o se pisa lo que habia.
    unsafe fn map_device(&mut self, start: u64, bytes: u64) -> Result<(), &'static str>;

    /// Instala la captura de excepciones (P5, D7).
    ///
    /// # Safety
    ///
    /// Los handlers tienen que estar mapeados y ejecutables.
    unsafe fn install_fault_handlers(&mut self) -> Result<(), &'static str>;

    /// Provoca un breakpoint a propósito.
    ///
    /// Existe para que el arranque pueda comprobar que la captura funciona en
    /// vez de suponerlo. Un breakpoint es la única excepción pensada para que
    /// se pueda seguir después.
    fn trigger_breakpoint(&mut self);

    /// El último fault capturado, si hubo alguno.
    fn last_fault(&self) -> Option<Fault>;

    /// Lee memoria que la máquina **puede rechazar**, sin morirse si lo hace.
    ///
    /// `mem.read` puede apuntar al registro de un dispositivo, y un dispositivo
    /// rechaza lo que no sabe atender: un registro que solo acepta accesos de
    /// cuatro bytes, leído de a uno, es un acceso inválido. Y las dos
    /// arquitecturas no fallan igual — en x86_64 eso devuelve ceros y sigue, en
    /// aarch64 el bus lo rechaza con un abort externo.
    ///
    /// Ese acceso lo hace el kernel, en el camino del protocolo, donde no hay
    /// un `exec` que lo cubra. Así que se arma el mismo punto de recuperación
    /// que usa `exec`, alrededor de una sola instrucción: el fault vuelve como
    /// dato (P5) en vez de dejar al agente sin cordón por un pedido legítimo,
    /// que es lo que D5 y D17 dicen que no puede pasar.
    ///
    /// `None` si la máquina lo rechazó; el detalle queda en `last_fault`.
    ///
    /// # Safety
    ///
    /// `addr` tiene que estar mapeada y alineada a `width`, que vale 1, 2, 4
    /// u 8. Que el aparato acepte el acceso no hace falta: de eso se trata.
    unsafe fn guarded_read(&mut self, addr: u64, width: u64) -> Option<u64>;

    /// Escribe memoria que la máquina puede rechazar. `false` si la rechazó.
    ///
    /// # Safety
    ///
    /// Lo mismo que `guarded_read`.
    unsafe fn guarded_write(&mut self, addr: u64, width: u64, value: u64) -> bool;

    /// Salta a código máquina y vuelve con lo que haya pasado (P3, P5).
    ///
    /// El agente no es un participante en tiempo de ejecución: es un compilador
    /// que escribe código que corre sin él. Esto es donde ese código corre.
    ///
    /// **Nunca mata al kernel.** Si el código falla, el handler desvía la
    /// ejecución de vuelta acá y el fault se devuelve como dato, no como
    /// muerte. Esa es la razón de ser de todo lo anterior.
    ///
    /// # Safety
    ///
    /// `entry` tiene que apuntar a memoria mapeada y ejecutable. Lo que haya
    /// ahí puede ser cualquier cosa: el kernel no lo mira ni lo valida (P2).
    ///
    /// `region` es el reclamo entero donde vive ese código, y se pasa por dos
    /// motivos. Uno: hay arquitecturas donde la caché de instrucciones **no** es
    /// coherente con la de datos, así que código recién escrito por el camino de
    /// datos no se ve desde el camino de instrucciones hasta que alguien las
    /// sincroniza — en x86_64 el hardware lo hace solo, en aarch64 hay que
    /// pedirlo. Dos: en `supervised` de ahí sale la pila.
    ///
    /// `supervised` es lo que **el agente declaró** (D27), no algo que decida el
    /// kernel. En `false` el código corre con el privilegio del kernel, que es
    /// lo que había antes de D27. En `true` corre en el nivel de abajo —anillo 3
    /// en x86_64, EL0 en aarch64—, y ahí cambian dos cosas:
    ///
    /// - **la pila es el final de `region`**, porque en ese nivel la del kernel
    ///   no se puede ni escribir. Todo lo que corre sin privilegio vive en
    ///   memoria que el agente declaró suya con `mem.claim {user: true}`;
    /// - **para volver no alcanza un retorno común**: hay que ejecutar
    ///   `EXEC_RETURN`. Un retorno común salta a lo que haya quedado en la pila
    ///   y termina en fault — que se captura como cualquier otro (P5).
    /// `initial` es con qué valores arrancan los registros, en el orden de
    /// `REGISTERS` y con `None` en los que el agente no dijo nada. Los que no
    /// pidió quedan en cero, salvo el registro por el que se pasa el primer
    /// argumento: ahí va la dirección de entrada, para que el código pueda
    /// encontrar sus datos sin depender de dónde lo hayan cargado. Si el agente
    /// **sí** puso ese registro, gana el agente: es su código (P2).
    unsafe fn exec(
        &mut self,
        entry: u64,
        region: (u64, u64),
        supervised: bool,
        initial: &[Option<u64>],
    ) -> Outcome;

    /// Qué registros se pueden poner al arrancar un `exec`.
    ///
    /// Es un subconjunto de `REGISTERS` y no todos, porque hay tres que no son
    /// del agente aunque figuren en la lista: dónde empieza a ejecutar lo dice
    /// `off`, la pila la pone el kernel —y lo publica como `stack`—, y el
    /// registro de estado no es un valor que se cargue, es consecuencia de
    /// cómo se entra.
    ///
    /// Se publica en `describe` para que el agente no lo descubra chocándose
    /// (P4), y va acá y no horneado en el protocolo porque los nombres son los
    /// de esta máquina (D3).
    const EXEC_INITIAL: &'static [&'static str];

    /// Las instrucciones con las que el código `supervised` le devuelve el
    /// control al kernel.
    ///
    /// Se publican como **bytes de código máquina** y no como un número de
    /// vector o un nombre de instrucción: así el agente no tiene que saber que
    /// en x86_64 esto es un `int` y en aarch64 un `svc` (D3), y le alcanza con
    /// pegarlos al final de lo que emite (P4). Es la misma forma en que el
    /// kernel publica cómo tocar un timbre.
    const EXEC_RETURN: &'static [u8];

    /// Programa el timbre del cable serie y lo enciende (D5, D17).
    ///
    /// Mientras esto no exista, el kernel tiene que preguntarle al UART byte por
    /// byte, y eso quema un núcleo entero. Devuelve el número de timbre que
    /// quedó asignado, para poder informarlo.
    ///
    /// # Safety
    ///
    /// Las tablas de páginas y la captura de excepciones tienen que estar
    /// puestas: el timbre puede sonar apenas se enciende.
    unsafe fn install_serial_interrupt(&mut self, hw: &Hardware) -> Result<u8, &'static str>;

    /// Programa el timbre del buzón: una interrupción que el **agente** puede
    /// disparar para despertar al núcleo del protocolo (D17).
    ///
    /// Va con prioridad más baja que el cable serie a propósito. Por más que el
    /// agente inunde de llamadas, el cordón umbilical pasa primero — y eso lo
    /// hace cumplir el controlador de interrupciones, no una decisión del
    /// kernel (P6).
    ///
    /// Devuelve las escrituras que el agente tiene que hacer para tocarlo.
    ///
    /// # Safety
    ///
    /// El timbre del cable tiene que estar instalado antes: comparten
    /// controlador.
    unsafe fn install_doorbell(&mut self, hw: &Hardware) -> Result<Doorbell, &'static str>;

    /// Pone el código del agente a atender una interrupción de un aparato (D9).
    ///
    /// `interrupt` es el número con el que **la máquina** identifica esa fuente
    /// —el que publican las tablas de ACPI—, no una ranura de la tabla de
    /// interrupciones: eso último es modelo de x86 y no existe igual en ARM.
    ///
    /// `slot` es la ranura que `handlers` ya reservó, y es lo que la
    /// arquitectura usa para saber a qué handler llamar.
    ///
    /// Con `raw`, el kernel no pone nada alrededor. La variante envuelta **no
    /// restringe nada**: ahorra escribir los mismos veinte bytes de prólogo cada
    /// vez, y no es un guardarraíl.
    ///
    /// # Safety
    ///
    /// `entry` tiene que apuntar a código ejecutable, y con `raw` ese código
    /// tiene que terminar como el hardware espera.
    unsafe fn install_irq(
        &mut self,
        hw: &Hardware,
        interrupt: u32,
        slot: usize,
        raw: bool,
    ) -> Result<Doorbell, handlers::Error>;

    /// Marca un rango como alcanzable —o no— desde el nivel sin privilegio.
    ///
    /// El permiso es una propiedad de **la memoria**, no de la corrida: se pide
    /// al reclamarla y se comprueba al usarla (D27). Ponerlo en cada `exec`
    /// obligaría a tocar las tablas en caliente, y dos reclamos que compartan
    /// bloque se pisarían el permiso sin que nadie se entere.
    ///
    /// El grano es el bloque de `paging::BLOCK`, así que el rango tiene que
    /// estar alineado y ser múltiplo de eso. El que llama se encarga.
    ///
    /// # Safety
    ///
    /// El rango tiene que estar mapeado y no ser memoria del kernel.
    unsafe fn set_user_access(
        &mut self,
        start: u64,
        bytes: u64,
        user: bool,
    ) -> Result<(), &'static str>;

    /// Prende o apaga la atención a los timbres en **este** núcleo.
    ///
    /// Existe para **establecer** el estado en vez de heredarlo del firmware.
    /// El bucle del protocolo corre con los timbres apagados —para que no se
    /// pierda un despertador entre "no hay nada" y "me duermo"— y eso tiene que
    /// ser un hecho puesto, no una suposición sobre lo que dejó UEFI.
    ///
    /// Y se prenden durante un `exec`: en el núcleo del kernel la interrupción
    /// tiene prioridad sobre el código del agente (D29).
    fn set_interrupts(&mut self, on: bool);

    /// Duerme este núcleo hasta que suene algún timbre.
    ///
    /// Es lo que convierte un núcleo quemado en un núcleo reservado: mientras
    /// nadie hable, no consume nada.
    fn sleep(&mut self);

    /// Dónde tiene esta arquitectura los registros del UART, o `None` si no
    /// están en memoria.
    ///
    /// Existe para poder **contrastarlo contra lo que dice la máquina**: hay una
    /// tabla de ACPI (SPCR) que informa dónde está la consola, y el kernel
    /// arranca con esa dirección horneada porque necesita poder hablar antes de
    /// leer ninguna tabla. Comparar las dos convierte una suposición en un dato
    /// comprobado (P4).
    ///
    /// En x86_64 el UART no está en memoria sino en puertos de E/S, que son otro
    /// espacio de direcciones: ahí devuelve `None`.
    fn uart_address(&self) -> Option<u64>;

    /// A qué ritmo sube el contador de la máquina, si la máquina lo dice
    /// (deuda 17).
    ///
    /// Sin esto el kernel puede contar vueltas pero no tiempo, y no son lo
    /// mismo: las mismas vueltas son segundos en un emulador y milisegundos en
    /// silicio. Donde eso importa de verdad es la ventana de rescate del blob
    /// (D18) — si dura milisegundos, no hay rescate.
    ///
    /// No es un driver ni le pide nada al firmware: en las dos arquitecturas es
    /// **una instrucción** —`rdtsc`, `mrs cntpct_el0`— y el contador ya viene
    /// andando desde antes de que el kernel exista.
    ///
    /// `None` cuando la máquina no dice a qué ritmo sube. El contador se puede
    /// leer igual y sirve para comparar dos lecturas, pero no para saber cuánto
    /// pasó: **decir un tiempo que no se sabe es peor que no decirlo** (P4), así
    /// que ahí quien lo necesite vuelve a contar vueltas.
    fn clock(&self) -> Option<Clock>;

    /// Averigua a qué ritmo sube el contador, una sola vez, en el arranque.
    ///
    /// En aarch64 no hace nada: la máquina lo informa en un registro. En x86_64
    /// el TSC cuenta ciclos y el CPU **puede no decir** a qué ritmo, así que hay
    /// que medirlo contra otro reloj de frecuencia conocida — el que informa la
    /// FADT, que sube siempre al mismo ritmo en cualquier máquina.
    ///
    /// Va aparte de `clock` porque medir cuesta tiempo real: calibrar en cada
    /// consulta haría que `describe` tardara milisegundos.
    ///
    /// # Safety
    ///
    /// Solo después de `ExitBootServices`, y una sola vez.
    unsafe fn calibrate_clock(&mut self, hw: &Hardware);

    /// El contador ahora. Sube y no vuelve para atrás; entre dos lecturas, la
    /// diferencia es lo que pasó.
    fn ticks(&self) -> u64;

    /// Se muda al puerto serie que informó la máquina, si puede.
    ///
    /// La dirección con la que el kernel arranca **tiene** que estar horneada:
    /// si el arranque se cuelga antes de leer ninguna tabla, el cable es lo
    /// único que queda para contarlo. Pero apenas la máquina dice dónde tiene
    /// su consola, quedarse con la propia sería preferir una suposición a un
    /// dato (P4) — y es lo único que ata el kernel a una placa concreta.
    ///
    /// Devuelve `false` si esta máquina no tiene a dónde mudarse: en x86_64 el
    /// UART está en puertos de E/S, que no son direcciones de memoria y no es
    /// lo que informa esa tabla.
    ///
    /// # Safety
    ///
    /// `addr` tiene que ser la ventana de registros de un UART de la clase que
    /// esta arquitectura sabe manejar, ya mapeada. Si no lo es, el cordón se
    /// pierde en la primera escritura.
    unsafe fn use_serial_at(&mut self, addr: u64) -> bool;

    /// Si el puerto serie en uso es el que informó la máquina.
    fn serial_from_machine(&self) -> bool;

    /// El identificador del núcleo sobre el que corre el kernel.
    ///
    /// Es el que atiende el protocolo, y por eso es el único que no se puede
    /// reclamar: sería quitarle el piso a quien está contestando el pedido.
    fn this_core(&self) -> u64;

    /// Enciende el IOMMU **antes de que el agente pida nada** (D8).
    ///
    /// Encendido y sin nada declarado, un aparato no llega a ninguna parte. Ese
    /// es el estado que hace que `dma.allow` signifique algo: si el kernel lo
    /// dejara apagado hasta el primer pedido, todo lo que el agente no declaró
    /// estaría permitido, y la declaración no declararía nada.
    ///
    /// No es un guardarraíl del kernel: es el punto de partida contra el que el
    /// agente declara (P6). Encendido, lo que el agente permite lo hace cumplir
    /// el silicio; apagado, permitir sería decorativo.
    ///
    /// # Safety
    ///
    /// Solo después de `ExitBootServices`: el firmware usa DMA para leer el
    /// disco, y apagarle el paso mientras corre le saca el piso.
    unsafe fn enable_iommu(&mut self, hw: &Hardware) -> Result<&'static str, &'static str>;

    /// Si el IOMMU está traduciendo de verdad **en este momento**.
    ///
    /// Se pregunta al silicio, no a una bandera nuestra: la diferencia entre
    /// "lo encendimos" y "está encendido" es justo la que hace que una promesa
    /// sea comprobable (P4). Y el agente la necesita para saber si `dma.allow`
    /// significa algo en esta máquina.
    fn iommu_enabled(&self) -> bool;

    /// Lo que el IOMMU anotó: si hubo algún DMA que no estaba permitido.
    ///
    /// Es la parte de P5 que le toca al silicio. Un acceso negado no se pierde:
    /// queda registrado, y el agente lo puede mirar para saber que su driver
    /// apuntó a donde no debía — en vez de encontrarse memoria distinta sin
    /// explicación, que es lo que pasaba antes de que el IOMMU existiera.
    ///
    /// `None` si esta máquina no tiene con qué contarlo.
    fn dma_faults(&self) -> Option<u64>;

    /// Declara qué memoria puede tocar un dispositivo por su cuenta (D8).
    ///
    /// El IOMMU es una MMU entre el aparato y la RAM. Sin él, un puntero mal
    /// puesto en el registro de una placa no da fault: da memoria distinta, en
    /// silencio y en cualquier parte — el peor error posible para un agente que
    /// está depurando el driver que acaba de escribir.
    ///
    /// No es un guardarraíl: el kernel no decide nada, hace cumplir lo que el
    /// agente **declaró** (P6). Lo que sí hace es no dejarlo abierto por las
    /// dudas — sin nada declarado, un aparato no llega a ninguna parte.
    ///
    /// `device` es el número con el que **el bus** nombra al aparato, que es lo
    /// que el silicio ve llegar en cada pedido de DMA: en PCIe, bus, dispositivo
    /// y función juntos.
    ///
    /// # Safety
    ///
    /// El rango tiene que estar mapeado. Lo que el aparato haga adentro es
    /// asunto del agente (P2).
    unsafe fn set_dma_access(
        &mut self,
        hw: &Hardware,
        device: u32,
        start: u64,
        bytes: u64,
        allow: bool,
    ) -> Result<(), &'static str>;

    /// Despierta a un núcleo que está durmiendo esperando trabajo.
    ///
    /// Es un timbre de núcleo a núcleo: un IPI por el APIC en x86_64, un SGI
    /// por el GIC en aarch64. La misma maquinaria con la que el agente despierta
    /// al núcleo del protocolo, apuntada al revés.
    ///
    /// No dice nada sobre qué hay que hacer — eso ya quedó en el buzón. Solo
    /// saca al núcleo del `hlt` o del `wfi` para que lo mire.
    fn wake_core(&mut self, id: u64);

    /// Le manda a un núcleo la interrupción que **no se puede enmascarar**.
    ///
    /// Es el segundo escalón para cortar código que no vuelve. El timbre normal
    /// alcanza para todo lo que no se tapó los oídos, que es casi todo; esto
    /// alcanza a quien enmascaró las interrupciones comunes — `cli` en x86_64,
    /// `msr daifset, #2` en aarch64.
    ///
    /// Las dos máquinas tienen una línea aparte para esto y no se llama igual:
    /// **NMI** en x86_64, **FIQ** en aarch64. Devuelve `false` donde no la haya,
    /// porque prometer un corte que no va a llegar es peor que decir que no se
    /// puede (P4).
    ///
    /// Sigue habiendo un techo, y es de la arquitectura y no del kernel: quien
    /// enmascare **todo** queda fuera de alcance. D29 dice que en su núcleo eso
    /// lo decide el agente.
    fn stop_core(&mut self, id: u64) -> bool;

    /// Si esta máquina **tiene** esa línea. Se pregunta en vez de intentarlo:
    /// mandar la interrupción a un núcleo inventado para ver qué pasa sería
    /// justo la clase de prueba que rompe algo.
    const CAN_STOP_CORES: bool;

    /// Programa el reloj de **este** núcleo para que avise en ese instante, o lo
    /// desarma con `None`.
    ///
    /// Es lo que convierte el contador en un plazo: leerlo dice cuánto pasó,
    /// pero para cortar código que no vuelve hace falta que además **avise**, y
    /// eso no se puede hacer preguntando — quien tendría que preguntar es
    /// justamente el que está colgado.
    ///
    /// El instante va en pasos del contador, absoluto, no en milisegundos: la
    /// traducción la hace `Clock`, que es portable, y así esto no tiene que
    /// saber a qué ritmo sube.
    ///
    /// # Safety
    ///
    /// La captura de excepciones tiene que estar puesta: si el aviso llega sin
    /// handler, es un fault en el kernel.
    unsafe fn set_deadline(&mut self, at: Option<u64>);

    /// Si el aviso de plazo quedó instalado de verdad.
    ///
    /// No es una propiedad de la arquitectura sino de **esta** máquina: depende
    /// de que el CPU sepa comparar contra el contador y de que el arranque haya
    /// podido instalarlo. Se informa en vez de suponerlo, porque un plazo que
    /// no va a llegar es peor que no ofrecerlo (P4).
    fn deadline_ready(&self) -> bool;

    /// Instala un handler para una interrupcion que el aparato dispara
    /// **escribiendo en memoria** (MSI), en vez de por un cable.
    ///
    /// Los aparatos PCIe de hoy no tienen cable de interrupción: escriben un
    /// dato en una dirección y el silicio lo convierte en interrupción. Para el
    /// agente eso es lo que importa, porque saber qué cable le tocaría a su
    /// aparato requiere interpretar AML — un lenguaje entero adentro de ACPI,
    /// que este kernel no tiene ni va a tener por esto.
    ///
    /// **El número lo elige el kernel**, no el agente: es un recurso de la
    /// máquina y el agente no tiene cómo saber cuál está libre. Devuelve cuál
    /// tocó y, sobre todo, **la escritura que lo dispara** — que es lo que el
    /// agente le va a poner al aparato en su registro de MSI. La misma
    /// información sirve para las dos cosas: configurarlo y hacerlo sonar a
    /// mano para probar el handler antes de que el aparato hable.
    ///
    /// # Safety
    ///
    /// La entrada del handler tiene que estar en un reclamo vigente.
    unsafe fn install_msi(
        &mut self,
        hw: &Hardware,
        slot: usize,
        raw: bool,
    ) -> Result<(u32, Doorbell), handlers::Error>;

    /// Instala el aviso de plazo. Va en el arranque, con los otros timbres.
    ///
    /// # Safety
    ///
    /// Después de que la captura de excepciones y el controlador de
    /// interrupciones estén puestos.
    unsafe fn install_deadline(&mut self) -> Result<(), &'static str>;

    /// Le pide a la máquina que arranque un núcleo, y le dice qué ranura es la
    /// suya para que pueda avisar cuando llegue.
    ///
    /// Devolver `Ok` significa que el pedido se hizo, no que el núcleo ya esté
    /// vivo: eso lo dice `cores::has_arrived`. Son dos CPUs distintas y una no
    /// puede afirmar por la otra.
    ///
    /// # Safety
    ///
    /// Solo después de que las tablas de páginas y la captura de faults estén
    /// puestas: el núcleo nuevo copia esa configuración.
    unsafe fn start_core(
        &mut self,
        hw: &Hardware,
        id: u64,
        slot: usize,
    ) -> Result<(), cores::Error>;
}

/// Escritor de texto sobre el cordón umbilical.
///
/// Es la única concesión a la legibilidad humana en todo el kernel, y existe
/// solo para el arranque y la depuración: el canal del agente es CBOR binario.
pub struct Umbilical<'a, P: Platform> {
    p: &'a mut P,
}

impl<'a, P: Platform> Umbilical<'a, P> {
    pub fn new(p: &'a mut P) -> Self {
        Self { p }
    }

    pub fn line(&mut self, s: &str) {
        let _ = self.write_str(s);
        let _ = self.write_str("\r\n");
    }

    pub fn kv(&mut self, k: &str, v: &str) {
        let _ = write!(self, "{k}: {v}\r\n");
    }

    /// Un tamaño en la unidad binaria más grande que lo represente **exacto**.
    ///
    /// Nunca redondea: un "512 MiB" que en realidad eran 511,9 sería el kernel
    /// mintiendo sobre la máquina, y todo el proyecto depende de que no lo haga
    /// (P4). Si no entra exacto en MiB, sale en KiB.
    pub fn size(&mut self, bytes: u64) {
        const KI: u64 = 1024;
        const MI: u64 = KI * KI;
        const GI: u64 = MI * KI;

        let _ = if bytes >= GI && bytes % GI == 0 {
            write!(self, "{:>6} GiB", bytes / GI)
        } else if bytes >= MI && bytes % MI == 0 {
            write!(self, "{:>6} MiB", bytes / MI)
        } else if bytes % KI == 0 {
            write!(self, "{:>6} KiB", bytes / KI)
        } else {
            write!(self, "{bytes:>6} B  ")
        };
    }
}

impl<'a, P: Platform> Write for Umbilical<'a, P> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.as_bytes() {
            self.p.uart_write_byte(*b);
        }
        Ok(())
    }
}
