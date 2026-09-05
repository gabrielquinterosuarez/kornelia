---
tipo: concepto
estado: pendiente
dificultad: 2
principios: [P1, P4, P5]
decisiones: [D12]
practicas: [P01-Preguntarle-a-Linux-que-maquina-es, P05-Leer-un-registro-de-un-aparato-de-verdad]
capitulos: [04-El-bus-tocar-algo-que-no-es-memoria, 45-Un-registro-no-es-RAM]
---

# MMIO

> Hablarle a un [[Aparato|aparato]] **escribiendo en direcciones que no son memoria**. La dirección existe, el chip de RAM no.

*Memory-Mapped I/O.* Es el mecanismo por el que un kernel controla absolutamente todo el hardware moderno, y la idea entera cabe en una frase: algunas direcciones, en vez de llegar a la RAM, llegan a un aparato.

## Qué problema resuelve

Un procesador sabe hacer dos cosas con el mundo exterior: leer de una dirección y escribir en una dirección. Si le quisieras agregar una forma nueva de hablarle a cada clase de aparato, harían falta instrucciones nuevas por aparato — y el juego de instrucciones es silicio, no se puede extender.

La solución es no agregar nada: **reusar las direcciones**. El aparato se hace pasar por memoria. Escribir un 1 en cierta dirección es apretar un botón del aparato; leer otra es mirar su tablero.

x86 tiene además un mecanismo aparte y anterior —los **puertos de I/O**, con instrucciones propias `in` y `out` y un espacio de 65.536 direcciones separado— que es de dónde viene el famoso `0x3F8` del [[UART|cable serie]]. Es historia: todo lo nuevo es MMIO, y ARM y RISC-V nunca tuvieron puertos.

## Cómo funciona

```mermaid
flowchart LR
    CPU[Procesador<br/>mov [0xC0001000], 1] --> MMU[MMU<br/>traduce virtual→física]
    MMU --> BUS{¿Quién responde<br/>a esta dirección?}
    BUS -->|0x0–0x7FFFFFFF| RAM[Chips de RAM]
    BUS -->|0xC0000000–...| DEV[Un aparato PCIe<br/>en su BAR]
    BUS -->|nadie| ERR[Abort / ceros / basura]
```

Quién responde a cada rango lo decide un árbol de ruteo del [[Bus|bus]], y **quién lo configura es el [[Firmware|firmware]] o el kernel**, escribiendo los [[PCIe|BARs]] del aparato. Un aparato no elige su dirección: se la asignan.

La rama de abajo es la que hace daño: si nadie responde, lo que pasa **depende de la arquitectura**, y eso está en la sección de cómo se rompe.

## Las tres reglas que no valen para la RAM

Un registro de aparato se parece a memoria y se comporta distinto en tres cosas. Las tres producen bugs que no se ven venir.

### 1. No se puede cachear

Si la escritura se queda en la [[Cache|caché]], nunca llega al aparato. Si la lectura sale de la [[Cache|caché]], devuelve la copia vieja en vez del valor de ahora — y el valor de ahora es justamente el punto: un registro de estado **cambia solo**.

Entonces el mapeo se marca como "no cacheable" o "dispositivo" en la [[Tabla-de-paginas|tabla de páginas]]. En Kornelia eso es D12 y vive en `kernel-x86_64/src/paging.rs:68#pub unsafe fn map_device`; el comentario de al lado (`kernel-x86_64/src/paging.rs:54#PCD: cache disable`) explica qué bits se prenden.

### 2. Leer tiene efecto

En RAM, leer dos veces da lo mismo y no cambia nada, así que el compilador puede borrar la segunda lectura o reordenarla. En un aparato, leer un registro puede **vaciar una cola** o **borrar una bandera de interrupción**. Por eso el acceso va marcado `volatile`: "esto ocurre exactamente las veces que escribí y en el orden que escribí".

`volatile` **no** sirve para concurrencia entre núcleos; para eso hacen falta [[42-Ordenamiento-de-memoria|barreras y atómicos]]. Confundir las dos cosas es clásico.

### 3. El ancho importa

Y esta es la que más caro sale. Muchos registros **solo aceptan accesos de su ancho exacto**: un registro de 4 bytes leído de a un byte no devuelve el primer byte, devuelve cualquier cosa — o mata la máquina.

Por eso `mem.read` y `mem.write` de Kornelia toman `width`, y por eso los accesos crudos tienen esa firma: `kernel-x86_64/src/guarded.rs:113#pub unsafe fn read`.

## Cómo lo hace Linux

Un [[Driver|driver]] **no** desreferencia un puntero: pide el mapeo y usa funciones específicas.

```c
void __iomem *base = ioremap(bar_addr, bar_len);   // mapea, no-cacheable
u32 status = readl(base + 0x04);                    // lectura de 4 bytes, volatile
writel(1, base + 0x08);                             // escritura de 4 bytes
iounmap(base);
```

`readl`/`writel` (*long* = 4 bytes; hay `readb`, `readw`, `readq`) existen para que **el ancho esté en el nombre de la función** y no se pueda equivocar por accidente. Y el tipo `__iomem` hace que el verificador estático se queje si alguien intenta usar ese puntero como memoria normal. Las dos son defensas contra las tres reglas de arriba.

Desde el espacio de usuario se puede ver el mapa (`sudo cat /proc/iomem`) y, con permiso, tocarlo por `/sys/bus/pci/devices/*/resource0`. Ver [[P01-Preguntarle-a-Linux-que-maquina-es]].

## Cómo lo hace Kornelia

El kernel **no tiene drivers** (D4): quien toca los registros es el agente. Así que el kernel no ofrece `ioremap` ni `readl`, ofrece que el agente alcance el rango y lo lea con el ancho que él diga.

| | |
|---|---|
| **Decisiones** | D4 (el agente escribe sus drivers), D12 (MMIO no cacheable), P1, P4, P5 |
| **Los verbos** | `mem.claim` para el rango, `mem.read`/`mem.write` con `width`, o `exec` con código propio |
| **Dónde vive** | `kernel-x86_64/src/paging.rs:68#pub unsafe fn map_device`, `kernel-x86_64/src/guarded.rs:113#pub unsafe fn read` |

Tres cosas de este kernel salen directo de que MMIO no es RAM:

1. **La clase `unreported`.** Un rango que cae en un hueco del mapa —donde quedan los [[BAR|BARs]] que el firmware no listó— se entrega con esa clase, que **no es `mmio`** (`kernel-core/src/memory.rs:131#Kind::Unreported`). El agente se lleva el rango **y** la advertencia de que la máquina nunca dijo qué hay ahí. Alcanzarlo no es enterarse (P4).
2. **Se mapea aunque esté fuera del mapa.** Si el rango cae más arriba de lo que las tablas cubren, el kernel lo mapea y reintenta en vez de contestar `unmapped`. No es comodidad: en aarch64 los BARs de [[PCIe]] caen en 512 GiB y el mapa del firmware llega a 257, así que el controlador [[NVMe]] era **inalcanzable** — o sea, el kernel era la razón por la que no se podía usar un aparato, que es exactamente lo que prohíbe P1.
3. **Un acceso rechazado no mata al kernel.** `mem.read`/`mem.write` corren en el camino del protocolo, y ahí no había punto de recuperación. Ahora van con el mismo que usa `exec`, armado alrededor de **una sola instrucción**, y el rechazo vuelve como `access-refused` con la dirección que cortó (P5). Ver [[MMIO]].

## Cómo se ve roto

> [!danger] El mismo error, dos síntomas opuestos
> Un registro que solo acepta lecturas de 4 bytes, leído de a uno:
> - en **x86_64** devuelve **ceros, en silencio** — se ve como si el aparato no estuviera;
> - en **aarch64** lo rechaza el bus con un abort externo que **dejaba la máquina muda**. El mismo pedido: en una arquitectura miente, en la otra mata. Es el mejor argumento concreto para D22/D23 —las dos arquitecturas siempre en verde—: no es portabilidad, es que cada una revela lo que la otra esconde.

| Síntoma | Causa |
|---|---|
| El registro devuelve ceros. | Ancho equivocado; o el rango no está mapeado; o está mapeado como cacheable. |
| La escritura no tiene efecto. | Se quedó en la caché o en el buffer de escritura: falta marcar no-cacheable, o falta una barrera. |
| Anda una vez y después no. | Se leyó un registro que se limpia al leerse, dos veces. O el compilador borró un acceso por falta de `volatile`. |
| La máquina se queda muda al tocar un aparato. | Abort del bus en aarch64. Ver arriba. |
| El aparato existe en `lspci` pero su BAR es inalcanzable. | La ventana de configuración o el BAR no están en el mapa que dio el firmware. |

## Práctica

- [[P01-Preguntarle-a-Linux-que-maquina-es]] — *(mirar)* `/proc/iomem`, `lspci -v`, y encontrar un BAR de verdad.
- [[P05-Leer-un-registro-de-un-aparato-de-verdad]] — *(construir)* leer el mismo registro en Linux y en Kornelia, y equivocarle el ancho a propósito para ver los dos síntomas.

## Recordar #flashcards/conceptos

¿Qué es MMIO en una frase?::Que algunas direcciones, en vez de llegar a la RAM, llegan a un aparato: leerlas y escribirlas le habla al hardware.

¿Por qué el MMIO no se puede cachear?::Porque una escritura que queda en la caché no llega al aparato, y una lectura desde la caché devuelve la copia vieja en vez del valor actual de un registro que cambia solo.

¿Por qué `readl`/`writel` de Linux tienen el ancho en el nombre?::Porque muchos registros solo aceptan accesos de su ancho exacto, y tener el ancho en el nombre de la función hace imposible equivocarse por accidente.

Leer un registro de 4 bytes de a un byte: ¿qué pasa en x86 y qué en ARM?::En x86 devuelve ceros en silencio, como si el aparato no estuviera. En ARM el bus lo rechaza con un abort externo que puede dejar la máquina muda. El mismo pedido: en una miente, en la otra mata.

¿Qué significa la clase `unreported` de Kornelia?::Que el rango se entrega alcanzable pero la máquina **nunca dijo qué hay ahí**. No es `mmio`: alcanzarlo no es enterarse (P4).

¿`volatile` sirve para sincronizar dos núcleos?::No. Sirve para que el compilador no borre ni reordene un acceso a un aparato. Para concurrencia hacen falta barreras y atómicos.

## Ver también

- [[MMIO]] · [[PCIe]] · [[DMA]]
- [[Falsos-amigos#4]] — MMIO no es [[DMA]]: en MMIO el procesador va al aparato; en DMA el aparato va a la memoria.
