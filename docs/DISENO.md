# Kernel agente-céntrico — Documento de diseño

**Estado:** las dos arquitecturas arrancan por UEFI, le toman la máquina al firmware y
**hablan el protocolo CBOR** por el cordón umbilical. Corren sobre pila y tablas de páginas
propias, capturan los faults como datos, y **siete de los diez verbos andan**: el agente
reclama memoria, sube código máquina, lo corre, y arranca los otros núcleos.
Faltan `irq.install`, `irq.install_raw` y `dma.allow`.
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
| D24 | **La frontera se parte en dos ejes, no en uno: arquitectura y entorno de arranque.** El código de UEFI vive en un crate propio (`boot-uefi`) que no lleva `asm!` y lo comparten las dos arquitecturas. Los **tipos normalizados** (región de memoria, dispositivo) viven en `kernel-core`. | `kernel-x86_64` en realidad significaba "x86_64 **+ UEFI**": el `asm!` varía por arquitectura, pero cómo se pide el mapa de memoria varía por entorno de arranque, y el *formato* de ese mapa no varía por ninguno de los dos (UEFI lo estandarizó). Meter UEFI en cada crate de arquitectura lo duplicaría idéntico; meterlo en `kernel-core` dejaría a `check-frontera.sh` dando verde sobre un núcleo casado con UEFI — un falso positivo, peor que un rojo. D18 ya anticipa el segundo eje ("en ARM/RISC-V embebido el equivalente es la ROM de arranque"): con esta partición, un `boot-embedded` entra sin tocar `kernel-core`. **Los tipos normalizados van en `kernel-core` o esto degenera:** si se los queda `boot-uefi`, cada entorno de arranque inventa su propio vocabulario y no hay frontera. |
| D25 | **Al firmware se le pide todo antes de `ExitBootServices`, que se llama una sola vez, en el arranque.** Mapa de memoria, punteros a ACPI / device tree y el blob (D19): todo en esa ventana. | No hay segunda oportunidad: después de salir, llamar a un Boot Service es un crash. Y hay una dependencia que obliga: **lo único que sabe leer FAT32 es el firmware**, así que el blob de D19 —que vive en la partición EFI al lado del kernel— solo se puede cargar antes de salir. Salir apenas arranca haría imposible D18/D19. Además `ExitBootServices` exige la *llave* del mapa más reciente: si algo pide memoria entremedio, la llave queda vieja y la llamada falla. El orden es rígido y conviene que sea un único momento fijo, no un estado que el kernel tenga que rastrear. |
| D27 | **El agente declara con qué privilegio corre su código.** `exec` acepta `supervised` (anillo 3 en x86_64, EL0 en aarch64) o `raw` (anillo 0 / EL1, que es lo que hay hoy). El kernel ofrece los dos y no elige por él. | Hoy el código del agente corre con el mismo privilegio que el kernel, así que una sola instrucción —`cli` en x86, `msr daifset` en ARM— deja la máquina muda sin recuperación posible: es lo único que el agente puede hacer y de lo que el kernel no puede volver. Todo lo demás ya se recupera (un fault vuelve como dato por P5; un bucle infinito se puede desviar con el punto de retorno de `exec`). **El aislamiento no contradice el proyecto: `DESCARTADO.md` ya dice que "importa más con un operador estocástico, no menos".** Lo que sí lo contradiría es que el kernel lo *imponga* — por eso lo declara el agente, y ahí el hardware hace cumplir lo declarado y no una política, que es P6 al pie de la letra. **El precio de `supervised` es acotado y hay que saberlo:** se pierden las tablas de páginas propias (D12), los MSRs, los puertos de E/S de x86 y poder enmascarar interrupciones. Lo que **no** se pierde es la parte central de un driver: los registros de un dispositivo PCIe están mapeados en memoria, y escribirles es una instrucción común que anda igual en anillo 3. Y una violación vuelve como fault estructurado, así que la frontera se documenta sola con maquinaria ya escrita: el agente se entera de qué intentó y puede volver a pedirlo en `raw`. **Complementa D8 en vez de duplicarlo:** el privilegio impide romper el kernel con el CPU, el IOMMU impide romperlo con un dispositivo; un agente sin privilegio que programa mal un DMA sigue pudiendo pisar todo. Costo de implementación conocido: las tablas de páginas dejan de ser cuatro entradas de 1 GiB puestas una vez, porque hay que marcar qué páginas alcanza el agente y a esa granularidad el kernel comparte gigabyte con todo lo demás. |
| D26 | **El puerto serie va crudo: sin multiplexor de monitor.** Los scripts usan `-serial stdio`, **nunca** `-serial mon:stdio`. Se sale de QEMU con `Ctrl-C`. | Con `mon:`, QEMU multiplexa su monitor sobre la misma terminal, y ese multiplexor **se come el byte `0x01` (Ctrl-A) como escape junto con el que le sigue**. Por este puerto viaja CBOR y, más adelante, código máquina (D6): ahí `0x01` es un byte tan legítimo como cualquier otro. Se descubrió de la peor manera posible — el primer pedido del cliente empezaba con `83 01 68` y QEMU contestó su pantalla de ayuda, porque leyó `Ctrl-A h`. La comodidad de `Ctrl-A X` no vale un canal que corrompe mensajes en silencio. |

---

## 4. Superficie del kernel

Esto es el kernel entero. No hay más.

| Verbo | Qué hace |
|---|---|
| `describe` | Devuelve la máquina real: núcleos, registros disponibles, mapa de memoria física, dispositivos, cachés, NUMA. |
| `mem.claim(bytes, constraints)` | Reclama marcos de memoria física — o un rango MMIO / BAR de un dispositivo. Devuelve un handle. RAM se mapea cacheable; MMIO **no-cacheable** (si no, la CPU cachea las escrituras a registros y el dispositivo nunca se entera). |
| `mem.read(handle, off, len)` | Bytes crudos hacia afuera. |
| `mem.write(handle, off, bytes)` | Bytes crudos hacia adentro. |
| `core.claim(id, modo)` | Un núcleo físico. En `dedicated` es solo del agente, con el timer enmascarado. En `shared` es el núcleo que atiende el protocolo: el kernel le pide prestados microsegundos cuando llega un pedido. El núcleo del protocolo **nunca** se entrega como `dedicated`, y el kernel lo dice con los datos para que el agente decida (P4). |
| `exec(core, handle, off, regs)` | Salta a código máquina. Devuelve estado de registros + fault si lo hubo. |
| `irq.install(vector, handle, off)` | Instala un handler. El kernel pone prólogo, epílogo y EOI. |
| `irq.install_raw(vector, handle, off)` | Igual, pero el agente hace todo. Sin red de contención. |
| `dma.allow(device, handle)` | Declara qué memoria puede tocar un dispositivo. Programa el IOMMU. |
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
De los diez verbos de la sección 4 hay **siete** implementados; los otros tres todavía no.

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
| **`mem.claim` · `mem.read` · `mem.write` · `release`** | Andando. Reclamos por tamaño o por dirección exacta (así se pide MMIO), con alineación y tope. Los handles son de la máquina y no se reusan (D14). |
| **`core.claim`** | Andando en las dos. PSCI en aarch64; INIT/SIPI más un trampolín de 16→32→64 bits en x86_64. El núcleo nuevo copia las tablas de páginas y la captura de faults, y avisa por un atómico. |
| **`exec`** | Andando en las dos. **El fault vuelve como respuesta, no como muerte** (P5): el handler desvía el regreso al punto de recuperación en vez de detener el núcleo. El agente corre en pila propia y las excepciones en otra, así que ni destruyendo el puntero de pila se lleva la máquina. |
| **Tablas de páginas propias** (D12) | Andando en las dos. Identity map con páginas de 1 GiB; MMIO no cacheable. La raíz se relee del registro y se verifica contra el mapa. |
| **Captura de faults** (P5, D7) | Andando en las dos. Causa + crudo + dirección + registros. Autotest de breakpoint en cada arranque. Todavía no viaja por CBOR ni vuelve al agente. |
| Los otros tres verbos | `irq.install`, `irq.install_raw`, `dma.allow`. |

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
2. **La dirección del PL011 sigue horneada** en `kernel-aarch64/src/uart.rs` (`0x0900_0000`, la
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

7. **Al reportar un fault, dos núcleos que fallan a la vez entrelazan la salida del cordón.**
   No se corrompe nada —el estado del fault ya es por núcleo— pero el texto sale mezclado y se
   lee mal. Cuando los faults viajen por CBOR en vez de por texto deja de importar.

8. **Un núcleo reclamado todavía no puede recibir trabajo.** Arranca, se configura solo y queda
   esperando, pero `exec` corre siempre en el núcleo que atiende el protocolo: falta un buzón por
   núcleo y que `exec` acepte a cuál mandarle el trabajo, que es lo que la sección 4 especifica
   (`exec(core, handle, off, regs)`).

9. **Un test falló una vez y no reprodujo.** Ocurrió una sola vez en la suite de `kernel-core` y
   no se repitió en veinte corridas seguidas. Se auditó lo único que puede causarlo —los tests
   que tocan las tablas globales de reclamos y de núcleos— y todos toman el mismo candado. **No
   está diagnosticado**; queda anotado para no darlo por inexistente si vuelve a pasar.

10. **Los atributos de cacheabilidad que informa UEFI se descartan.** D12 anda igual porque la
   cacheabilidad se deduce de la *clase* de cada región, pero UEFI informa además atributos por
   región (`UC`, `WC`, `WT`, `WB`) que son más precisos que esa deducción. Mientras el grano del
   mapeo sea 1 GiB casi no cambia nada; cuando haya que mapear MMIO fino con `mem.claim`, sí.

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
2. **Por dónde seguir: cuál de los tres verbos que faltan.** Los tres son grandes y ninguno
   bloquea a los otros.

   - **`core.claim`** — arrancar los otros núcleos. Ya sabemos cuántos hay y con qué
     identificador se los nombra (la MADT lo dice). En aarch64 es una llamada PSCI al
     firmware, que es corto; en x86_64 hay que mandar INIT/SIPI por el APIC, que es más largo.
     No se parecen en nada.
   - **`irq.install`** — que el agente ponga sus propios handlers (D9). Hace falta programar el
     controlador de interrupciones, y falta leer de la MADT las rutas de interrupción, que hoy
     no se sacan.
   - **`dma.allow`** — el IOMMU. **Es el más grande de todo el proyecto** y el más específico
     de cada fabricante: VT-d en Intel, SMMU en ARM, sin nada en común. Y D8 lo pone encendido
     por defecto, así que no es opcional. Además es el que más gana con silicio real: el IOMMU
     emulado de QEMU no es el de verdad.

3. **Si `exec` debe recibir un estado inicial de registros.** La sección 4 lo especifica
   (`exec(core, handle, off, regs)`) y hoy no lo hace: el código recibe solo su propia
   dirección. Sumarlo es fácil; la pregunta es qué nombres se aceptan, y ahí manda D3 — tendrían
   que ser los que informa `describe`, no una lista horneada.

4. **Leer el device tree.** Hoy se sabe encontrarlo pero no se lee, así que una placa embebida
   —que no tiene ACPI— no reporta ni núcleos ni buses. Es también lo que haría falta para sacar
   la dirección del PL011 de su fuente legítima en vez de tenerla horneada (deuda 2).

3. **Qué del System Table cruza la frontera.** El mapa de memoria *normalizado* es portable;
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
