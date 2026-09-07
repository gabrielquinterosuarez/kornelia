---
tipo: concepto
estado: pendiente
dificultad: 1
principios: [P1, P4, P6]
decisiones: [D4, D8]
practicas: [P01-Preguntarle-a-Linux-que-maquina-es]
capitulos: [04-El-bus-tocar-algo-que-no-es-memoria, 17-PCIe-buses-funciones-y-BARs, 49-Escribir-un-driver]
---

# Aparato

> Todo lo que hay en la máquina que **no es el procesador ni la memoria**, y con lo que el procesador puede hablar.

Es la palabra que este libro usa 500 veces y que no estaba definida en ninguna parte. En la literatura en español se dice más **dispositivo**; acá se dice *aparato* porque es más corto y suena a lo que es: una cosa física con la que hay que arreglárselas. Son sinónimos exactos, y en inglés los dos son *device*.

## Qué problema resuelve la palabra

No es un problema técnico: es que hace falta **un nombre para "el resto"**. Un kernel administra tres cosas —tiempo, memoria y aparatos— y las dos primeras son homogéneas: un núcleo es como otro núcleo, un byte de RAM es como otro byte. Los aparatos no se parecen entre sí en nada. Un disco, una placa de red, un chip de temperatura y un reloj no tienen ninguna propiedad en común… salvo **cómo se los alcanza**.

Y eso es lo que hace útil a la palabra: aparato no es una categoría de cosas, es una categoría de **relación con el procesador**.

## Qué hace que algo sea un aparato

Un aparato es cualquier cosa que hace una o más de estas cuatro cosas. Ninguna otra propiedad importa.

| | Qué es | Concepto |
|---|---|---|
| **1. Ocupa direcciones** | Tiene *registros*: direcciones que no son memoria, y escribirlas o leerlas le habla. | [[MMIO]], [[BAR]] |
| **2. Puede avisar** | Interrumpe al procesador cuando le pasa algo, en vez de esperar a que le pregunten. | [[Interrupcion]], [[MSI]] |
| **3. Puede tocar la memoria solo** | Lee y escribe RAM por su cuenta, sin que el procesador copie nada. | [[DMA]], [[IOMMU]] |
| **4. Se puede descubrir** | Está en un bus que se enumera, o la máquina lo declara en sus tablas. | [[PCIe]], [[ACPI]], [[Device-tree]] |

**Y casi ningún aparato hace las cuatro.** Eso es lo interesante:

| Aparato | Registros | Avisa | DMA | Se descubre |
|---|---|---|---|---|
| [[UART]] (el cable serie) | sí | sí | **no** | por tabla, no por bus |
| Un pin de GPIO | sí | a veces | no | por tabla |
| [[NVMe]] (un disco) | sí | sí | **sí** | por [[PCIe]] |
| Una placa de red | sí | sí | sí | por PCIe |
| El controlador de interrupciones | sí | *él es el que avisa* | no | por tabla |
| El [[IOMMU]] | sí | sí (cuando bloquea algo) | no | por tabla |

Mirá la fila del UART: **no hace DMA y no está en ningún bus enumerable**. Por eso es el aparato con el que se arranca cualquier kernel — es el único al que se le puede hablar sabiendo una sola dirección y nada más.

Y mirá las dos últimas filas: el controlador de interrupciones y el IOMMU **son aparatos**, con sus registros y todo. No se sienten como aparatos porque no se pueden desconectar, pero desde el procesador son exactamente lo mismo que un disco: direcciones que responden.

> [!tip] La consecuencia que ordena media biblioteca
> **Todo lo que un kernel hace con el hardware pasa por esos cuatro canales.** Un [[Driver|driver]] no es más que código que usa alguno de los cuatro. Cuando leas la especificación de un aparato que no conocés, buscá las cuatro respuestas —qué registros tiene, cómo avisa, si hace DMA, cómo se lo encuentra— y ya sabés casi todo.

## Qué NO es un aparato

- **El procesador.** Es el que habla, no el hablado.
- **La [[Cache|caché]], la [[MMU]], el [[TLB]].** Son partes del procesador; no tienen registros que se alcancen por el bus, se manejan con instrucciones y registros de control.
- **La RAM.** Responde a direcciones, sí, pero no hace nada por su cuenta: no avisa, no inicia transferencias, no se descubre en un bus. Es memoria, que es la otra categoría.
- **El [[Firmware|firmware]].** Es código, no un aparato — aunque *corre* en la máquina y *configura* aparatos.

El límite se pone difuso en el borde y no importa: la palabra sirve para pensar, no para clasificar.

## Cómo lo hace Linux

Linux modela los aparatos como un **árbol**, y lo publica entero:

```bash
ls /sys/bus/                       # los buses: pci, usb, i2c, platform...
ls /sys/bus/pci/devices/           # los aparatos de PCIe
ls /sys/devices/                   # el arbol fisico completo
lspci -v                           # los de PCIe, legible
lsusb                              # los de USB
sudo cat /proc/iomem               # canal 1: quien ocupa que direcciones
cat /proc/interrupts               # canal 2: quien esta avisando, y cuanto
ls /sys/kernel/iommu_groups/       # canal 3: quien puede tocar la memoria
```

Esos cuatro últimos comandos son, uno por uno, los cuatro canales de la tabla de arriba. **Los aparatos que no están en ningún bus** —el UART, el reloj, el controlador de interrupciones— aparecen en `/sys/bus/platform/devices/`, que es el bus de los que no tienen bus: existe justamente porque el modelo necesitaba un lugar para los que la máquina declara en sus tablas en vez de enumerarse.

## Cómo lo hace Kornelia

Acá la palabra tiene una consecuencia directa, y es de las más fuertes del proyecto: **el kernel no tiene drivers** (D4). No sabe qué es un disco. Lo único que hace es entregarle al agente los cuatro canales, uno por verbo:

| Canal | Verbo | Dónde vive |
|---|---|---|
| Registros | `mem.claim` + `mem.read`/`mem.write` con `width` | `kernel-core/src/protocol.rs:276#"mem.claim"` |
| Avisos | `irq.install` | `kernel-core/src/protocol.rs:283#"irq.install"` |
| DMA | `dma.allow` | `kernel-core/src/protocol.rs:285#"dma.allow"` |
| Descubrimiento | `describe {what:["pcie","tables"]}` | [[Procfs-y-sysfs]] tiene el contraste |

Visto así, **la superficie de once verbos deja de parecer arbitraria**: son los cuatro canales de un aparato, más memoria, más núcleos, más ejecutar código. No hay nada más que un kernel *tenga* que dar.

Y el canal 3 es el único que arranca **cerrado**: el [[IOMMU]] va encendido y vacío (D8), así que un aparato no llega a la memoria hasta que el agente lo declara. No es un guardarraíl — es que el hardware haga cumplir lo que el agente dijo (P6).

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| El aparato aparece en `lspci` pero sus registros dan ceros o cuelgan. | Canal 1 roto: el [[BAR]] no está mapeado, o se lee con el ancho equivocado. Ver [[MMIO]]. |
| El aparato "no hace nada" y no hay error. | Canal 2: la interrupción no llega, así que nadie recoge el resultado. El aparato quizá terminó su trabajo hace rato. |
| El aparato hace su trabajo y la memoria queda intacta. | Canal 3: el IOMMU bloqueó el DMA. Y ojo — eso se ve **idéntico** a que el DMA nunca haya ocurrido. |
| El aparato no aparece en ningún lado. | Canal 4: el bus no se enumeró, o la ventana de configuración no está en el mapa. En aarch64 eso pasó de verdad: sin la tabla MCFG, [[PCIe]] era inalcanzable. |

Que los síntomas se ordenen por canal no es casualidad: **si algo no funciona con un aparato, es uno de los cuatro**. Es la primera pregunta que conviene hacerse.

## Práctica

- [[P01-Preguntarle-a-Linux-que-maquina-es]] — *(mirar)* recorrer los cuatro canales de un aparato de verdad de tu máquina, y llenar la tabla con sus números.

## Recordar #flashcards/conceptos

¿Qué es un aparato?::Todo lo que hay en la máquina que no es el procesador ni la memoria y con lo que el procesador puede hablar. No es una categoría de cosas: es una categoría de relación con el procesador.

¿Cuáles son los cuatro canales que definen a un aparato?::Ocupa direcciones (registros/MMIO), puede avisar (interrupciones), puede tocar la memoria solo (DMA), y se puede descubrir (bus enumerable o tablas del firmware).

¿Por qué el UART es el aparato con el que arranca cualquier kernel?::Porque no hace DMA y no está en ningún bus enumerable: se le puede hablar sabiendo una sola dirección y nada más, antes de saber cualquier otra cosa de la máquina.

¿Es la RAM un aparato?::No. Responde a direcciones, pero no hace nada por su cuenta: no avisa, no inicia transferencias y no se descubre en un bus. Es la otra categoría que administra un kernel.

¿Por qué los once verbos de Kornelia no son un número arbitrario?::Porque son los cuatro canales de un aparato (registros, avisos, DMA, descubrimiento) más memoria, núcleos y ejecutar código. El kernel no tiene drivers (D4): entrega los canales y el agente escribe el driver.

Un aparato no aparece en `lspci`. ¿Qué canal está roto?::El cuarto, el de descubrimiento: el bus no se enumeró o la ventana de configuración no está en el mapa de memoria.

## Ver también

- [[Falsos-amigos#13]] — aparato, dispositivo, periférico y **controlador**, que en español significa dos cosas.
- [[MMIO]] · [[Interrupcion]] · [[DMA]] · [[PCIe]] · [[Driver]]
- [[Bus]] — el camino por el que se lo alcanza.
