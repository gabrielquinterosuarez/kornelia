# Kernel agente-céntrico — Documento de diseño

**Estado:** las dos arquitecturas arrancan por UEFI, le toman la máquina al firmware y
**hablan el protocolo CBOR** por el cordón umbilical. Corren sobre pila y tablas de páginas
propias, capturan los faults como datos, y **los once verbos andan**: el agente reclama
memoria, sube código máquina, lo corre, arranca los otros núcleos y les manda trabajo, y
declara qué puede tocar cada aparato por DMA. El IOMMU está en x86_64; en aarch64 el
kernel todavía no programa el SMMUv3 y lo dice.
**Última actualización:** 2026-08-31

---

## 1. Objetivo

Un kernel experimental, mínimo, que **supone un agente de IA como usuario** y que quita todas
las capas posibles entre ese agente y el hardware. El agente usa el cómputo con una lógica que
no tiene por qué ser la humana, sin las abstracciones ni los guardarraíles heredados de POSIX.

No es un sistema operativo de propósito general. Es un experimento sobre qué queda de un kernel
cuando el operador deja de ser una persona.

---

## 2. Principios

| # | Principio |
|---|---|
| P1 | El kernel nunca es la razón por la que no se puede usar un dispositivo. |
| P2 | Cada capa que se saca no se reemplaza: se deja vacía para que el agente la llene si quiere. |
| P3 | El agente no es un participante en tiempo de ejecución. Es un **compilador**: escribe código que corre sin él. |
| P4 | La máquina se describe a sí misma. El agente no asume nada sobre ella. |
| P5 | Los faults son datos, no muerte. |
| P6 | El hardware hace cumplir lo que **el agente declaró**, no una política del kernel. |

---

## 3. Decisiones tomadas

| # | Decisión | Por qué |
|---|---|---|
| D1 | El agente es **externo hoy**. Cliente, no residente. La puerta a un agente residente queda abierta. | Correrlo adentro implicaría stack de inferencia completo sobre bare metal: un orden de magnitud más de trabajo que todo el resto. Pero el mecanismo del blob-cargador (D18) ya sirve: un modelo residente sería simplemente algo grande que el cargador se trae del disco. No hay que decidirlo ahora. |
| D2 | **Portable entre arquitecturas** desde el diseño. x86_64 primero, ARM64 segundo y temprano. | Una frontera de portabilidad que no se prueba es ficción. A la tercera semana hay x86 filtrado por todos lados. |
| D3 | El protocolo **no contiene nombres específicos de arquitectura**. | x86_64 tiene RAX; ARM64 tiene X0-X30; RISC-V tiene x0-x31. `describe` informa qué registros hay (P4). |
| D4 | **El agente escribe sus propios drivers.** El kernel no tiene ninguno salvo el UART de arranque. | P1 y P2. Un driver es código que el agente sube y ejecuta contra los BARs del dispositivo. |
| D5 | **UART como cordón umbilical**, no como transporte. El agente escribe el transporte rápido y se lo entrega al kernel. | El kernel nunca necesita stack de red. Es el driver mínimo posible: ~150 líneas, sin PCI. |
| D6 | Protocolo **binario (CBOR)**, no JSON. | Se transportan código máquina y volcados de memoria. JSON obligaría a base64 y a escapar. CBOR manda bytes crudos y lo parsea cualquier lenguaje. |
| D7 | **Faults estructurados.** Page fault, división por cero, opcode inválido: el kernel captura, devuelve registros + causa + instrucción, y sigue vivo. | P5. En un OS normal esto es un SIGSEGV. Acá es un valor de retorno. |
| D8 | **IOMMU encendido por defecto, desde el comienzo.** | No es guardarraíl: hace cumplir lo que el agente declaró con `dma.allow` (P6). Y sin él, un DMA mal apuntado es corrupción silenciosa de memoria — el peor bug posible para un agente que depura su propio driver. Es instrumentación tanto como protección. |
| D9 | **Interrupciones: handlers escritos por el agente**, instalados en la tabla de vectores. El kernel no está en el camino. | Una interrupción se atiende en microsegundos; el agente responde en segundos. El agente nunca puede estar en ese loop (P3). |
| D10 | Sin procesos, sin sistema de archivos, sin shell, sin usuarios. | Son las abstracciones que el experimento pone en duda. |
| D11 | **El kernel no deshace, pero cuenta.** El fault devuelve registros + causa + instrucción exacta + log de lo que alcanzó a ejecutarse. Sin rollback ni snapshot. | El rollback verdadero es imposible, no caro: un DMA que ya salió escribió, un registro de GPU ya escrito cambió el aparato. Prometer atomicidad sería mentir, y un agente que confía en una atomicidad falsa decide peor que uno que sabe que no la tiene. Si quiere rollback, se lo construye con `mem.read`/`mem.write` (P2). |
| D12 | **El kernel identity-mapea toda la RAM** (virtual = física, páginas de 1 GiB). El agente que quiera tablas propias las arma y las carga desde su código en `exec`. | El formato de tablas es lo menos portable que hay (x86_64 4-5 niveles, ARM64 TTBR0/1, RISC-V Sv39/48/57): si lo maneja el agente, rompe D3. Y los dispositivos hablan en físicas, así que identity map elimina el segundo sistema de coordenadas al escribir drivers. Aviso: el agente que carga tablas propias y no mapea el kernel y el UART pierde el cordón umbilical. **Enmendado por D27:** cargar tablas propias es una instrucción privilegiada, así que esta puerta abierta solo vale corriendo en modo `raw`. En `supervised` el intento vuelve como fault estructurado en vez de ejecutarse. |
| D13 | **Un solo agente.** La máquina entera es suya. Sin dueños, sin aislamiento entre agentes. | El agente se multiplica solo: si quiere cinco tareas en paralelo, reclama cinco núcleos e instala código distinto en cada uno. No necesita que el kernel sea plural para él serlo (P2). Varios agentes aislados reintroducirían procesos y permisos (contra D10). |
| D14 | **Los handles pertenecen a la máquina, no a la conexión.** Sobreviven a la desconexión del agente y son estables al reconectar. | El estado de la máquina no es una sesión: los núcleos siguen corriendo y los handlers siguen atendiendo mientras el agente no está. Al reconectar, `describe` le devuelve lo que tenía reclamado y lo que pasó. |
| D15 | **El kernel es agnóstico sobre quién está del otro lado.** Sin identidad, sin autenticación, sin sesiones. | "Agente" es una forma de interacción, no una identidad: algo que planifica, emite código y tolera faults estructurados. Si el kernel supiera distinguir clientes, tendría adentro un modelo de "qué es un agente" — el mismo error antropocéntrico al revés. Con UART, quien tiene el cable tiene la máquina (igual que una consola serie o JTAG); cuando el transporte pasa a ser red, la autenticación es problema del transporte que escribió el agente (P2). |
| D16 | **`describe` es consultable, no un volcado fijo.** El cliente pide la profundidad que quiere. | Un agente potente quiere latencias NUMA y estructura del TLB; un cliente simple quiere una lista corta. Volcar todo ahoga al chico, resumir le saca información al grande. El kernel no adivina a quién le habla. |
| D17 | **El UART nunca se abandona.** El transporte que escribe el agente se *agrega*: el kernel escucha por ambos y responde por donde le llegó el pedido. | Un cordón que se puede cortar no era un cordón. Costo casi nulo (un `if` más en el bucle). Si el driver de red del agente se muere a las 3 AM, la máquina sigue contestando por serie — justo cuando más se necesita. La alternativa (que el kernel *juzgue* si el canal nuevo anda) sería una heurística adivinando intención. |
| D18 | **Hay persistencia a través del reinicio: el firmware carga un blob junto al kernel.** El kernel salta a él antes de escuchar el UART, con una ventana de rescate por serie para desactivarlo. | El kernel sigue con cero drivers: el que lee el disco es el firmware UEFI, que ya existe y corre antes que nosotros. Sin esto, un corte de luz deja la máquina muda hasta que alguien llegue por cable — fatal para un nodo remoto. Específico de UEFI: en ARM/RISC-V embebido el equivalente es la ROM de arranque (columna "por arquitectura"). |
| D19 | **El blob es un cargador, no el payload completo.** Unos KB con el driver de NVMe y "leé el resto desde el bloque tal". | Disuelve el tope de 4 GB de FAT32 en el ESP, y el arranque sigue siendo rápido (NVMe lee a GB/s). |
| D20 | **Separación kernel / distribución.** `kernel.efi` (cero drivers, nunca cambia) y `blob.bin` (espacio del agente por defecto: driver de red, driver de NVMe, cargador) — reemplazable, borrable, ignorable. | El blob no es el kernel: es código del agente pre-armado, exactamente lo que hubiera subido él. No contradice D4. Sin esto, lo primero que hace cualquier agente son 15 minutos subiendo un driver de red por serie, cada vez. El blob por defecto trae drivers para una lista conocida (virtio-net, Intel comunes); fuera de la lista, se cae al UART. La lista crece con el tiempo. |
| D21 | **Sin nombre propio por ahora.** La especificación usa los términos técnicos: **kernel** (`kernel.efi`) y **blob** (`blob.bin`). | El nombre es una decisión de marketing y no bloquea nada técnico. Ponerlo ahora obligaría a renombrar todo si cambia. |
| D22 | **Las dos arquitecturas arrancan en verde desde el primer commit.** x86_64 y aarch64, ambas en QEMU. No "ARM más adelante". | Además de portabilidad, es **corrección**: x86 tiene modelo de memoria fuerte y esconde barreras faltantes; ARM reordena. Código con bugs de concurrencia anda perfecto en x86 y se rompe en ARM. Con núcleos aislados, handlers de interrupción y ring buffers compartidos, esa es justo la clase de bug que vamos a tener — y no se puede encontrar probando solo en x86. |
| D23 | **La frontera de portabilidad la verifica el compilador y CI.** El crate portable no lleva una sola línea de `#[cfg(target_arch)]`; habla con el hardware solo por un trait. CI falla si aparece `target_arch` fuera de `arch/`. | Una frontera que no se prueba es ficción. El chequeo mecánico es lo único que de verdad frena la filtración de x86 al resto. |
| D24 | **La frontera se parte en dos ejes, no en uno: arquitectura y entorno de arranque.** El código de UEFI vive en un crate propio (`boot-uefi`) que no lleva `asm!` y lo comparten las dos arquitecturas. Los **tipos normalizados** (región de memoria, dispositivo) viven en `kernel-core`. | `kernel-x86_64` en realidad significaba "x86_64 **+ UEFI**": el `asm!` varía por arquitectura, pero cómo se pide el mapa de memoria varía por entorno de arranque, y el *formato* de ese mapa no varía por ninguno de los dos (UEFI lo estandarizó). Meter UEFI en cada crate de arquitectura lo duplicaría idéntico; meterlo en `kernel-core` dejaría a `check-boundary.sh` dando verde sobre un núcleo casado con UEFI — un falso positivo, peor que un rojo. D18 ya anticipa el segundo eje ("en ARM/RISC-V embebido el equivalente es la ROM de arranque"): con esta partición, un `boot-embedded` entra sin tocar `kernel-core`. **Los tipos normalizados van en `kernel-core` o esto degenera:** si se los queda `boot-uefi`, cada entorno de arranque inventa su propio vocabulario y no hay frontera. |
| D25 | **Al firmware se le pide todo antes de `ExitBootServices`, que se llama una sola vez, en el arranque.** Mapa de memoria, punteros a ACPI / device tree y el blob (D19): todo en esa ventana. | No hay segunda oportunidad: después de salir, llamar a un Boot Service es un crash. Y hay una dependencia que obliga: **lo único que sabe leer FAT32 es el firmware**, así que el blob de D19 —que vive en la partición EFI al lado del kernel— solo se puede cargar antes de salir. Salir apenas arranca haría imposible D18/D19. Además `ExitBootServices` exige la *llave* del mapa más reciente: si algo pide memoria entremedio, la llave queda vieja y la llamada falla. El orden es rígido y conviene que sea un único momento fijo, no un estado que el kernel tenga que rastrear. |
| D29 | **En el núcleo del kernel manda el kernel; en los núcleos del agente manda el agente.** Una interrupción que entra en el núcleo que atiende el protocolo **tiene prioridad**: interrumpe lo que esté corriendo ahí, incluido código del agente, y después se sigue. En un núcleo que el agente reclamó como `dedicated`, la prioridad la decide él — incluido enmascarar todo y no ser molestado. | Es la misma división que ya tenían los modos de `core.claim`, dicha como regla. En el núcleo del protocolo el kernel es quien presta el servicio y quien sostiene el cordón (D5, D17): si el código del agente pudiera quedarse con ese núcleo, el cordón dejaría de estar garantizado. En los núcleos del agente el kernel no tiene nada que decir (P2, P6). **Consecuencia que hay que sostener:** para que "tiene prioridad" sea verdad y no una intención, el agente **no puede poder** enmascarar en ese núcleo — y enmascarar es privilegiado. Así que en el núcleo del kernel el agente corre `supervised` (D27), y si quiere `raw` o quiere que nadie lo moleste, reclama un núcleo dedicado. Esto cierra además la pregunta de si el agente puede usar el núcleo del protocolo: sí, supervisado. |
| D28 | **`listen` es el verbo once, y se agrega en vez de esconderse en un acuerdo implícito.** El agente arma un buzón en memoria que reclamó, y `listen(handle)` se lo entrega al kernel como segundo canal. | D17 promete que el kernel escucha por el transporte que escribe el agente, y **la lista de diez verbos no tenía forma de entregárselo**: era una promesa que la superficie no podía pedir. Había dos salidas. La primera, un acuerdo implícito —una región con forma especial que el kernel mirara sin que nadie se la nombre— mantenía el número diez a costa de esconder complejidad donde nadie la ve: quien lee los verbos no se enteraría de que existe un segundo canal. La segunda es admitir que son once. Se eligió la segunda: **un número redondo no es un principio del proyecto, y la lista de verbos es la documentación real de lo que el kernel hace.** El acuerdo del buzón (firma, versión, dónde va cada índice) lo publica `describe`, así que el agente tampoco lo tiene horneado (P4). Y el kernel sigue sin saber nada de red: recibe un pedazo de memoria con una forma acordada y mira ahí; quien mueve los paquetes es el agente (D4). |
| D27 | **El agente declara con qué privilegio corre su código.** `exec` acepta `supervised` (anillo 3 en x86_64, EL0 en aarch64) o `raw` (anillo 0 / EL1, que es lo que hay hoy). El kernel ofrece los dos y no elige por él. | Hoy el código del agente corre con el mismo privilegio que el kernel, así que una sola instrucción —`cli` en x86, `msr daifset` en ARM— deja la máquina muda sin recuperación posible: es lo único que el agente puede hacer y de lo que el kernel no puede volver. Todo lo demás ya se recupera (un fault vuelve como dato por P5; un bucle infinito se puede desviar con el punto de retorno de `exec`). **El aislamiento no contradice el proyecto: `DESCARTADO.md` ya dice que "importa más con un operador estocástico, no menos".** Lo que sí lo contradiría es que el kernel lo *imponga* — por eso lo declara el agente, y ahí el hardware hace cumplir lo declarado y no una política, que es P6 al pie de la letra. **El precio de `supervised` es acotado y hay que saberlo:** se pierden las tablas de páginas propias (D12), los MSRs, los puertos de E/S de x86 y poder enmascarar interrupciones. Lo que **no** se pierde es la parte central de un driver: los registros de un dispositivo PCIe están mapeados en memoria, y escribirles es una instrucción común que anda igual en anillo 3. Y una violación vuelve como fault estructurado, así que la frontera se documenta sola con maquinaria ya escrita: el agente se entera de qué intentó y puede volver a pedirlo en `raw`. **Complementa D8 en vez de duplicarlo:** el privilegio impide romper el kernel con el CPU, el IOMMU impide romperlo con un dispositivo; un agente sin privilegio que programa mal un DMA sigue pudiendo pisar todo. Costo de implementación conocido: las tablas de páginas dejan de ser cuatro entradas de 1 GiB puestas una vez, porque hay que marcar qué páginas alcanza el agente y a esa granularidad el kernel comparte gigabyte con todo lo demás. **Y hay una restricción del hardware que ata el orden de la implementación:** una página marcada como alcanzable por el agente **deja de ser ejecutable por el kernel**. En x86_64 eso es SMEP, una función que se prende; en aarch64 está metido en el modelo de permisos y no se puede apagar. Las dos dicen lo mismo: o la página es del agente, o el kernel la ejecuta, nunca las dos. La consecuencia práctica es que los bits de permiso **no se pueden poner antes** de que el agente corra sin privilegio — mientras corra con privilegio, marcarlos rompe `exec`. Van juntos o no van. **PRECISIÓN IMPORTANTE — la garantía es más angosta de lo que parece:** cubre el código que el agente corre con `exec`, y **no** el que instala como handler de interrupción con `irq.install`. El hardware no sabe entregar una interrupción a un nivel sin privilegio: en x86_64 la entrada de la IDT exige anillo 0 y en aarch64 la excepción entra en EL1. Así que un handler del agente corre siempre privilegiado y desde ahí puede enmascarar interrupciones. Bajarlos también costaría una transición de privilegio en **cada** interrupción, y D9 dice justamente que el kernel no está en ese camino. La división que queda es defendible y conviene verla como intencional: el **cómputo** es donde el agente genera mucho código, rápido y con errores, y ese va supervisado; los **handlers** son piezas chicas y cuidadas —el driver del blob, veinte líneas que llenan un buffer— escritas una vez. El volumen y el riesgo están en el primero. |
| D26 | **El puerto serie va crudo: sin multiplexor de monitor.** Los scripts usan `-serial stdio`, **nunca** `-serial mon:stdio`. Se sale de QEMU con `Ctrl-C`. | Con `mon:`, QEMU multiplexa su monitor sobre la misma terminal, y ese multiplexor **se come el byte `0x01` (Ctrl-A) como escape junto con el que le sigue**. Por este puerto viaja CBOR y, más adelante, código máquina (D6): ahí `0x01` es un byte tan legítimo como cualquier otro. Se descubrió de la peor manera posible — el primer pedido del cliente empezaba con `83 01 68` y QEMU contestó su pantalla de ayuda, porque leyó `Ctrl-A h`. La comodidad de `Ctrl-A X` no vale un canal que corrompe mensajes en silencio. |

---

## 4. Superficie del kernel

Esto es el kernel entero. **Son once, no diez:** el once (`listen`) se agregó porque D17 prometía
algo que la lista de diez no podía pedir — ver D28.

| Verbo | Qué hace |
|---|---|
| `describe` | Devuelve la máquina real: núcleos, registros disponibles, mapa de memoria física, dispositivos, cachés, NUMA. |
| `mem.claim(bytes, constraints)` | Reclama marcos de memoria física — o un rango MMIO / BAR de un dispositivo. Devuelve un handle. RAM se mapea cacheable; MMIO **no-cacheable** (si no, la CPU cachea las escrituras a registros y el dispositivo nunca se entera). |
| `mem.read(handle, off, len)` | Bytes crudos hacia afuera. |
| `mem.write(handle, off, bytes)` | Bytes crudos hacia adentro. |
| `core.claim(id, modo)` | Un núcleo físico. En `dedicated` es solo del agente, con el timer enmascarado. En `shared` es el núcleo que atiende el protocolo: el kernel le pide prestados microsegundos cuando llega un pedido. El núcleo del protocolo **nunca** se entrega como `dedicated`, y el kernel lo dice con los datos para que el agente decida (P4). |
| `exec(core, handle, off, regs, mode)` | Salta a código máquina. Devuelve estado de registros + fault si lo hubo. `mode` es `supervised` o `raw` y **lo declara el agente** (D27): no tiene valor por omisión, porque elegirlo sería el kernel eligiendo. |
| `irq.install(interrupt, handle, off)` | Instala un handler. El kernel pone prólogo, epílogo y EOI. El argumento es el número con el que **la máquina** identifica la fuente, no una ranura de tabla: eso último es modelo de x86 y no existe igual en ARM (D3). |
| `irq.install_raw(interrupt, handle, off)` | Igual, pero el agente hace todo. Sin red de contención. **En aarch64 devuelve error**: el GIC entrega el número y el reparto es en software, así que no hay un camino más crudo que el que ya se usa — decirlo es mejor que aceptar el pedido y dar otra cosa (P4). |
| `dma.allow(device, handle)` | Declara qué memoria puede tocar un dispositivo. Programa el IOMMU. |
| `listen(handle)` | Adopta como **segundo canal** un buzón que armó el agente en memoria reclamada (D5, D17, D28). El kernel escucha por ahí *además* del cable, y contesta por donde le llegó el pedido. |
| `release(handle)` | Devuelve lo reclamado. |

**Nota sobre `irq.install`:** la variante no-`raw` no restringe nada. Ahorra escribir los mismos
veinte bytes de prólogo cada vez. No es un guardarraíl.

**Patrón previsto:** el handler escribe en un ring buffer que el agente lee cuando vuelve. La
máquina junta eventos sola durante horas; el agente aparece después y lee la historia.

---

## 5. Lo que el kernel NO tiene

Explícitamente fuera de alcance, por diseño:

- Stack de red (lo escribe el agente — D5)
- Sistema de archivos
- Drivers de dispositivo (salvo el UART de arranque)
- Procesos, hilos, planificador de tiempo compartido
- Shell, terminal, consola de texto
- Usuarios, permisos, cuentas
- Memoria virtual como ilusión de espacio privado
- Compatibilidad POSIX

---

## 6. Frontera de portabilidad

| Portable | Específico por arquitectura |
|---|---|
| El protocolo y los verbos | Arranque (UEFI / device tree / otro) |
| El modelo de handles | Tablas de páginas y MMU |
| La contabilidad de recursos | Controlador de interrupciones |
| El formato de los faults | Estado y nombres de registros |
| | MMIO y operaciones atómicas |
| | Descubrimiento de dispositivos (PCIe vs device tree) |

En x86_64 y servidores ARM hay PCIe. En chips embebidos no: hay device tree y periféricos
mapeados en memoria. `describe` tiene que cubrir los dos modelos de descubrimiento.

---

## 7. Estado del código

**Cuidado al leer este documento:** las secciones 4 y 6 son *especificación*, no descripción.
Los **once** verbos de la sección 4 están implementados. El último, `dma.allow`, anda entero
en x86_64 y en aarch64 informa que todavía no se programa (deuda 14).

Lo que sí existe:

| Pieza | Estado |
|---|---|
| Arranque UEFI en x86_64 y aarch64 | Andando. ~20 KB por kernel. |
| Cordón umbilical (UART) — **entrada y salida** | 16550 por puertos de E/S en x86, PL011 por MMIO en ARM. La lectura no bloquea (D17: hay que poder escuchar dos canales). |
| `ExitBootServices` (D25) | Andando. El kernel toma la máquina en el arranque, con reintento si el mapa se movió. |
| **Mapa de memoria físico real** | Andando en las dos arquitecturas. Se captura de UEFI y se normaliza al vocabulario de `kernel-core` (D24). |
| El trait `Platform` | Cinco miembros: `ARCH`, `uart_write_byte`, `uart_read_byte`, `park`, `machine`. |
| `scripts/check.sh` | El portón: frontera + 15 tests + compila las dos + **las bootea en QEMU** y verifica lo que dicen. Probado que falla cuando debe. |
| CI (`.github/workflows/ci.yml`) | Llama al mismo portón, para que no haya chequeos que solo existan en una de las dos partes. |
| **El protocolo CBOR** (D6) | Andando. Escrito a mano, sin dependencias; verificado contra los vectores canónicos del RFC 8949. |
| **`describe`** | Andando: sirve `memory`, `tables`, `claims`, `cpus`, `interrupts` y `pcie`. Sin argumentos devuelve el índice, no un volcado (D16). |
| **Lectura de ACPI** | Andando en las dos. MADT (núcleos y controlador de interrupciones) y MCFG (PCIe), con el checksum verificado tabla por tabla. |
| **Permiso de memoria** (D27) | Andando en las dos. `mem.claim {user: true}` entrega memoria alcanzable sin privilegio, y **lo hace cumplir el hardware**: SMEP en x86_64, el modelo de permisos en aarch64. |
| **Transición de privilegio** (D27) | Andando en las dos. `exec {mode}` entra a anillo 3 / EL0 y vuelve por una ventanilla —`int 0x80` con `DPL=3`, `svc #0`— cuyos bytes publica `describe`. La pila sale del final del reclamo del agente. **Comprobado por lo que el hardware niega:** apagar las interrupciones desde `supervised` vuelve como fault en vez de dejar la máquina muda. |
| **`mem.claim` · `mem.read` · `mem.write` · `release`** | Andando. Reclamos por tamaño o por dirección exacta (así se pide MMIO), con alineación y tope. Los handles son de la máquina y no se reusan (D14). |
| **`irq.install`** | Andando en las dos. El agente pone su código a atender un aparato, y el kernel publica además **cómo hacer sonar esa interrupción a propósito** para que pueda probar su handler sin esperar al aparato. `irq.install_raw` solo en x86_64. |
| **Timbre del buzón** | Andando en las dos. El agente lo toca con código máquina propio: un IPI por el APIC en x86_64, un SGI por el GIC en aarch64. **Con prioridad más baja que el cable**, así que por más que el agente inunde de llamadas el cordón pasa primero (D17, P6). El kernel cuenta cuántas veces sonó, que es lo que permite comprobarlo. |
| **`listen`** | Andando en las dos. El kernel escucha por el cable y por el buzón, y contesta por donde le llegó (D17). El acuerdo lo publica `describe`. |
| **`core.claim`** | Andando en las dos. PSCI en aarch64; INIT/SIPI más un trampolín de 16→32→64 bits en x86_64. El núcleo nuevo copia las tablas de páginas y la captura de faults, y avisa por un atómico. |
| **Trabajo en un núcleo reclamado** | Andando en las dos. `exec {core}` deja el pedido en un buzón por núcleo y el núcleo **duerme** hasta que lo despierta un IPI/SGI. Comprobado con código del agente que informa en qué núcleo corre. Sincrónico y con tope (deuda 13). |
| **`exec`** | Andando en las dos. **El fault vuelve como respuesta, no como muerte** (P5): el handler desvía el regreso al punto de recuperación en vez de detener el núcleo. El agente corre en pila propia y las excepciones en otra, así que ni destruyendo el puntero de pila se lleva la máquina. |
| **Tablas de páginas propias** (D12) | Andando en las dos. Identity map con páginas de 1 GiB; MMIO no cacheable. La raíz se relee del registro y se verifica contra el mapa. |
| **Timbre del cable serie** | Andando en las dos. El núcleo duerme entre pedidos en vez de preguntarle al UART byte por byte. APIC + IO-APIC en x86_64, GIC en aarch64. Es la misma maquinaria que va a necesitar `irq.install`. |
| **Captura de faults** (P5, D7) | Andando en las dos. Causa + crudo + dirección + registros. Autotest de breakpoint en cada arranque. Todavía no viaja por CBOR ni vuelve al agente. |
| **`dma.allow`** (D8) | Andando en x86_64 con VT-d: tablas de traducción por dispositivo, grano de 4 KiB, y el IOMMU **encendido desde el arranque** — sin declarar nada, ningún aparato llega a ninguna parte. En aarch64 la máquina informa su SMMUv3 y el kernel todavía no lo programa; lo dice en vez de callarlo (deuda 14). |

Verificado el 2026-08-30 contra dos fuentes independientes: el mapa que imprime el kernel en
aarch64 coincide con el device tree que genera QEMU (`memory@40000000` → primera región en esa
dirección; `pl031@9010000` → esa página reportada como MMIO), y el total de RAM libre coincide
con lo que se le pidió a QEMU en las dos arquitecturas.

### Deudas anotadas

1. **~~La pila del kernel vive en memoria reclamable.~~ RESUELTO.** El kernel se muda a una
   pila propia (`kernel-core/src/stack.rs`) apenas deja de necesitar al firmware. Al ser un
   arreglo estático vive dentro de la imagen, que UEFI cargó como `LoaderData` y el mapa
   informa como `Kind::Kernel` — una clase que no se entrega nunca. No se da por sentado: el
   arranque comprueba contra el mapa real que la pila haya caído ahí, y lo dice por el cordón.

   **Pero el problema de fondo no está cerrado:** la pila era *una* de las cosas nuestras que
   vivían en memoria que `ExitBootServices` convirtió en libre. Sigue estando la siguiente.
2. **~~La dirección del PL011 sigue horneada.~~ COMPROBADA.** Sigue escrita a mano porque el kernel necesita poder hablar antes de leer ninguna tabla, pero ya no se da por buena: el arranque la contrasta contra la tabla SPCR de ACPI —que es la máquina diciendo dónde tiene su consola— y avisa si no coinciden. En QEMU `virt` coincide: `0x9000000`, interrupción 33. Lo que falta para cerrarla del todo es *usar* la que dice la tabla en vez de la propia, que solo importa en una placa donde no coincidan.

   **Nota vieja:** *La dirección del PL011 estaba horneada* en `kernel-aarch64/src/uart.rs` (`0x0900_0000`, la
   placa `virt` de QEMU). La fuente legítima es el device tree —o la tabla SPCR de ACPI—, que
   todavía no leemos. Mientras siga así, el cordón umbilical solo funciona en esa placa.
3. **~~Las tablas de ACPI se encuentran pero no se leen.~~ RESUELTO.** Se recorre el XSDT
   verificando el checksum de cada tabla, y de ahí salen los núcleos (MADT) y dónde se
   configura PCIe (MCFG). `describe` gana las secciones `cpus`, `interrupts` y `pcie`. Las
   tablas que este kernel no interpreta se informan igual por su firma: que exista algo que no
   sabemos leer es más útil que callarlo (P4).

   Lo que falta encima: el device tree sigue sin leerse, así que una placa embebida —que no
   tiene ACPI— no reporta nada de esto. Y de la MADT solo se sacan núcleos y el controlador;
   las rutas de interrupción (`irq.install` las va a necesitar) todavía no.
4. **~~Seguimos sobre las tablas de páginas del firmware.~~ RESUELTO (D12).** El kernel arma
   las suyas y las carga: identity map con páginas de 1 GiB, tablas en arreglos estáticos —
   o sea dentro de la imagen, en memoria `Kind::Kernel`. El registro raíz (`CR3` / `TTBR0_EL1`)
   se **relee** para confirmar que el cambio ocurrió, y se comprueba contra el mapa que la raíz
   haya caído en memoria del kernel.

   Con esto quedan cerradas las dos cosas nuestras que vivían en memoria reclamable, que era
   lo que bloqueaba `mem.claim`.

5. **~~Una pila rota durante `exec` mata la máquina.~~ RESUELTO (P5).** El código del agente
   corre en **su propia pila**, y las excepciones entran en **otra**: en x86_64 con GDT y TSS
   propios más la IST; en aarch64 con `SP_EL0` para el agente y `SP_EL1` para el kernel, que la
   arquitectura ya trae bancados. Verificado con código máquina que destruye el puntero de pila
   y después falla: vuelve como fault capturado en las dos. Y comprobado al revés — sacándole
   la IST a x86_64, el mismo programa reinicia la máquina.

6. **`exec` no recibe un estado inicial de registros**, aunque la sección 4 lo especifica. El
   código recibe en el primer registro de argumento su propia dirección, y nada más. Y no se
   puede elegir núcleo, porque `core.claim` no existe.

7. **~~Al reportar un fault, dos núcleos que fallan a la vez entrelazan la salida.~~ RESUELTO.**
   El reporte toma un candado. No se corrompía nada —el estado del fault ya era por núcleo—,
   pero se perdía lo único que ese texto sirve: poder leerlo. Dos reportes intercalados byte a
   byte son dos reportes ilegibles, justo en el momento en que no hay otra forma de mirar.

   El candado es de girar y no se suelta si el que lo tiene se cuelga, a propósito: si un núcleo
   se colgó adentro del reporte de un fault, que el otro no escriba encima es lo que más ayuda.

8. **~~Los handlers del agente corren diferidos.~~ RESUELTO (D9, D29).** Durante un `exec` las
   interrupciones quedan abiertas, así que un handler entra al instante y el `exec` sigue
   después. El bucle del protocolo sí corre cerrado —ahí el hueco entre "no hay nada" y "me
   duermo" es real— pero eso son dos instrucciones, no todo el trabajo del agente.

   De paso se cerró algo que estaba peor de lo que parecía: **el estado de la máscara no se
   establecía, se heredaba del firmware.** El diseño del bucle dependía de que estuviera
   cerrada y nadie lo había puesto; en aarch64 funcionaba porque UEFI la dejaba así. Ahora se
   establece explícitamente.

9. **~~Un núcleo reclamado todavía no puede recibir trabajo.~~ RESUELTO.** `exec` acepta `core`
   —el handle que devolvió `core.claim`— y el trabajo va a un buzón por núcleo: el del protocolo
   deja el pedido, el reclamado lo levanta, corre y contesta. Un escritor y un lector por ranura,
   así que no hay candado; lo que hay es orden de memoria dicho explícitamente, que es lo que
   aarch64 exige y x86_64 perdona.

   **El núcleo reclamado duerme entre trabajos** y lo despierta un timbre de núcleo a núcleo (un
   IPI por el APIC, un SGI por el GIC). Girar esperando habría sido quemar un núcleo entero — lo
   mismo que el proyecto ya le sacó al núcleo del protocolo cuando el cable tuvo timbre.

   La prueba no es lo que el kernel dice: **el propio código del agente informa en qué núcleo
   está corriendo** —`cpuid` en x86_64, `mpidr_el1` en aarch64— y el número tiene que ser
   distinto del núcleo que atiende y coincidir con el que se pidió.

   **Lo que apareció al hacerlo, y es el ejemplo más limpio de por qué D22 pide las dos
   arquitecturas:** un núcleo arrancado por PSCI viene con los registros SIMD **atrapados**
   (`CPACR_EL1` en cero), porque ese es su valor de reset. El núcleo de arranque no lo sufría
   porque UEFI se los había habilitado. Y como el compilador usa registros anchos para copiar
   structs, la primera copia de una respuesta era una excepción — que el handler de faults
   volvía a provocar al copiar la suya, así que el núcleo entraba en un bucle de faults **sin
   alcanzar a avisar por el cordón**. En x86_64 no pasa nada de esto. Es exactamente la clase de
   diferencia entre núcleos que no se ve hasta que el segundo hace algo que el primero hacía
   gratis.

   **Queda abierto que `exec` en otro núcleo es sincrónico:** el del protocolo espera la
   respuesta, con tope. Un trabajo más largo que el tope vuelve como `core did not answer` aunque
   el núcleo esté sano — ver deuda 13.

10. **Un test falló una vez y no reprodujo.** Ocurrió una sola vez en la suite de `kernel-core` y
   no se repitió en veinte corridas seguidas. Se auditó lo único que puede causarlo —los tests
   que tocan las tablas globales de reclamos y de núcleos— y todos toman el mismo candado. **No
   está diagnosticado**; queda anotado para no darlo por inexistente si vuelve a pasar.

13. **Un `exec` en otro núcleo es sincrónico, y eso le pone techo a lo que el agente puede
   correr ahí.** El núcleo del protocolo deja el trabajo y **espera**, con un tope de vueltas para
   que un núcleo que no contesta no se lleve puesto el cordón umbilical (D5, D17). El precio es
   que un trabajo legítimamente largo se informa igual que uno perdido, y el núcleo queda marcado
   como fallado sin serlo.

   Lo que falta es la forma asincrónica: `exec` devuelve enseguida un handle de trabajo y el
   agente pregunta después si terminó. Es lo que un núcleo `dedicated` pide de verdad —correr
   algo durante horas mientras el agente no está—, y encaja con D14: el resultado sería otro
   estado de la máquina que sobrevive a la desconexión.

14. **En aarch64 el SMMUv3 se informa pero no se programa.** La IORT dice dónde está y
   `describe` lo publica, pero `enable_iommu` devuelve error y `dma.allow` también. Es a
   propósito: aceptar el pedido y no hacer nada dejaría al agente escribiendo drivers contra
   una garantía que no existe, y el síntoma aparecería lejos de la causa (P4). Mientras tanto,
   en esa arquitectura un DMA mal apuntado sigue siendo corrupción silenciosa.

   Lo que falta es la máquina de estados del SMMUv3: tabla de streams, descriptores de
   contexto y cola de comandos. Es más trabajo que VT-d, no más difícil.

15. **Un BAR que asignó el firmware puede no estar en el mapa de memoria.** `mem.claim` por
   dirección exacta lo rechaza con `unmapped`, así que el agente no puede leer ni escribir los
   registros de ese aparato con `mem.read`/`mem.write` — solo desde su propio código en `exec`,
   porque el identity map cubre el bloque de 1 GiB entero aunque el mapa no liste el rango.
   Apareció escribiendo la prueba del IOMMU. No bloquea nada, pero es una asimetría que un
   agente va a encontrar y hoy no está explicada por ningún lado.

11. **Los atributos de cacheabilidad que informa UEFI se descartan.** D12 anda igual porque la
   cacheabilidad se deduce de la *clase* de cada región, pero UEFI informa además atributos por
   región (`UC`, `WC`, `WT`, `WB`) que son más precisos que esa deducción. Mientras el grano del
   mapeo sea 1 GiB casi no cambia nada; cuando haya que mapear MMIO fino con `mem.claim`, sí.

12. **~~Falta la transición de privilegio de D27.~~ RESUELTO.** `exec` toma `mode`, que **declara
    el agente**: `supervised` entra a anillo 3 en x86_64 y a EL0 en aarch64; `raw` corre con el
    privilegio del kernel. **No hay valor por omisión** — si falta, el pedido se rechaza. Un
    default sería el kernel eligiendo, que es exactamente lo que D27 le devuelve al agente (P6).

    La prueba no es lo que el kernel dice sino lo que el hardware hace: el programa que apaga las
    interrupciones —`cli` en x86_64, `msr daifset` en aarch64, la única cosa de la que el kernel
    **no podía volver**— ahora vuelve como fault estructurado (`protection` e `invalid-opcode`
    respectivamente). Está en el portón, en las dos arquitecturas.

    **Tres cosas que la mecánica de abajo no cubría y aparecieron al implementarla:**

    - **La pila.** El código del agente corría sobre un arreglo estático del kernel, que desde
      anillo 3 no se puede ni escribir: el primer `push` faultea. Se resolvió con que la pila de
      `supervised` sea **el final del mismo reclamo** — todo lo que corre sin privilegio vive en
      memoria que el agente declaró suya. Lo publica `describe` como `stack: claim-end`.
    - **Cómo vuelve.** Desde el nivel de abajo un `ret` no vuelve, así que hay una ventanilla:
      `int 0x80` con `DPL=3` en x86_64, `svc #0` en aarch64. El agente no la tiene horneada —
      `describe {what:["exec"]}` publica **los bytes de código máquina** que tiene que emitir,
      igual que publica cómo tocar un timbre (P4, D3). En x86_64 entra por `TSS.RSP0` y no por la
      IST, a propósito: así esa pila se ejercita en cada `exec supervised` en vez de ser un campo
      del TSS que nadie mira.
    - **La restricción de D29 quedó pendiente, atada a la deuda 9.** Abajo dice que en el núcleo
      del protocolo solo se admite `supervised` y que no es opcional. Aplicarla hoy haría que
      `raw` **no exista**: `exec` corre siempre en el núcleo del protocolo, así que el kernel
      ofrecería dos modos con uno inalcanzable, y eso no es lo que dice D27. Va junto con que
      `exec` pueda elegir núcleo. Lo que **sí** se hace cumplir son las otras dos comprobaciones:
      `supervised` exige memoria con `user`, y `raw` exige que no lo sea.

    Y de paso se corrigió algo que estaba mal desde antes: en aarch64 `REGISTERS` tenía `pc` en la
    posición 31, pero el camino de retorno de `exec` guardaba ahí el **puntero de pila** — el
    kernel informando un registro con el nombre de otro, que es justo lo que P4 no permite. Ahora
    `sp` existe como tal, y los faults también lo informan.

    **La mecánica, como estaba escrita antes de implementarla:**

    **x86_64.** La GDT necesita dos descriptores más, de anillo 3: código (tipo `0xFA` en vez de
    `0x9A`) y datos (`0xF2` en vez de `0x92`). Hoy la tabla es `0=nulo, 1=código0, 2=datos0,
    3+=TSS` con dos entradas por TSS, así que los nuevos van en 3 y 4 y **la base de los TSS se
    corre a 5** — hay que mover `selector_tss`. Y el `TSS.RSP0`, que hoy está en cero sin usar,
    tiene que apuntar a la pila del kernel: es donde el CPU aterriza cuando llega un trap desde
    anillo 3.

    La entrada se arma a mano: apilar `SS`(datos3), el puntero de pila del agente, `RFLAGS`
    **con el bit de interrupciones prendido** (D29), `CS`(código3) y la dirección de entrada, y
    hacer `iretq`. La vuelta es una compuerta de la IDT con `DPL=3` —así el agente la puede
    invocar con `int`— cuyo stub corre ya en anillo 0 sobre `TSS.RSP0`.

    **Cuidado con el desvío de faults:** hoy el handler solo reescribe `RIP`. Viniendo de anillo
    3 hay que reescribir también `CS`, `SS` y el puntero de pila, o el `iretq` vuelve a anillo 3
    a una dirección del kernel y falla de nuevo.

    **aarch64.** Más corto: `eret` con `SPSR_EL1.M = 0b0000` (EL0t), `ELR_EL1` en la entrada,
    `SP_EL0` en la pila del agente y el bit `I` limpio. La vuelta es `svc` desde EL0, que entra
    por el offset `0x400` de la tabla de vectores —donde hoy está `vec_common`— y se distingue
    de un fault real por `EC == 0x15`. Un IRQ desde EL0 entra por `0x480`, que ya apunta a
    `vec_irq`: eso no hay que tocarlo.

    **Cuidado:** el desvío de faults hoy hace `m.spsr |= 1` para volver a `SP_EL1`. Viniendo de
    EL0 hay que poner el campo `M` entero en `0b0101` (EL1h), no prender un bit.

    **Y en las dos:** conviene **un solo punto de aterrizaje** con una bandera en el bloque por
    núcleo que distinga "volvió" de "falló", en vez de dos puntos — el ensamblador queda mucho
    más corto. Agregar ese campo corre los offsets del bloque, que el ensamblador usa a mano;
    los `offset_of!` avisan al compilar.

    **Dos comprobaciones que el verbo tiene que hacer**, y que no son opcionales:
    `exec supervised` exige memoria reclamada con `user: true` y `exec raw` exige que **no** lo
    sea —la misma página no puede ser las dos cosas—, y en el núcleo del protocolo solo se
    admite `supervised` (D29).

4b. **~~No hay manejo de excepciones.~~ RESUELTO (P5, D7).** IDT en x86_64, tabla de vectores en
   aarch64. Un fault devuelve causa normalizada, el número crudo que usó la máquina, la
   dirección tocada y los registros con **sus** nombres (D3). El arranque provoca un breakpoint
   a propósito y comprueba que vuelva bien, en vez de suponerlo.

   Y con `exec` quedó cerrado el círculo: **durante un `exec`, un fault no detiene nada** — el
   handler desvía el regreso al punto de recuperación y el fault vuelve al agente por CBOR.
   Fuera de un `exec`, un fault sigue siendo un bug del kernel y detiene el núcleo, que es lo
   correcto: ahí no hay a quién devolvérselo.

---

## 8. Preguntas abiertas

1. **Dónde se publica el código.** Hay repositorio git local desde el Hito 1 (rama `main`).
   El alojamiento remoto sigue sin definir: repo aparte, no en empujoneducativo.
2. **Por dónde seguir.** **Los once verbos andan.** Lo que queda es emparejar las dos
   arquitecturas: el SMMUv3 de aarch64 (deuda 14) es lo único que un agente puede pedir y
   recibir en una y no en la otra.

   Y quedó destrabada la restricción de D29 que la deuda 12 dejó pendiente: ahora que `exec`
   puede elegir núcleo, exigir `supervised` en el del protocolo ya **no** deja `raw` sin lugar
   donde correr. Hacerla cumplir es un cambio chico en el verbo y uno grande en las pruebas: casi
   todas corren `raw` en el núcleo que atiende, y pasarían a necesitar un núcleo reclamado.

3. **Si `exec` debe recibir un estado inicial de registros.** La sección 4 lo especifica
   (`exec(core, handle, off, regs)`) y hoy no lo hace: el código recibe solo su propia
   dirección. Sumarlo es fácil; la pregunta es qué nombres se aceptan, y ahí manda D3 — tendrían
   que ser los que informa `describe`, no una lista horneada.

4. **Leer el device tree.** Hoy se sabe encontrarlo pero no se lee, así que una placa embebida
   —que no tiene ACPI— no reporta ni núcleos ni buses. Es también lo que haría falta para sacar
   la dirección del PL011 de su fuente legítima en vez de tenerla horneada (deuda 2).

3. **~~Qué del System Table cruza la frontera.~~ CERRADA por D24.** El mapa de memoria *normalizado* es portable;
   cómo se obtiene (UEFI vs device tree vs ROM de arranque) no lo es. Se decide con `describe`.

---

## 9. Entorno de desarrollo

Esta sección describe **requisitos**, no una máquina: el proyecto ya se mudó una vez y la lista
de "lo que está instalado acá" se pudrió sola.

| Necesario para | Qué | Cómo |
|---|---|---|
| Compilar ambas | Rust estable + targets `x86_64-unknown-uefi` y `aarch64-unknown-uefi` | `rustup` (no el Rust de Debian: hacen falta `rustup target add`). `rust-toolchain.toml` los declara. |
| Enlazar | Nada externo: el `rust-lld` que viene con la toolchain alcanza. | — |
| Correr x86_64 | `qemu-system-x86_64` + OVMF | `apt install qemu-system-x86 ovmf` |
| Correr aarch64 | `qemu-system-aarch64` + AAVMF | `apt install qemu-system-arm qemu-efi-aarch64` |
| Depurar ARM desde x86 | `gdb-multiarch` | `apt install gdb-multiarch` |

Los scripts leen las rutas del firmware de las variables `OVMF_CODE`/`OVMF_VARS` y
`AAVMF_CODE`/`AAVMF_VARS`, así que una distribución que las ubique en otro lado se acomoda sin
tocar el código.

### Sobre hardware real

**Decidido: la máquina de desarrollo no se bootea.** Todo el trabajo va en QEMU. Cuando llegue
el momento de probar en silicio, se hace en un equipo aparte, dedicado.

El motivo es que bootear el kernel no es un `cargo run`: es pendrive UEFI y reinicio, con la
máquina entera fuera de servicio mientras dura la prueba — y el kernel no tiene forma de
devolverte el control salvo apagando. Convertir la máquina de trabajo en el banco de pruebas
sería pagar ese costo en cada iteración.

Para tener presente cuando llegue ese momento:

- **NVIDIA está fuera de alcance** (firmware firmado desde Turing — ver `DESCARTADO.md`). Una
  iGPU Intel o una AMD sí son terreno viable.
- **D8 necesita un IOMMU real** (VT-d en Intel, AMD-Vi). QEMU puede emular uno, pero la
  diferencia entre el emulado y el de silicio es exactamente donde viven los bugs interesantes.
- El equipo dedicado tiene que arrancar por **UEFI** (D18 y D20 dependen de que el firmware
  cargue el blob) y conviene que tenga **salida serie accesible** — sin cordón umbilical no hay
  forma de ver qué pasó.
