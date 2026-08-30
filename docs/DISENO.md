# Kernel agente-céntrico — Documento de diseño

**Estado:** en discusión. Sin código todavía.
**Última actualización:** 2026-08-30

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
| D12 | **El kernel identity-mapea toda la RAM** (virtual = física, páginas de 1 GiB). El agente que quiera tablas propias las arma y las carga desde su código en `exec`. | El formato de tablas es lo menos portable que hay (x86_64 4-5 niveles, ARM64 TTBR0/1, RISC-V Sv39/48/57): si lo maneja el agente, rompe D3. Y los dispositivos hablan en físicas, así que identity map elimina el segundo sistema de coordenadas al escribir drivers. Aviso: el agente que carga tablas propias y no mapea el kernel y el UART pierde el cordón umbilical. |
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

---

## 4. Superficie del kernel

Esto es el kernel entero. No hay más.

| Verbo | Qué hace |
|---|---|
| `describe` | Devuelve la máquina real: núcleos, registros disponibles, mapa de memoria física, dispositivos, cachés, NUMA. |
| `mem.claim(bytes, constraints)` | Reclama marcos de memoria física — o un rango MMIO / BAR de un dispositivo. Devuelve un handle. RAM se mapea cacheable; MMIO **no-cacheable** (si no, la CPU cachea las escrituras a registros y el dispositivo nunca se entera). |
| `mem.read(handle, off, len)` | Bytes crudos hacia afuera. |
| `mem.write(handle, off, bytes)` | Bytes crudos hacia adentro. |
| `core.claim(id)` | Un núcleo físico, con el timer enmascarado. |
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

## 7. Preguntas abiertas

1. **Dónde vive el código.** Todavía sin definir. Repo aparte, no en empujoneducativo.

---

## 8. Estado del entorno de desarrollo

Instalado y verificado en esta sesión:

- Rust 1.94.1 con targets bare-metal: `x86_64-unknown-none`, `x86_64-unknown-uefi`,
  `aarch64-unknown-none`, `aarch64-unknown-uefi`
- QEMU 8.2.2: `qemu-system-x86_64` y `qemu-system-aarch64`
- Firmware UEFI: OVMF para x86 (`/usr/share/OVMF/`) y AAVMF para ARM (`/usr/share/AAVMF/`)
- `gdb`, `ld.lld`

Las dos arquitecturas se pueden bootear y probar acá mismo, sin hardware.

Sin GPU ni passthrough en este contenedor: los dispositivos reales solo se podrán probar en
hardware propio.
