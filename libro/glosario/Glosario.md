---
tipo: glosario
estado: vivo
---

# Glosario

**Una línea por término, y un enlace.** Acá no se explica nada: si necesitás más de una línea, el término tiene su nota de concepto y está enlazado.

Si dos términos te suenan iguales, el lugar es [[Falsos-amigos]].

> [!note] Cómo crece
> Cada vez que aparece un término nuevo en un capítulo, entra acá con su línea. Un glosario que se escribe al final no sirve: el que sabe ya no se acuerda de qué era confuso.

---

## A

- **ABI** (*Application Binary Interface*) — El acuerdo de **cómo** se pasan los argumentos, por qué registros y en qué orden. Distinto de la API: la ABI la fija el compilador y el *target*, no la arquitectura. Ver [[27-La-ABI-la-pone-el-target-no-el-silicio]].
- **ACPI** — El dialecto de tablas con que el firmware de una PC describe la máquina: cuántos núcleos, dónde está la consola, dónde el IOMMU. Ver [[ACPI]].
- **Aborto** (*abort*) — Excepción de la que no se puede volver de forma confiable, porque el estado quedó incierto. Ver [[Falsos-amigos#2]].
- **AML** — Un lenguaje de programación entero embebido dentro de ACPI. Interpretarlo es lo que hace caro averiguar qué cable le toca a un aparato, y es por qué [[MSI|MSI]] lo vuelve innecesario.
- **Anillo** (*ring*) — Tres cosas distintas. Ver [[Falsos-amigos#3]].
- **APIC** — El controlador de interrupciones moderno de x86: reparte interrupciones entre núcleos y deja que un núcleo le toque el timbre a otro. Ver [[Interrupcion]].
- **Aparato** — Todo lo que hay en la máquina que **no es el procesador ni la memoria** y con lo que el procesador puede hablar. Se define por cuatro canales: registros, avisos, DMA y descubrimiento. Ver [[Aparato]].
- **Atómico** — Una operación que ocurre entera o no ocurre; ningún otro núcleo la ve a mitad de camino.

## B

- **BAR** (*Base Address Register*) — Un registro de un aparato PCIe que dice **en qué dirección aparecen sus registros**. El aparato no elige: el firmware o el kernel le escriben la dirección. Ver [[BAR]].
- **Barrera** (*fence*, *barrier*) — Instrucción que le prohíbe al procesador (o al compilador) reordenar accesos a memoria de un lado al otro. Ver [[42-Ordenamiento-de-memoria]].
- **Big-endian** — Guardar el byte más significativo primero. El device tree es big-endian; las dos arquitecturas de este kernel no.
- **Bit** — Un cable con corriente (1) o sin corriente (0). No hay nada más: un bit **es un voltaje**. Cómo se guarda uno: [[Flip-flop]].
- **Blob** — Un pedazo de código o datos que el sistema trata como opaco: lo carga y lo ejecuta sin entenderlo. Acá, [[51-El-blob-y-la-ventana-de-rescate|`blob.bin`]].
- **Bus** — El camino por el que el procesador alcanza algo que no es memoria. Ver [[Bus]].

## C

- **Cable** — En este proyecto, el cable serie por el que el kernel habla: el **cordón umbilical** (D5). El aparato es el [[UART]], y `describe {what:["cable"]}` informa su estado, incluidos los bytes que perdió.
- **Caché** — Copia rápida de memoria lenta. Miente cuando la dirección es un registro de aparato. Ver [[Cache]].
- **CBOR** — Formato binario de datos, parecido a JSON pero en bytes. El protocolo de Kornelia va en CBOR porque transporta código máquina y volcados (D6).
- **Ciclo** — Un tic del reloj del procesador. La unidad de tiempo del silicio. Ver [[01-El-reloj-y-el-transistor]].
- **`cli`** — La instrucción de x86 que dice "no me interrumpan". Lo único de lo que un kernel no puede volver por su cuenta, y por eso existe el [[39-Plazos-y-cortes|segundo escalón]].
- **CNTPCT_EL0** — El contador que ya viene andando en aarch64. El reloj sin driver. Ver [[37-El-reloj-contadores-y-no-saber-la-frecuencia]].
- **Controlador** — En español significa **dos cosas**: el chip (*controller*) y el código que le habla (*driver*). Ver [[Falsos-amigos#13]].
- **Coherencia** — La garantía de que dos núcleos que miran la misma dirección ven lo mismo. La da el silicio, con esfuerzo.

## D

- **Descriptor** — Una estructura en memoria con un formato que **define el silicio**, no tu programa: entradas de tablas de páginas, de la GDT, de las colas del IOMMU. Los bits importan de a uno.
- **Device tree** (*DT*, *FDT*) — El otro dialecto con que una máquina se describe, usado donde no hay ACPI: placas ARM y RISC-V. Un árbol de nodos, en big-endian. Ver [[Device-tree]].
- **Dirección** — Un número que apunta a algo. Hay **cuatro clases** —virtual, física, de bus e IOVA— y confundirlas es la causa clásica de que un [[DMA]] escriba en el lugar equivocado. Ver [[Falsos-amigos#4]].
- **Dispositivo** — Sinónimo de [[Aparato|aparato]], más formal. Es la traducción habitual de *device* en la literatura en español.
- **DMA** (*Direct Memory Access*) — Que el aparato lea o escriba memoria **por su cuenta**, sin que el procesador copie byte por byte. Ver [[DMA]].
- **Driver** — El código que sabe hablarle a un aparato. En Kornelia no lo tiene el kernel: lo escribe el agente (D4). Ver [[Driver]].
- **Doble fallo** (*double fault*) — Una excepción que ocurre mientras se atiende otra. Si eso también falla, la máquina se reinicia sin decir nada.

## E

- **EL** (*Exception Level*) — Lo que en x86 es el anillo, en aarch64: EL0 usuario, EL1 kernel, EL2 hipervisor, EL3 firmware. Los números van al revés que en x86.
- **ELF** — El formato de ejecutable de Linux. Ver [[ELF-y-PE]].
- **Espacio de direcciones** — El mapa de un programa: no *tiene* memoria, tiene rangos mapeados con huecos enormes en el medio. Ver [[Espacio-de-direcciones]].
- **`eret`** — La instrucción de aarch64 con la que se vuelve de una excepción, y también con la que se **baja** de privilegio. Ver [[25-Como-se-baja-de-privilegio]].
- **`ExitBootServices`** — La llamada de UEFI con la que el kernel toma la máquina. Se llama **una sola vez** y después no hay segunda oportunidad: todo lo que haga falta del firmware hay que pedirlo antes (D25).
- **Excepción** — Término paraguas para trap, fault y abort. Ver [[Falsos-amigos#2]].

## F

- **Fault** — Excepción de la que **sí** se puede volver, porque la instrucción no alcanzó a cambiar nada. En Kornelia vuelve al agente como dato (P5). Ver [[Fault]].
- **FIQ** — La interrupción de alta prioridad de ARM, que `msr daifset, #2` no tapa. Sería el segundo escalón para cortar un núcleo, y [[39-Plazos-y-cortes|en esta máquina no se puede usar]].
- **Firmware** — Código que ya venía en la máquina. UEFI es firmware. Ver [[Firmware]].
- **Flip-flop** — El circuito que guarda un bit capturándolo **en el flanco** del reloj. Un registro de 64 bits son 64 de estos. Ver [[Flip-flop]].

## G

- **GDT** (*Global Descriptor Table*) — La tabla de x86 donde viven los descriptores de segmento. En 64 bits casi no direcciona nada, pero sigue mandando el privilegio.
- **GIC** — El controlador de interrupciones de ARM. Hace lo mismo que el APIC y no se parece. Ver [[Interrupcion]].
- **Grupo** (del GIC) — La partición de interrupciones que decide si se entregan como IRQ o como FIQ. Ver [[39-Plazos-y-cortes]].

## H

- **Handler** — La función que atiende una interrupción o excepción. Corre en un contexto raro: no puede bloquearse ni tardar. Ver [[Handler]].
- **Handle** — Un número con el que el agente se refiere a algo que reclamó (memoria, núcleo, aparato). Ver [[23-Asignadores-y-por-que-aca-no-hay]].
- **`hlt` / `wfi`** — "No tengo nada que hacer, despertame cuando llegue una interrupción". La diferencia entre dormir y girar. Ver [[38-Dormir-en-vez-de-girar]].

## I

- **Identity map** — Mapear cada dirección virtual a la misma física. La mentira más simple que sirve, y la que elige Kornelia (D12). Ver [[Memoria-virtual]].
- **IDT** (*Interrupt Descriptor Table*) — La tabla de x86 donde se anota qué función atiende cada uno de los 256 vectores. Ver [[Fault]].
- **Instrucción** — Un puñado de bytes en memoria que el procesador trae, decodifica y ejecuta. **No es una idea: son bytes**, y se pueden mirar con `objdump -d`. Ver [[03-Que-hace-realmente-una-instruccion]].
- **IOMMU** — Una MMU para los aparatos: traduce y **filtra** lo que un aparato pide de la memoria. En Kornelia arranca encendido y vacío (D8). Ver [[IOMMU]].
- **Interrupción** — El aparato avisa en vez de que el procesador pregunte. Ver [[Interrupcion]] y [[Falsos-amigos#2]].
- **IOVA** — La dirección virtual de un aparato, la que el IOMMU traduce. Ver [[Falsos-amigos#4]].
- **IPI** (*Inter-Processor Interrupt*) — Cuando un núcleo le toca el timbre a otro. En ARM se llama SGI. Ver [[44-Mandar-trabajo-buzones-e-IPI]].
- **IST** (*Interrupt Stack Table*) — El mecanismo de x86_64 para atender una excepción en **otra pila**. Lo que hace que destruir el puntero de pila no mate la máquina. Ver [[31-La-pila-que-sobrevive]].
- **`iretq`** — La instrucción de x86_64 con la que se vuelve de una interrupción, y también con la que se **baja** a anillo 3.

## L

- **Jerarquía de memoria** — Registros, cachés, RAM, disco: cinco órdenes de magnitud de diferencia. La tabla que explica casi todo el diseño de un kernel. Ver [[Jerarquia-de-memoria]].
- **Latch** — Un bit de memoria sensible al **nivel**: mientras el permiso está alto, la salida sigue al dato. El escalón previo al [[Flip-flop|flip-flop]].
- **Línea de caché** — La unidad de la caché, típicamente 64 bytes. No se lee un byte de memoria: se lee una línea.
- **Little-endian** — Guardar el byte menos significativo primero. Lo que hacen x86 y (en la práctica) aarch64.

## M

- **Máquina virtual** — Una máquina entera hecha de software. Adentro corre un kernel que no sabe que no está solo. Ver [[Maquina-virtual]] y [[QEMU]].
- **Memoria virtual** — El mecanismo de traducción de direcciones. **No** es el swap. Ver [[Memoria-virtual]] y [[Falsos-amigos#7]].
- **Módulo** — Un pedazo de kernel que se carga sin reiniciar, siempre con privilegio completo. Ver [[Modulo-de-kernel]].
- **MCFG** — La tabla de ACPI que dice dónde está la ventana de configuración de PCIe. Sin ella, en aarch64, PCIe es inalcanzable. Ver [[ACPI]].
- **MMIO** (*Memory-Mapped I/O*) — Hablarle a un aparato **escribiendo en direcciones de memoria** que no son memoria. Ver [[MMIO]].
- **Metaestabilidad** — Un flip-flop que queda **entre 0 y 1** porque el dato cambió justo en el flanco. El tiempo que dura no tiene cota garantizada. Ver [[Flip-flop]].
- **MMU** — El pedazo de silicio que traduce direcciones virtuales a físicas leyendo las tablas de páginas. Ver [[MMU]].
- **MSI** (*Message-Signaled Interrupt*) — Una interrupción sin cable: el aparato escribe un dato en una dirección. Ver [[MSI]].

## N

- **NMI** — Interrupción que no se puede tapar. El segundo escalón para cortar a un núcleo que dijo `cli`.
- **`no_std`** — En Rust, compilar sin biblioteca estándar: sin sistema operativo abajo, no hay archivos, hilos ni `malloc`. Todo kernel escrito en Rust es `no_std`.
- **NVMe** — El protocolo de los discos rápidos de hoy. Se maneja con [[NVMe|colas en memoria]], que es el patrón de casi todo el hardware moderno. Ver [[NVMe]].
- **Núcleo** — En este libro, **siempre** un procesador. El programa se llama *kernel*. Ver [[Falsos-amigos#1]].

## O

- **Oops** — Lo que Linux escribe cuando algo se rompió: mata la tarea y sigue, inestable. Un `panic` en cambio detiene todo. Ver [[Oops-y-panic]].

## P

- **Página** — La unidad de la traducción de direcciones; típicamente 4096 bytes. Ver [[Pagina]].
- **`/proc` y `/sys`** — Archivos que no existen en ningún disco: el kernel arma la respuesta cuando los leés. Ver [[Procfs-y-sysfs]].
- **PCIe** — El bus donde vive casi todo lo que se conecta a una máquina moderna. Ver [[PCIe]].
- **PE** — El formato de ejecutable de Windows, y el que pide UEFI. Por eso el target de este kernel es `x86_64-unknown-uefi` y por eso su ABI es la de Windows. Ver [[ELF-y-PE]].
- **PIC** — El controlador de interrupciones viejo de x86 (dos chips 8259 en cascada, 16 líneas). Reemplazado por el APIC, todavía presente por compatibilidad.
- **PSCI** — La interfaz estándar de ARM para pedirle al firmware que **prenda otro núcleo**. En x86 hay que hacerlo a mano con INIT/SIPI y un trampolín. Ver [[40-Arrancar-el-segundo-nucleo]].
- **Privilegio** — De qué es capaz el código que está corriendo. Lo hace cumplir el silicio, no el kernel. Ver [[Modo-privilegiado]].

## R

- **QEMU** — El emulador donde vive este libro. Ver [[QEMU]].
- **`raw`** (modo de `exec`) — Que el código del agente corra con privilegio completo, pudiendo colgar la máquina. Lo declara el agente (D27, P6).
- **Reclamo** (*claim*) — El registro de que el agente pidió un recurso. No es una asignación: el agente elige **cuál**. Ver [[23-Asignadores-y-por-que-aca-no-hay]].
- **Registro** (del procesador) — Un puñado de celdas adentro del procesador. La memoria más rápida y más escasa que existe. Ver [[Registro]].
- **Registro** (de un aparato) — Una dirección que **no es memoria**: leerla y escribirla le habla al aparato. Ver [[MMIO]].
- **Reset vector** — La dirección desde la que el procesador empieza a ejecutar cuando se le da corriente. El primer byte de todo. Ver [[Firmware]].

## S

- **Setup y hold** — El tiempo que un dato tiene que estar quieto antes y después del flanco. De ahí sale el techo de la frecuencia, y de violarlo sale el overclocking inestable. Ver [[Flip-flop]].
- **SGI** (*Software Generated Interrupt*) — El IPI de ARM.
- **Silicio** — El hardware mismo, en oposición a lo que hace el software. Cuando el libro dice "lo hace cumplir el silicio" quiere decir que **no hay forma de esquivarlo escribiendo código**. Ver [[Modo-privilegiado]].
- **SMMU** — El IOMMU de ARM. Se le habla por **colas en memoria**, no por registros como al de Intel. Ver [[IOMMU]].
- **SMP** (*Symmetric MultiProcessing*) — Varios núcleos iguales, todos capaces de correr el kernel. Ver [[40-Arrancar-el-segundo-nucleo]].
- **SPCR** — La tabla de ACPI que dice dónde está la consola serie. Kornelia arranca con una dirección horneada y se **muda** a la que dice SPCR (P4). Ver [[ACPI]].
- **`supervised`** (modo de `exec`) — Que el código del agente corra sin privilegio, en anillo 3 / EL0, de forma que no pueda colgar la máquina. Lo declara el agente (D27).
- **Syscall** — La puerta por la que el código sin privilegio le pide algo al kernel. Ver [[Syscall]].

## T

- **TLB** (*Translation Lookaside Buffer*) — La caché de traducciones de la MMU. Cuando cambiás una tabla de páginas, el procesador **no se entera**: hay que invalidarla a mano. Ver [[TLB]].
- **Tabla de páginas** — La estructura, en memoria, que la MMU recorre para traducir. La escribe el software, la lee el hardware. Ver [[Tabla-de-paginas]].
- **Trampolín** — Un pedazo chico de código cuyo único trabajo es llevar al procesador de un estado a otro. El de x86_64 lleva un núcleo nuevo de 16 a 32 a 64 bits.
- **Trap** — Excepción que la instrucción **pidió** a propósito. Ver [[Falsos-amigos#2]].
- **TSC** (*Time Stamp Counter*) — El contador que ya viene andando en x86. No siempre dice a qué ritmo sube, y ahí hay que [[37-El-reloj-contadores-y-no-saber-la-frecuencia|medirlo]].

## U

- **UART** — El chip del cable serie. El aparato más viejo y más útil: es lo único que un kernel puede usar para hablar antes de saber nada de la máquina. En Kornelia es el **cordón umbilical** (D5). Ver [[UART]].
- **UEFI** — El firmware de las máquinas modernas, reemplazo del BIOS. Trae servicios que dejan de existir en cuanto llamás a `ExitBootServices`. Ver [[UEFI]].
- **`unreported`** — La clase que Kornelia le pone a un rango de memoria que la máquina **nunca dijo qué es**. Alcanzarlo no es enterarse (P4). Ver [[18-Lo-que-la-maquina-no-dice]].

## V

- **Vector** (de interrupción) — El índice en la tabla de handlers. Distinto del número de IRQ. Ver [[Falsos-amigos#12]].
- **`volatile`** — "No optimices este acceso". Obligatorio para MMIO, inútil para concurrencia. Ver [[MMIO]].
- **VSOCK** (*AF_VSOCK*) — Una familia de direcciones para que una máquina virtual hable con su anfitrión **sin pasar por la red**: en vez de IP y puerto, un CID y un puerto. Ver [[Maquina-virtual]].
- **VT-d** — El IOMMU de Intel. Se le habla por registros. Ver [[IOMMU]].

## W

- **`wfi`** (*wait for interrupt*) — El `hlt` de ARM.
- **Width** (*ancho*) — De cuántos bytes es un acceso. Para RAM casi nunca importa; para un registro de aparato, es la diferencia entre andar y no. Ver [[MMIO]].
