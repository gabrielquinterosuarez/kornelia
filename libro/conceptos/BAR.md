---
tipo: concepto
estado: pendiente
dificultad: 3
principios: [P1, P4]
decisiones: [D12]
practicas: [P14-Medir-un-BAR-escribiendo-unos, P05-Leer-un-registro-de-un-aparato-de-verdad]
capitulos: [17-PCIe-buses-funciones-y-BARs, 18-Lo-que-la-maquina-no-dice, 45-Un-registro-no-es-RAM]
---

# BAR

> El registro donde un [[Aparato|aparato]] dice **cuánto [[Espacio-de-direcciones|espacio de direcciones]] necesita**, y donde el sistema le contesta **dónde se lo puso**. El aparato no elige: se la asignan.

*Base Address Register.* Son seis campos del [[PCIe|espacio de configuración]], en los desplazamientos `0x10` a `0x24`. La confusión típica es leerlos como "la dirección del aparato", como si fuera un dato del fabricante. Es al revés: es una casilla vacía que el aparato sabe cuán grande tiene que ser, y que alguien más llena.

## Qué problema resuelve

Si cada aparato viniera con su dirección de fábrica, dos placas del mismo modelo no podrían convivir, y cualquier aparato nuevo podría chocar con uno viejo. Poner la dirección en un jumper —que es lo que se hacía— traslada el problema a una persona con un manual.

El acuerdo de PCI invierte quién decide. El aparato solo declara **cuánto** y **de qué tipo**; quien enumera reparte el espacio de direcciones de la máquina y le escribe a cada uno dónde le tocó. Un aparato es, desde este punto de vista, una función que se puede reubicar.

## Cómo funciona

Cada BAR son 4 bytes, y los bits de abajo **no son dirección**: son la declaración.

| Bit | Qué dice |
|---|---|
| 0 | `0` = espacio de memoria ([[MMIO]]). `1` = puerto de I/O, que solo existe en x86 y es historia. |
| 2:1 | `00` = la dirección es de 32 bits. `10` = es de **64 bits y ocupa dos ranuras**: la mitad de arriba está en el BAR siguiente. |
| 3 | *Prefetchable*: leer no tiene efectos colaterales y las escrituras se pueden combinar. |
| resto | La dirección base, alineada al tamaño de la región. |

Un BAR de 64 bits que se lee como si fuera de 32 da una dirección **truncada**, que es peor que ninguna: apunta a algún lado. Por eso el recorrido de Kornelia mira esos dos bits antes de seguir: `scripts/client.py:1927#wide = (bar0 & 0x6) == 0x4`.

### Prefetchable no es una optimización menor

Un BAR de registros **nunca** es prefetchable: leer un registro de estado puede vaciar una cola. Un framebuffer de video sí. La diferencia importa porque los puentes usan ese bit para decidir si pueden traer datos por adelantado y juntar escrituras — y porque los puentes tienen ventanas separadas para lo prefetchable y lo que no, así que declararlo mal puede dejar al aparato sin dónde caer.

### Cómo se descubre el tamaño

El aparato no publica cuántos bytes necesita en ningún campo. Se lo descubre **escribiendo todos unos y leyendo de vuelta**: los bits que el aparato no decodifica vuelven en cero.

```text
1. Apagar el bit de "espacio de memoria" en el command  (si no, el aparato se muda mientras)
2. Guardar el valor actual del BAR
3. Escribir 0xFFFFFFFF
4. Leer: vuelve algo como 0xFFFFC000
5. Enmascarar los bits de tipo, invertir, sumar 1  ->  0x4000 = 16 KiB
6. Restaurar el valor guardado
```

Es un procedimiento **destructivo**: entre el paso 3 y el 6 el aparato está respondiendo en otra dirección. Hacerlo con el aparato en uso es una forma prolija de colgar una máquina.

### Quién lo asigna

Normalmente el [[Firmware|firmware]], antes de que arranque el sistema. El kernel puede aceptar lo que encontró o rehacerlo (en Linux, `pci=realloc`). Y **escribir un BAR no prende el aparato**: para que empiece a contestar en esa dirección hay que prender el bit 1 del registro *command*. Son dos pasos y confundirlos es la causa más común de "el aparato está pero no responde".

## Cómo lo hace Linux

Linux lee los BARs en la enumeración y los deja resueltos como *recursos*: rangos con principio, fin y banderas. `lspci -v` los muestra ya interpretados, con el tamaño que descubrió escribiendo unos:

```bash
lspci -v -s 00:01.0
#   Memory at f7c00000 (64-bit, non-prefetchable) [size=16K]
cat /sys/bus/pci/devices/0000:00:01.0/resource   # start, end, flags por línea
grep -i nvme /proc/iomem                          # dónde quedó, en el mapa global
```

Un [[Driver|driver]] **no** lee el BAR a mano: pide el recurso ya resuelto y lo mapea.

```c
pci_request_regions(pdev, "mi-driver");        // reservar, para que nadie más lo tome
resource_size_t base = pci_resource_start(pdev, 0);
resource_size_t len  = pci_resource_len(pdev, 0);
void __iomem *regs   = pci_iomap(pdev, 0, len); // ioremap con el ancho correcto
pci_set_master(pdev);                           // el bit de bus master, para DMA
```

`pci_resource_len` existe porque el tamaño **ya se midió**: nadie repite el truco de los unos en un driver.

## Cómo lo hace Kornelia

| | |
|---|---|
| **Decisiones** | D4 (el agente lee los BARs), D12 (MMIO no cacheable), P1, P4 |
| **Los verbos** | `mem.read` sobre la ventana ECAM para leer el BAR; `mem.claim {at}` para alcanzar lo que apunta |
| **Dónde vive** | `scripts/client.py:1897#def pcie_scan` (del lado del agente), `kernel-core/src/protocol.rs:1166#p.map_device` (del lado del kernel) |

El kernel no sabe qué es un BAR. Lo que sabe es entregar un rango que el agente pidió, y para eso el BAR ya está leído: el agente lo lee del espacio de configuración, le enmascara los bits de tipo, y hace `mem.claim {at: esa_dirección}`.

**Lo que devuelve no es `mmio`, es `unreported`.** Un BAR que el firmware no listó en el mapa cae en un hueco, y ahí el kernel entrega el rango **y** la advertencia de que la máquina nunca dijo qué hay: `kernel-core/src/memory.rs:131#Kind::Unreported`. Alcanzarlo no es enterarse (P4). Ver [[18-Lo-que-la-maquina-no-dice]].

> [!warning] El caso real: un BAR más arriba que el mapa
> En aarch64 los BARs de PCIe caen en **512 GiB**, y el mapa de memoria que da el firmware llega a **257**. El identity map se calcula a partir de ese mapa, así que el rango del controlador [[NVMe]] ni siquiera estaba mapeado: `mem.claim` contestaba `unmapped` y el aparato era **inalcanzable**. O sea, el kernel era la razón por la que no se podía usar un aparato, que es exactamente lo que prohíbe **P1**. La salida no fue agrandar el identity map —512 GiB de tablas por si acaso— sino **mapear y reintentar una vez**: si el reclamo falla por `unmapped` y cae más arriba de lo que las tablas cubren, el kernel mapea ese gigabyte como dispositivo y vuelve a intentar (`kernel-core/src/protocol.rs:1166#p.map_device`, y el registro de lo agregado en `kernel-core/src/paging.rs:73#pub fn note_mapped`). Como dispositivo y no como RAM porque **no se sabe qué hay ahí**; y se sigue entregando como `unreported`, porque haberlo alcanzado no cambia lo que la máquina dijo.

**Qué se quitó:** no hay reasignación de BARs, ni ventanas de puente calculadas por el kernel, ni un asignador de espacio de direcciones. Se acepta lo que dejó el firmware. Si algún día hace falta reasignar, lo hace el agente: tiene `mem.write` sobre la ventana de configuración, que es todo lo que se necesita (P2).

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| El BAR se lee como cero. | El firmware no le asignó nada, o se está leyendo el BAR alto de un aparato de 64 bits como si fuera uno propio. |
| La dirección del BAR apunta a cualquier lado. | Se leyó de 32 bits un BAR de 64. La mitad de arriba está en la ranura siguiente: hay que mirar los bits 2:1. |
| `mem.claim` sobre el BAR devuelve `unmapped`. | El rango cae más arriba de lo que cubren las tablas. Es el bug de aarch64 de arriba; hoy el kernel lo mapea y reintenta. |
| El reclamo sale con clase `unreported` y parece un error. | No lo es: la máquina nunca informó ese rango. Es la respuesta honesta, no una falla (P4). |
| El aparato aparece en el [[Bus|bus]] pero no contesta en su BAR. | Falta prender el bit 1 del *command*. Escribir el BAR no prende nada. |
| Después de medir el tamaño, el aparato desapareció. | Se escribió `0xFFFFFFFF` y no se restauró el valor original. |
| La máquina se cuelga al medir un BAR. | Se lo midió con el aparato respondiendo: durante la medición está mapeado en otra dirección. |
| Los registros se leen bien pero las escrituras no tienen efecto. | El rango quedó cacheable. Un BAR de registros va no cacheable (D12). Ver [[MMIO]]. |

## Práctica

- [[P14-Medir-un-BAR-escribiendo-unos]] — *(romper)* en la VM: apagar el bit de memoria, escribir todos unos, leer el tamaño, restaurar. Y comparar con el `[size=...]` que informa `lspci -v`.
- [[P05-Leer-un-registro-de-un-aparato-de-verdad]] — *(construir)* llegar hasta el primer registro del aparato desde el BAR, en Linux y en Kornelia.

## Recordar #flashcards/conceptos

¿Qué es un BAR?::Un campo del espacio de configuración de PCI donde el aparato declara cuánto espacio de direcciones necesita y de qué tipo, y donde quien enumera le escribe **dónde** se lo asignó. El aparato no elige la dirección.

¿Cómo se descubre el tamaño de un BAR?::Escribiéndole todos unos y leyendo de vuelta: los bits que el aparato no decodifica vuelven en cero. Se enmascaran los bits de tipo, se invierte y se suma uno. Hay que restaurar el valor original después.

¿Qué pasa si leés de 32 bits un BAR de 64?::Te llevás la mitad de abajo y una dirección truncada, que apunta a otro lado. Los bits 2:1 en `10` avisan que la mitad de arriba está en la ranura siguiente.

¿Qué significa que un BAR sea *prefetchable*?::Que leerlo no tiene efectos colaterales y las escrituras se pueden combinar, así que un puente puede traer datos por adelantado. Un BAR de registros nunca lo es; un framebuffer sí.

¿Por qué en aarch64 el NVMe era inalcanzable?::Porque sus BARs caen en 512 GiB y el mapa del firmware llega a 257, así que el identity map no los cubría y `mem.claim` devolvía `unmapped`. El kernel era la razón por la que no se podía usar un aparato (P1). Ahora mapea y reintenta.

¿Alcanza con escribir el BAR para que el aparato responda?::No. Hay que prender además el bit 1 del registro *command* ("respondé a accesos de memoria"). Son dos pasos.

## Ver también

- [[PCIe]] — dónde vive el BAR y cómo se llega a leerlo.
- [[MMIO]] — qué son las direcciones que el BAR entrega, y las tres reglas que no valen para la RAM.
- [[NVMe]] — el aparato cuyo BAR desató el arreglo de arriba.
- [[Falsos-amigos#10]] — asignar, reclamar, mapear y reservar no son sinónimos.
