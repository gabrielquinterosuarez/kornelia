# Kernel agente-céntrico — Documento de diseño

**Estado:** las dos arquitecturas arrancan por UEFI, le toman la máquina al firmware y
**hablan el protocolo CBOR** por el cordón umbilical. Corren sobre pila y tablas de páginas
propias, capturan los faults como datos, y **los once verbos andan**: el agente reclama
memoria, sube código máquina, lo corre, arranca los otros núcleos y les manda trabajo, y
declara qué puede tocar cada aparato por DMA. El IOMMU está **en las dos**: VT-d en
x86_64, SMMUv3 en aarch64.
**Última actualización:** 2026-09-01

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
| D19 | **El blob es un cargador, no el payload completo.** Unos KB con el driver de NVMe y "leé el resto desde el bloque tal". | Disuelve el tope de 4 GB de FAT32 en el ESP, y el arranque sigue siendo rápido (NVMe lee a GB/s). **Andando de punta a punta, del lado del agente.** El driver de NVMe existe y usa **sólo los once verbos**: encuentra el controlador por su clase (`01.08.02`, no por fabricante), reclama su ventana, lo resetea, le arma las colas, declara el DMA, y lee. El formato en disco es el más chico posible: una cabecera en el bloque 0 con firma, versión, tamaño, punto de entrada y una suma, y el payload a continuación. Sin sistema de archivos, y no es carencia — un cargador que entiende FAT32 es mucho más grande que uno que lee bloques por número, y el payload lo escribe el mismo que escribe el cargador. Se comprueba con la única prueba que vale: se pone código de verdad en el disco, el cargador lo trae **por DMA directo a memoria del agente**, verifica la suma, salta, y el código deja `0xc0ffee`. Lo que falta es mudarlo de Python a código máquina dentro del blob: la lógica ya está probada y lo único que cambia es quién consigue los recursos. |
| D20 | **Separación kernel / distribución.** `kernel.efi` (cero drivers, nunca cambia) y `blob.bin` (espacio del agente por defecto: driver de red, driver de NVMe, cargador) — reemplazable, borrable, ignorable. | El blob no es el kernel: es código del agente pre-armado, exactamente lo que hubiera subido él. No contradice D4. Sin esto, lo primero que hace cualquier agente son 15 minutos subiendo un driver de red por serie, cada vez. El blob por defecto trae drivers para una lista conocida (virtio-net, Intel comunes); fuera de la lista, se cae al UART. La lista crece con el tiempo. |
| D21 | **Sin nombre propio por ahora.** La especificación usa los términos técnicos: **kernel** (`kernel.efi`) y **blob** (`blob.bin`). | El nombre es una decisión de marketing y no bloquea nada técnico. Ponerlo ahora obligaría a renombrar todo si cambia. |
| D22 | **Las dos arquitecturas arrancan en verde desde el primer commit.** x86_64 y aarch64, ambas en QEMU. No "ARM más adelante". | Además de portabilidad, es **corrección**: x86 tiene modelo de memoria fuerte y esconde barreras faltantes; ARM reordena. Código con bugs de concurrencia anda perfecto en x86 y se rompe en ARM. Con núcleos aislados, handlers de interrupción y ring buffers compartidos, esa es justo la clase de bug que vamos a tener — y no se puede encontrar probando solo en x86. |
| D23 | **La frontera de portabilidad la verifica el compilador y CI.** El crate portable no lleva una sola línea de `#[cfg(target_arch)]`; habla con el hardware solo por un trait. CI falla si aparece `target_arch` fuera de `arch/`. | Una frontera que no se prueba es ficción. El chequeo mecánico es lo único que de verdad frena la filtración de x86 al resto. |
| D24 | **La frontera se parte en dos ejes, no en uno: arquitectura y entorno de arranque.** El código de UEFI vive en un crate propio (`boot-uefi`) que no lleva `asm!` y lo comparten las dos arquitecturas. Los **tipos normalizados** (región de memoria, dispositivo) viven en `kernel-core`. | `kernel-x86_64` en realidad significaba "x86_64 **+ UEFI**": el `asm!` varía por arquitectura, pero cómo se pide el mapa de memoria varía por entorno de arranque, y el *formato* de ese mapa no varía por ninguno de los dos (UEFI lo estandarizó). Meter UEFI en cada crate de arquitectura lo duplicaría idéntico; meterlo en `kernel-core` dejaría a `check-boundary.sh` dando verde sobre un núcleo casado con UEFI — un falso positivo, peor que un rojo. D18 ya anticipa el segundo eje ("en ARM/RISC-V embebido el equivalente es la ROM de arranque"): con esta partición, un `boot-embedded` entra sin tocar `kernel-core`. **Los tipos normalizados van en `kernel-core` o esto degenera:** si se los queda `boot-uefi`, cada entorno de arranque inventa su propio vocabulario y no hay frontera. |
| D25 | **Al firmware se le pide todo antes de `ExitBootServices`, que se llama una sola vez, en el arranque.** Mapa de memoria, punteros a ACPI / device tree y el blob (D19): todo en esa ventana. | No hay segunda oportunidad: después de salir, llamar a un Boot Service es un crash. Y hay una dependencia que obliga: **lo único que sabe leer FAT32 es el firmware**, así que el blob de D19 —que vive en la partición EFI al lado del kernel— solo se puede cargar antes de salir. Salir apenas arranca haría imposible D18/D19. Además `ExitBootServices` exige la *llave* del mapa más reciente: si algo pide memoria entremedio, la llave queda vieja y la llamada falla. El orden es rígido y conviene que sea un único momento fijo, no un estado que el kernel tenga que rastrear. |
| D29 | **En el núcleo del kernel manda el kernel; en los núcleos del agente manda el agente.** Una interrupción que entra en el núcleo que atiende el protocolo **tiene prioridad**: interrumpe lo que esté corriendo ahí, incluido código del agente, y después se sigue. En un núcleo que el agente reclamó como `dedicated`, la prioridad la decide él — incluido enmascarar todo y no ser molestado. | Es la misma división que ya tenían los modos de `core.claim`, dicha como regla. En el núcleo del protocolo el kernel es quien presta el servicio y quien sostiene el cordón (D5, D17): si el código del agente pudiera quedarse con ese núcleo, el cordón dejaría de estar garantizado. En los núcleos del agente el kernel no tiene nada que decir (P2, P6). **Consecuencia que hay que sostener:** para que "tiene prioridad" sea verdad y no una intención, el agente **no puede poder** enmascarar en ese núcleo — y enmascarar es privilegiado. Así que en el núcleo del kernel el agente corre `supervised` (D27), y si quiere `raw` o quiere que nadie lo moleste, reclama un núcleo dedicado. Esto cierra además la pregunta de si el agente puede usar el núcleo del protocolo: sí, supervisado. **Y desde ahora incluye al blob**, que era la única excepción: corría `raw` justo en ese núcleo, así que podía enmascarar las interrupciones y dejar el cordón sordo para siempre — sin recuperación posible, ni reiniciando, porque en cada arranque volvía a correr. Se consideró ponerle un plazo y descartado: un plazo por omisión es el kernel opinando sobre cuánto puede tardar el código del agente, justo lo que D27 evita, y corta un cargador legítimo que tarda. También se consideró mandarlo a otro núcleo, y descartado porque no hay a dónde en una máquina de un solo núcleo. Corriendo `supervised` el problema **desaparece en vez de gestionarse**: el silicio no lo deja enmascarar, así que el timbre entra siempre. Lo que lo hizo posible es que un cargador **no necesita privilegio** — pide memoria, declara DMA y toca registros por el protocolo, que es como se escribió el driver de NVMe entero. |
| D30 | **El cable no se puede cerrar.** El agente no tiene forma de decirle al kernel "de acá en más solo escucho el canal autenticado". La autenticación, si hace falta, vive en el transporte que escribe el agente (D5) o en dónde está enchufado el cable — no en el kernel, que sigue siendo agnóstico sobre quién habla (D15). | Se planteó al revés: lo único que daría seguridad de verdad sería poder cerrarlo. Y se descartó porque **el cordón es lo que permite recuperar la máquina** (P1, D5): cerrarlo cambia seguridad por la posibilidad de perderla para siempre. Es la misma tensión que ya se resolvió a favor del rescate en la ventana de 2 segundos del blob (D18) y al bajarlo a `supervised` (D29). Ojo con el razonamiento fácil de que "hace falta acceso físico": no siempre — en un servidor con BMC la consola serie sale por red, y con `SOCKET=` sale por un socket. Lo que protege es dónde está enchufado el cable, y eso es despliegue, no kernel. |
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
| `mem.read(handle, off, len, width)` | Bytes crudos hacia afuera. `width` (1, 2, 4 u 8) es de a cuánto se toca la memoria: un registro de dispositivo puede aceptar solo su ancho exacto. Por omisión, uno. |
| `mem.write(handle, off, bytes, width)` | Bytes crudos hacia adentro, con el mismo `width`. |
| `core.claim(id, modo)` | Un núcleo físico. En `dedicated` es solo del agente, con el timer enmascarado. En `shared` es el núcleo que atiende el protocolo: el kernel le pide prestados microsegundos cuando llega un pedido. El núcleo del protocolo **nunca** se entrega como `dedicated`, y el kernel lo dice con los datos para que el agente decida (P4). |
| `exec(core, handle, off, regs, mode, wait, deadline_ms)` | Salta a código máquina. Devuelve estado de registros + fault si lo hubo. `regs` es con qué valores arranca, nombrados como los nombra **esta** máquina (D3); cuáles se pueden poner lo publica `describe`. `mode` es `supervised` o `raw` y **lo declara el agente** (D27): no tiene valor por omisión, porque elegirlo sería el kernel eligiendo. En el núcleo del protocolo solo se admite `supervised` (D29). Con `core`, `wait:false` contesta enseguida y el resultado queda en `describe {what:["cores"]}`. `deadline_ms` es cuánto puede tardar antes de que se lo corte, y **también lo declara el agente**: sin plazo, un código que no vuelve no vuelve. |
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
Los **once** verbos de la sección 4 están implementados, y desde que se programó el SMMUv3
(deuda 14) andan **enteros en las dos arquitecturas**.

Lo que sí existe:

| Pieza | Estado |
|---|---|
| Arranque UEFI en x86_64 y aarch64 | Andando. ~20 KB por kernel. |
| Cordón umbilical (UART) — **entrada y salida** | 16550 por puertos de E/S en x86, PL011 por MMIO en ARM. La lectura no bloquea (D17: hay que poder escuchar dos canales). |
| `ExitBootServices` (D25) | Andando. El kernel toma la máquina en el arranque, con reintento si el mapa se movió. |
| **Mapa de memoria físico real** | Andando en las dos arquitecturas. Se captura de UEFI y se normaliza al vocabulario de `kernel-core` (D24), incluida la **cacheabilidad que informa el firmware** región por región, que el mapeo prefiere a deducirla de la clase. |
| El trait `Platform` | Cinco miembros: `ARCH`, `uart_write_byte`, `uart_read_byte`, `park`, `machine`. |
| `scripts/check.sh` | El portón: frontera + idioma + 102 tests + compila las dos + **las bootea en QEMU** y les habla el protocolo, más una corrida extra de aarch64 **sin ACPI** para ejercitar el device tree. Probado que falla cuando debe. |
| CI (`.github/workflows/ci.yml`) | Llama al mismo portón, para que no haya chequeos que solo existan en una de las dos partes. |
| **El protocolo CBOR** (D6) | Andando. Escrito a mano, sin dependencias; verificado contra los vectores canónicos del RFC 8949. |
| **`describe`** | Andando: sirve `memory`, `tables`, `claims`, `cpus`, `interrupts` y `pcie`. Sin argumentos devuelve el índice, no un volcado (D16). |
| **Lectura de ACPI** | Andando en las dos. MADT (núcleos y controlador de interrupciones) y MCFG (PCIe), con el checksum verificado tabla por tabla. |
| **El reloj de la máquina** | Andando en las dos. `CNTPCT_EL0` en aarch64, donde la máquina informa la frecuencia; `TSC` en x86_64, donde puede no informarla y entonces **se mide** contra el contador de frecuencia fija de la FADT. Se publica en `describe {what:["clock"]}`, y si la máquina no dice el ritmo se dice eso en vez de inventar un número (P4). |
| **El blob de arranque** (D18, D19, D20) | Andando en las dos. El firmware trae `blob.bin` de la misma partición de la que salió el kernel —**lo único que sabe leer FAT32 es él** (D25), así que se carga dentro de la ventana y antes de pedir el mapa— y el kernel lo corre con la maquinaria de `exec`: si falla, el fault vuelve como dato y el arranque sigue hasta el protocolo. Antes de saltar avisa por el cable y espera: **cualquier byte lo cancela**, que es lo que hace que un blob roto no deje la máquina inútil en cada arranque. **Y el blob le puede pedir cosas al kernel**: recibe en el segundo registro de argumento la dirección de una función que atiende **un** pedido y deja la respuesta en un buffer suyo. No es un mecanismo nuevo ni un verbo nuevo — es el mismo `dispatch` de los once, con un origen más (D17: el kernel contesta por donde le llegó el pedido). No hizo falta una ventanilla del estilo de `supervised` porque el blob corre privilegiado y en el mismo espacio de direcciones: llamar a una función del kernel es una instrucción. Cuáles son esos dos registros lo publica `describe {what:["exec"]}` en `arguments`, y son los de la ABI de C del target, para que el blob pueda ser una función compilada para la misma máquina. Se comprueba mirando los reclamos después del arranque: tiene que estar el que pidió el blob y que nunca pasó por el cable — y con el blob cancelado, no.

**Del blob está el mecanismo, no el contenido.** No hay un `blob.bin` en el repo: el único blob que existe es el de prueba que genera `client.py`, que le pide memoria al kernel para demostrar que la ventanilla anda. **Los dos drivers que nombran D19 y D20 sí existen** —NVMe y red, los dos escritos con los once verbos y del lado del agente, en `client.py`— pero todavía viven en Python del lado del cliente, no compilados adentro de un blob. Lo que falta es mudarlos: la lógica ya está probada de punta a punta y lo único que cambia es quién consigue los recursos. |
| **El transporte rápido** (D5, D17, D28) | **Andando en las dos, y con esto D5 deja de ser una promesa.** El agente tiene driver de red —Intel 82540EM, escrito con los once verbos y nada más— y encima de él un transporte: un bucle de **código máquina propio** que corre en un núcleo que reclamó (D29: ahí manda él) y mueve bytes entre la placa y el buzón que entregó con `listen`. El kernel contesta por el buzón sin enterarse de que del otro lado hay una red (D4). La prueba es la que no admite interpretación: **el pedido no sale por el cable** — sale de un socket del host, cruza la red, y la respuesta vuelve por el mismo camino; el cable sólo arma todo y pregunta después, que es exactamente lo que D17 exige que siga andando. **Y aparece un hecho del diseño que no estaba dicho: el buzón es un flujo de bytes, no una cola de mensajes.** El kernel sabe dónde termina un pedido porque escanea el CBOR a medida que entra; del lado del agente hacer lo mismo sería un decodificador de CBOR en código máquina. Así que el transporte **no interpreta nada** — es un caño, y los mensajes los arman las dos puntas. Un pedido o una respuesta pueden venir partidos en varios datagramas. |
| **Lectura del device tree** | Andando. El otro dialecto en el que una máquina se describe, para las placas que no traen ACPI: núcleos, controlador de interrupciones con su versión, puerto serie con su interrupción, PCIe e IOMMU. Cuál usar no lo elige el kernel — es cuál dejó el firmware. Se comprueba con un blob armado a mano en los tests y booteando con `acpi=off`, donde el portón exige que ande el IOMMU contra un aparato de verdad. |
| **Permiso de memoria** (D27) | Andando en las dos. `mem.claim {user: true}` entrega memoria alcanzable sin privilegio, y **lo hace cumplir el hardware**: SMEP en x86_64, el modelo de permisos en aarch64. |
| **Transición de privilegio** (D27) | Andando en las dos. `exec {mode}` entra a anillo 3 / EL0 y vuelve por una ventanilla —`int 0x80` con `DPL=3`, `svc #0`— cuyos bytes publica `describe`. La pila sale del final del reclamo del agente. **Comprobado por lo que el hardware niega:** apagar las interrupciones desde `supervised` vuelve como fault en vez de dejar la máquina muda. |
| **Dónde vale cada privilegio** (D29) | Andando en las dos. En el núcleo del protocolo `exec` **solo** admite `supervised`: ahí manda el kernel, y para que eso sea verdad el agente no puede *poder* enmascarar. `raw` exige un núcleo reclamado, donde la prioridad la decide él. El acuerdo se publica (`describe {what:["exec"]}` trae `this_core`) en vez de dejar que se descubra chocándose (P4). |
| **`mem.claim` · `mem.read` · `mem.write` · `release`** | Andando. Reclamos por tamaño o por dirección exacta (así se pide MMIO), con alineación y tope. Los handles son de la máquina y no se reusan (D14). Un rango que cae en un **hueco** del mapa se entrega con la clase `unreported` —ahí viven los BARs que el firmware no listó— y `width` permite tocarlo con el ancho que el aparato exige. **Un acceso que la máquina rechaza vuelve como respuesta**, no como muerte: los dos verbos van con el mismo punto de recuperación que usa `exec` (P5). |
| **Interrupciones por escritura (MSI)** | Andando en las dos. `irq.install {msi:true}` reserva una interrupción **sin rutear ningún cable** y publica la escritura que la dispara — que es lo que el agente le pone al aparato en su registro de MSI, y lo mismo con que puede hacerla sonar a mano para probar el handler antes de que el aparato hable. **El número lo elige el kernel**: es un recurso de la máquina y el agente no tiene cómo saber cuál está libre. Comprobado con un aparato PCIe de verdad que interrumpe al terminar un DMA. |
| **`irq.install`** | Andando en las dos. El agente pone su código a atender un aparato, y el kernel publica además **cómo hacer sonar esa interrupción a propósito** para que pueda probar su handler sin esperar al aparato. `irq.install_raw` solo en x86_64. |
| **Timbre del buzón** | Andando en las dos. El agente lo toca con código máquina propio: un IPI por el APIC en x86_64, un SGI por el GIC en aarch64. **Con prioridad más baja que el cable**, así que por más que el agente inunde de llamadas el cordón pasa primero (D17, P6). El kernel cuenta cuántas veces sonó, que es lo que permite comprobarlo. |
| **`listen`** | Andando en las dos. El kernel escucha por el cable y por el buzón, y contesta por donde le llegó (D17). El acuerdo lo publica `describe`. |
| **`core.claim`** | Andando en las dos. PSCI en aarch64; INIT/SIPI más un trampolín de 16→32→64 bits en x86_64. El núcleo nuevo copia las tablas de páginas y la captura de faults, y avisa por un atómico. |
| **El plazo que declara el agente** | Andando en las dos, **y en cualquier núcleo**. En el del protocolo lo hace cumplir el reloj local; en uno reclamado, el mismo timbre que corta un núcleo colgado — para el agente se ve igual, que es lo que importa. Y de paso las esperas del kernel dejaron de contarse en vueltas: `core.claim` espera medio segundo y `exec {core}` cinco, números que se pueden explicar. El tope en vueltas queda de red para la máquina que no diga a qué ritmo sube su reloj. `exec {deadline_ms}` corta el código si no vuelve en ese tiempo, y devuelve `cancelled`. Es lo que salva **el núcleo del protocolo**: ahí el agente corre `supervised` (D29) así que las interrupciones entran, pero el que tendría que mirar el reloj es justamente el que se colgó. El plazo lo declara el agente, igual que `mode`: el kernel no tiene una opinión sobre cuánto puede tardar su código, y un valor por omisión sería exactamente eso. |
| **Recuperar un núcleo colgado** | Andando en las dos, **con escalada de dos pasos** igual que Linux: primero el timbre normal —que alcanza para todo lo que no se tapó los oídos, que es casi todo— y después la línea que la máscara común no tapa. En x86_64 ese segundo escalón es el **NMI**, y con eso cae hasta el que hizo `cli`; en aarch64 sería el FIQ y **no se puede en esta máquina**, con el diagnóstico ya cerrado (el detalle está en `CLAUDE.md`): sólo el Grupo 0 se entrega como FIQ, así que el resto tendría que vivir en el Grupo 1 — y una interrupción del Grupo 1 acá queda pendiente pero **no se puede reconocer**, porque `GICC_IAR` devuelve 1022 ("es del otro grupo, leé `GICC_AIAR`") y `GICC_AIAR` lee cero: los registros del otro grupo existen sólo con extensiones de seguridad, y este GIC no las tiene. Sería posible en GICv3, pero eso es un driver nuevo y además se lleva puesto el MSI (GICv3 usa ITS en vez del frame GICv2m). El kernel **publica hasta dónde llega** (`describe {what:["exec"]}` trae `cancel`), así que el agente lo lee en vez de descubrirlo con un núcleo perdido. Andando en las dos. Un `exec` que no vuelve se corta con el mismo timbre que despierta núcleos: desde afuera no se puede desviar la ejecución de otro núcleo, solo pedirle que se desvíe solo, y el handler lo manda **al mismo punto de recuperación que usa un fault**. El corte vuelve como `cancelled`, aparte de `faulted`: el código no hizo nada mal, se lo cortaron. **Y si el código enmascaró las interrupciones no se puede, y el kernel lo dice** (`core-did-not-stop`, y el núcleo queda `lost`) — eso es D29, no un bug: en su núcleo la prioridad la decide el agente, incluido no ser molestado. Linux tampoco puede: en ARM64 su `cpu_kill` no mata, verifica, y avisa si el CPU no murió. |
| **Devolver un núcleo** | Andando. `release` acepta el handle de un núcleo, y **no lo apaga**: sigue vivo durmiendo en su buzón, así que reclamarlo otra vez no lo arranca de nuevo — la ranura es del núcleo físico para siempre, porque su bucle espera trabajo en *esa*. Lo que cambia es el handle, que nunca se reusa (D14). No se suelta uno con trabajo en curso: sería entregarlo con código de otro dueño adentro. |
| **Trabajo en un núcleo reclamado** | Andando en las dos. `exec {core}` deja el pedido en un buzón por núcleo y el núcleo **duerme** hasta que lo despierta un IPI/SGI. Comprobado con código del agente que informa en qué núcleo corre. El del protocolo espera **con los timbres abiertos**, así un handler del agente corre y el cordón se sigue atendiendo mientras dura el trabajo. **Y se puede no esperar** (`wait:false`): el resultado queda en `describe {what:["cores"]}` hasta que se mande otro trabajo, así que sobrevive a la desconexión (D14). |
| **`exec`** | Andando en las dos, con estado inicial de registros y todo. **El fault vuelve como respuesta, no como muerte** (P5): el handler desvía el regreso al punto de recuperación en vez de detener el núcleo. El agente corre en pila propia y las excepciones en otra, así que ni destruyendo el puntero de pila se lleva la máquina. |
| **Tablas de páginas propias** (D12) | Andando en las dos. Identity map con páginas de 1 GiB; MMIO no cacheable. La raíz se relee del registro y se verifica contra el mapa. |
| **Timbre del cable serie** | Andando en las dos. El núcleo duerme entre pedidos en vez de preguntarle al UART byte por byte. APIC + IO-APIC en x86_64, GIC en aarch64. Es la misma maquinaria que va a necesitar `irq.install`. |
| **Captura de faults** (P5, D7) | Andando en las dos. Causa + crudo + dirección + registros. Autotest de breakpoint en cada arranque. Todavía no viaja por CBOR ni vuelve al agente. |
| **`dma.allow`** (D8) | Andando en las dos: VT-d en x86_64, SMMUv3 en aarch64. Tablas de traducción por dispositivo, grano de 4 KiB, y el IOMMU **encendido desde el arranque** — sin declarar nada, ningún aparato llega a ninguna parte. Comprobado con un aparato de DMA de verdad en las dos, y lo que niega **queda anotado** por el silicio, así que el agente puede enterarse de que su driver apuntó a donde no debía (P5). |

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
2. **~~La dirección del PL011 sigue horneada.~~ RESUELTO.** El kernel arranca con una dirección
   horneada porque **tiene que poder hablar antes de leer ninguna tabla**: si el arranque se
   cuelga leyendo ACPI, el cable es lo único que queda para contarlo. Pero apenas la tabla SPCR
   dice dónde tiene la máquina su consola, **se muda ahí**, y de ese punto en adelante todo lo
   que sale del kernel pasa por la dirección que dijo la máquina. Con eso la horneada deja de
   ser algo que el kernel cree y pasa a ser solo con qué arranca (P4).

   El aviso va **antes** de mudarse, a propósito: si la dirección nueva no fuera un UART, la
   primera escritura se pierde y no habría con qué contarlo, así que la última línea que sale
   por el cable viejo tiene que decir a dónde se fue. Y no se muda a cualquier lado — una
   dirección fuera de lo que el identity map alcanza se rechaza diciéndolo.

   **Lo que esto no comprueba, y conviene saberlo:** en QEMU `virt` las dos direcciones
   coinciden, así que el camino de "mudarse a otra distinta" no se ejercita. Lo que sí queda
   comprobado es que la dirección **en uso** es la que informó la tabla: el kernel llama a la
   mudanza igual cuando coinciden, y todo el protocolo sale por ahí — si la tabla dijera
   cualquier cosa, el cordón se cortaría y el portón se pondría rojo. `describe` lo publica
   como `serial.in_use`.

   En x86_64 no hay a dónde mudarse: el UART está en puertos de E/S, que no son direcciones de
   memoria. El kernel lo dice en vez de intentarlo.

3. **~~Las tablas de ACPI se encuentran pero no se leen.~~ RESUELTO.** Se recorre el XSDT
   verificando el checksum de cada tabla, y de ahí salen los núcleos (MADT) y dónde se
   configura PCIe (MCFG). `describe` gana las secciones `cpus`, `interrupts` y `pcie`. Las
   tablas que este kernel no interpreta se informan igual por su firma: que exista algo que no
   sabemos leer es más útil que callarlo (P4).

   **Y el device tree también se lee** (`kernel-core/src/fdt.rs`), que era lo que faltaba
   encima. Una máquina sin ACPI ya no es una máquina sobre la que el kernel no sabe nada: de ahí
   salen los núcleos, el controlador de interrupciones con su versión, dónde está el puerto
   serie y por qué interrupción avisa, dónde se configura PCIe y dónde está el IOMMU. Con eso el
   kernel deja de estar atado a una máquina con ACPI, que era lo que más se alejaba de P4.

   Los dos formatos dicen lo mismo y no se parecen: ACPI son tablas con firma y checksum, el
   device tree es **un árbol** de nodos con nombres de texto y todo en big-endian, aunque la
   máquina no lo sea. Lo delicado no es el recorrido: es que **cuánto mide una dirección lo dice
   el nodo padre** (`#address-cells`), así que dar por sentado que son dos celdas de 32 bits
   anda en QEMU y falla en media placa real. Cuál de los dos usar no lo elige el kernel: es cuál
   dejó el firmware, y la decisión vive en un solo lugar (`tables::describe`) porque se toma en
   dos momentos muy separados —al armar el mapa de memoria y al describir el hardware— y
   tenerla escrita dos veces es tenerla escrita mal una vez.

   Se comprueba de las dos maneras. Con un blob armado a mano byte por byte en los tests, que
   permite preguntarle cosas que QEMU no ofrece; y **booteando la misma máquina con `acpi=off`**,
   donde el firmware pasa un device tree en lugar de las tablas y el portón exige que ande el
   IOMMU contra un aparato de verdad — que es la prueba que usa todo lo que sale de la
   descripción junto. Si algo saliera mal del árbol, esa no cierra.

   Lo que sigue faltando: de la MADT solo se sacan núcleos y el controlador; las rutas de
   interrupción (`irq.install` las va a necesitar) todavía no.
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

6. **~~`exec` no recibe un estado inicial de registros.~~ RESUELTO.** `exec {regs}` toma un mapa
   de nombre a valor, y los nombres son **los que informa esta máquina** (D3): se resuelven
   contra `REGISTERS` en vez de estar horneados en el protocolo. Un nombre que no existe se
   rechaza en vez de ignorarse — correr el código con un registro sin poner sería hacer algo
   distinto de lo que el agente pidió, sin decírselo.

   No se pueden poner todos, y `describe {what:["exec"]}` publica cuáles sí (P4). Quedan afuera
   cuatro, cada uno por su motivo: dónde empieza a ejecutar lo dice `off`; la pila la pone el
   kernel y ya la publicaba como `stack`; el registro de estado no es un valor que se cargue
   sino consecuencia de cómo se entra —y en `supervised` lleva las interrupciones prendidas a
   propósito (D29), así que dejarlo escribir sería dar por la ventana lo que D29 niega por la
   puerta—; y en aarch64 `x30`, que corriendo `raw` es la dirección a la que el código vuelve
   cuando termina.

   El que no se pide queda en cero, salvo el registro del primer argumento, que sigue llevando
   la dirección de entrada como antes. Si el agente **sí** lo pone, gana el agente: es su código
   (P2).

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

10. **~~Un test falló una vez y no reprodujo.~~ RESUELTO, y reproducido primero.** Era una
   carrera de verdad: leer la descripción de una máquina —de ACPI o del device tree— escribe en
   arreglos estáticos compartidos, y el `Hardware` que vuelve son **slices que apuntan ahí**.
   Con los tests en paralelo, otro test podía sobreescribir los núcleos entre la lectura y el
   `assert`.

   **La auditoría de entonces miró donde no era.** Revisó los tests que tocan las tablas de
   reclamos y de núcleos, y esos sí toman el candado; nadie miró los de descripción, que pisan
   **otros** estáticos. Es un recordatorio de que "se auditó lo único que puede causarlo" es una
   afirmación sobre lo que uno se acordó de mirar.

   Reproducido antes de arreglarlo, porque un arreglo de algo que no se vio fallar no se puede
   comprobar: corriendo **solo** los tres tests que comparten esos estáticos, con dieciséis hilos,
   dio **2 fallos en 400 corridas**. Con la suite entera no aparecía ni en 150. Después del
   arreglo, 0 en 400.

   Y se arregló de forma que no dependa de que el próximo test se acuerde: la función que lee
   una descripción devuelve el `Hardware` **con el guard del candado pegado**, así que el
   candado se suelta cuando el dato deja de usarse y **eso lo hace cumplir el compilador**.

13. **~~Un `exec` en otro núcleo es sincrónico.~~ RESUELTO.** `exec {core, wait:false}` deja el
   trabajo y **contesta enseguida**; el resultado se busca después en
   `describe {what:["cores"]}`, donde cada núcleo trae un `work` que dice si está corriendo o
   qué contestó. Un trabajo largo ya no se informa igual que un núcleo perdido, y el núcleo no
   queda marcado como fallado sin serlo. La forma que espera sigue estando y sigue siendo la de
   por omisión.

   **No hizo falta un verbo nuevo ni un handle de trabajo**, aunque esta deuda pedía uno. Un
   núcleo corre un trabajo por vez, así que el handle del núcleo ya lo identifica; inventarle
   otro habría sido un nombre nuevo para algo que ya tenía nombre. Y el resultado va en
   `describe` porque **es estado de la máquina**, que es lo que `describe` sirve (P4) — con lo
   cual sobrevive a la desconexión sin código extra, que era justo lo que la deuda pedía de D14:
   el agente puede mandar algo largo, irse, y volver a buscarlo.

   `wait` **sí** tiene valor por omisión, a diferencia de `mode`, y la diferencia no es de
   comodidad: `mode` declara con qué privilegio corre el código del agente —una propiedad del
   código, que solo él puede decidir (D27)—, mientras que `wait` dice cómo quiere la respuesta
   el que pregunta. Ahí el kernel no está eligiendo nada sobre el agente.

   **Lo que queda:** un núcleo cuyo código se colgó queda ocupado. El agente lo ve —`work` dice
   `running` para siempre— pero no lo puede recuperar. Un bucle infinito no es un fault y el
   kernel no tiene cómo distinguirlo de un trabajo largo, que es exactamente la razón por la que
   este verbo dejó de esperar.

14. **~~En aarch64 el SMMUv3 se informa pero no se programa.~~ RESUELTO.** `dma.allow` hace lo
   mismo en las dos arquitecturas, y lo comprueba el mismo aparato de verdad: bloqueado sin
   declarar —y el silicio lo anota—, permitido al declararlo, bloqueado otra vez al soltar el
   reclamo. Con eso **no queda nada que un agente pueda pedir en una arquitectura y no en la
   otra** salvo `irq.install_raw`, que en aarch64 no tiene sentido porque no hay un camino más
   crudo que el que ya se usa.

   El SMMUv3 no se parece al VT-d, y ahí está el valor de D22: al de Intel se le habla por
   registros, a este por **colas en memoria** —para invalidar hay que dejarle un comando en un
   anillo y avisarle—, y en vez de una tabla por bus tiene una **tabla de streams** indexada
   por el número que el bus le pone al aparato. Se usa la traducción de **etapa 2**, que es la
   única cuya entrada de stream lleva directo la raíz de las tablas del aparato.

   Salieron tres cosas al hacerlo y ninguna se parecía a su causa; están anotadas en
   `CLAUDE.md`. La más cara: **el aparato de prueba recortaba la dirección de DMA a 28 bits**,
   así que en aarch64 —donde la RAM arranca en 1 GiB— no llegaba nunca, y eso es
   indistinguible de un IOMMU bloqueando bien. Se encontró booteando sin IOMMU, que es
   justamente lo que este documento ya decía que había que hacer antes de creerle a un bloqueo.

   Para poder probarlo hubo que cerrar antes otra cosa: en aarch64 el agente **no alcanzaba
   PCIe**. El mapa de memoria de UEFI no informa la ventana de configuración —en x86_64 sí—,
   así que `mem.claim` la rechazaba por caer fuera del mapa y el identity map ni la cubría,
   mientras `describe pcie` publicaba su dirección. El kernel siendo la razón por la que no se
   puede usar un aparato es exactamente lo que P1 prohíbe. Quien sí sabe dónde está es la MCFG
   de ACPI, y la ventana se suma al mapa **donde el mapa se arma** (`boot-uefi`): metida antes
   de que nadie lo lea, entra sola en todo lo que se calcula a partir de él, empezando por
   hasta dónde llega el identity map.

15. **~~Un BAR que asignó el firmware puede no estar en el mapa de memoria.~~ RESUELTO.**
   `mem.claim` por dirección exacta ahora entrega un rango que cae en un **hueco** del mapa,
   siempre que el hueco sea entero —ni un borde adentro de una región— y que el identity map lo
   alcance. Sale con la clase `unreported`, que no es `mmio`: entregarlo con ese nombre sería
   afirmar lo que la máquina no dijo (P4). El agente se lleva el rango **y** la advertencia.

   El rechazo anterior no protegía nada, y ahí está el argumento: el identity map cubre el hueco
   igual, así que el código del agente en `exec` ya le escribía. Lo único que hacía `unmapped`
   era obligar a un rodeo. Una capa que estorba sin hacer cumplir nada es la que P2 manda sacar,
   y decidir qué aparatos existen no le toca al kernel (P1, D4).

   **Y no alcanzaba con eso, que es lo que la prueba encontró.** Reclamar la ventana y leerla
   devolvía ceros: `mem.read`/`mem.write` accedían **de a un byte**, y muchos registros solo
   aceptan accesos de su ancho exacto y descartan los más angostos. Ahora los dos verbos toman
   `width` (1, 2, 4 u 8), que **declara el agente** porque el kernel no sabe qué hay del otro
   lado (D4); por omisión sigue siendo uno. Se lee una vez por palabra y se reparte en bytes,
   porque hay registros que cambian de valor con solo mirarlos.

17. **~~La ventana de rescate del blob se cuenta en vueltas, no en tiempo.~~ RESUELTO.** El
   kernel lee un reloj, así que la ventana de D18 dura **dos segundos** y lo dice: el aviso sale
   en milisegundos y no en "vueltas, que duran lo que duren". Comprobado midiendo desde afuera:
   2029 ms contra 2000 prometidos.

   No es un driver ni le pide nada al firmware. En las dos arquitecturas el contador ya viene
   andando desde antes que el kernel y se lee con una instrucción — `rdtsc`, `mrs cntpct_el0`.
   Lo que hace falta averiguar es **a qué ritmo sube**, y ahí las dos no se parecen:

   - **aarch64 lo dice**, en `CNTFRQ_EL0`. Una instrucción y listo.
   - **x86_64 puede no decirlo.** El TSC cuenta ciclos y la frecuencia sale de CPUID, en dos
     hojas distintas y ninguna obligatoria — QEMU no informa ninguna de las dos. Así que cuando
     el CPU no habla, **se mide**: contra el contador que informa la FADT, que sube siempre a
     3.579545 MHz en cualquier máquina. Se cuentan los ciclos del TSC que caben en un pedazo
     conocido de ese otro contador.

   Un cero es la forma en que la máquina dice "no lo sé", y ahí el reloj vuelve a ser `None`:
   **un tiempo mal calculado es peor que no tener tiempo** (P4). El contador se puede leer igual
   y sirve para comparar dos lecturas, pero quien necesite un plazo vuelve a contar vueltas — y
   el kernel lo avisa en vez de prometer segundos que no puede cumplir.

   Y se publica en `describe {what:["clock"]}`, porque el agente lo necesita por lo mismo que lo
   necesita el kernel. La prueba no es que informe un número: es que **dos lecturas separadas
   por medio segundo den medio segundo**. Da 0.502 en aarch64 y 0.509 en x86_64.

   **Lo que salió al hacerlo**, y es de las que valen: el tope de vueltas seguía cortando la
   ventana a los 1200 ms mientras el kernel anunciaba 2000. Estaba pensado como red por si el
   reloj no avanza, pero se disparaba **antes** que lo que protegía. Una red que se activa antes
   que la cosa que cuida no es una red: es un límite disfrazado.

16. **~~Un acceso de ancho inválido a MMIO mata el kernel en aarch64.~~ RESUELTO.** Salió de la
   prueba de la deuda 15, y era una asimetría que solo aparece con las dos arquitecturas (D22):
   leer un registro de 4 bytes de a uno en x86_64 devuelve ceros y sigue, mientras que en
   aarch64 el bus lo rechaza con un abort externo y **la máquina quedaba muda**.

   El agujero era de P5: `mem.read` y `mem.write` corren en el camino del protocolo, donde no
   había punto de recuperación. Durante un `exec` un fault vuelve como dato porque el handler
   desvía el regreso; ahí el acceso lo hace el propio kernel y no había a dónde desviarlo. Con
   lo cual el agente podía dejar la máquina sin cordón con un pedido perfectamente legítimo, que
   es lo que D5 y D17 dicen que no puede pasar.

   Se arregló con **la misma maquinaria de `exec`, usada afuera de `exec`**: `guarded.rs` en
   cada arquitectura arma el punto de recuperación en el bloque del núcleo, hace **un** acceso, y
   lo desarma. La ventana armada es de una sola instrucción a propósito — cuanto más corta,
   menos chance de capturar un fault que no era el que se esperaba. El handler no necesitó
   cambios: ya desviaba con solo ver `armed`.

   El pedido rechazado vuelve como `access-refused`, con la dirección exacta que cortó, la causa
   normalizada y **los números crudos** con los que la máquina lo dijo: la causa sola no alcanza
   para distinguir un rango que no existe de un aparato que rechazó el ancho (P4).

   Y la prueba, que es la que vale, ahora corre en las dos: se lee mal a propósito, se comprueba
   que el kernel lo informe, y después **se le vuelve a hablar a la máquina**.

11. **~~Los atributos de cacheabilidad que informa UEFI se descartan.~~ RESUELTO.** El mapa de
   memoria lleva ahora, región por región, si la máquina dijo que se puede cachear, y el mapeo
   **prefiere eso a deducirlo de la clase** (P4). Deducir andaba casi siempre, y "casi siempre"
   en cacheabilidad significa un dispositivo que no se entera de una escritura.

   Se normaliza en vez de guardar los bits crudos: `EFI_MEMORY_WB` es una palabra de UEFI y
   `kernel-core` no sabe cómo arrancó la máquina (D24). Son tres valores y el tercero importa:
   `write-back`, `uncacheable`, y **`unknown`** — que el firmware no lo haya dicho no es lo
   mismo que decir que no se puede cachear, y ahí se sigue deduciendo de la clase.

   **El orden en que se pregunta decide todo:** la RAM común informa que soporta write-back *y*
   quedar sin cachear, así que si se mirara primero lo segundo, toda la memoria de la máquina
   quedaría sin cache. Hay un test que fija ese orden.

   Y se informa por el protocolo, en la respuesta de `mem.claim`, porque el agente lo necesita
   para escribir un driver. Eso es además lo que permite comprobarlo sin creerle al kernel: en
   la misma corrida, la RAM sale `write-back` y los registros de PCIe salen `uncacheable`. Si
   todo viniera con la misma etiqueta, el dato no vendría de la máquina.

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
    - **~~La restricción de D29 quedó pendiente.~~ SE HACE CUMPLIR.** En el núcleo del protocolo
      `exec` solo admite `supervised`; `raw` exige un núcleo reclamado. Estuvo pendiente mientras
      `exec` corría siempre acá, porque exigirla habría dejado a `raw` sin ningún lugar donde
      correr — el kernel ofrecería dos modos con uno inalcanzable, que no es lo que dice D27.
      Desde que `exec` elige núcleo, ya no. No es el kernel eligiendo por el agente (P6): los dos
      modos siguen estando y los dos se usan; lo que cambia es **dónde**. Y el kernel lo
      **publica** en vez de dejar que se descubra chocándose: `describe {what:["exec"]}` trae
      `this_core: supervised` (P4). Las otras dos comprobaciones siguen igual: `supervised` exige
      memoria con `user`, y `raw` exige que no lo sea.

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
2. **Por dónde seguir.** **Los once verbos andan, y ahora enteros en las dos arquitecturas**
   (deuda 14 cerrada). Ya no queda nada que un agente pueda pedir en una y recibir solo en la
   otra, salvo `irq.install_raw`, que en aarch64 no tiene un camino más crudo que ofrecer.

   Y la restricción de D29 **se hace cumplir**: en el núcleo del protocolo el agente corre
   `supervised`, y si quiere el privilegio entero reclama un núcleo. Fue un cambio chico en el
   verbo y uno grande en las pruebas, como estaba previsto — casi todas corrían `raw` en el
   núcleo que atiende y pasaron a pedir uno reclamado. Eso mismo resultó ser lo más valioso:
   **obligó a que el camino de D13 se use de verdad en todas las pruebas**, y ahí apareció que
   el núcleo del protocolo esperaba a otro núcleo **con los timbres cerrados**. Un handler que
   el agente había instalado no corría mientras durara el trabajo, y el cordón tampoco se
   atendía. Estaba desde que `exec` acepta `core`, y nadie lo veía porque ninguna prueba
   disparaba una interrupción desde otro núcleo.

3. **Si `exec` debe recibir un estado inicial de registros.** La sección 4 lo especifica
   (`exec(core, handle, off, regs)`) y hoy no lo hace: el código recibe solo su propia
   dirección. Sumarlo es fácil; la pregunta es qué nombres se aceptan, y ahí manda D3 — tendrían
   que ser los que informa `describe`, no una lista horneada.

4. **~~Leer el device tree.~~ HECHO** (deuda 3). Una placa sin ACPI reporta sus núcleos, sus
   buses y dónde está su propio cable. Y de paso cerró la otra mitad de la deuda 2: la dirección
   del PL011 sale de su fuente legítima en los dos dialectos, y el kernel se muda ahí.

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
