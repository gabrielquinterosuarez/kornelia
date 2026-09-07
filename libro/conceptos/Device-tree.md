---
tipo: concepto
estado: pendiente
dificultad: 3
principios: [P1, P4]
decisiones: [D8, D22, D23, D24]
practicas: [P16-Leer-un-device-tree-y-bootear-sin-ACPI, P01-Preguntarle-a-Linux-que-maquina-es]
capitulos: [16-El-otro-dialecto-device-tree, 15-Enumerar-sin-adivinar-ACPI, 59-Fronteras-verificadas]
---

# Device tree

> El **otro** dialecto en que una máquina se describe: un árbol de nodos con propiedades, en big-endian, donde no hay [[ACPI]].

Resuelve el mismo problema que ACPI y **no se parece en nada**. Es lo que traen las placas ARM y RISC-V embebidas, que son la mayoría de las computadoras que existen.

## Qué problema resuelve

Un servidor x86 se describe con ACPI porque tiene un firmware grande que puede armar tablas y hasta ejecutar AML. Una placa embebida no: su firmware son unos KB en una ROM, no hay enumeración de nada, y el hardware **no se puede descubrir probando** — un controlador de I²C no contesta si nadie lo busca en la dirección exacta.

El camino histórico de ARM en Linux fue horrible y vale contarlo: había un archivo de C **por placa**, con las direcciones escritas a mano, compilado adentro del kernel. Un kernel por modelo de placa, miles de archivos, y ninguno reutilizable.

El device tree corta eso separando **la descripción del hardware** del kernel: la placa viene con un archivo que dice qué tiene y dónde, y un solo binario de kernel sirve para todas. Es P4 llevado al extremo — el kernel no sabe nada de la placa hasta que lee el árbol.

## Cómo funciona

Un árbol. Nodos con nombre, cada uno con propiedades, cada propiedad con un nombre de texto y un valor de bytes crudos.

```
/ {
    #address-cells = <2>;
    #size-cells = <2>;
    cpus {
        cpu@0 { device_type = "cpu"; reg = <0x0 0x0>; };
        cpu@1 { device_type = "cpu"; reg = <0x0 0x1>; };
    };
    pl011@9000000 {
        compatible = "arm,pl011", "arm,primecell";
        reg = <0x0 0x9000000 0x0 0x1000>;
        interrupts = <0x0 0x1 0x4>;
    };
};
```

### Las tres cosas que hay que saber

**1. Es big-endian, siempre.** Aunque la máquina no lo sea. El formato viene de PowerPC y se quedó así. Cada número de 32 bits del archivo hay que darlo vuelta para leerlo.

**2. Tiene dos formas: `.dts` y `.dtb`.** El `.dts` es el texto de arriba, escrito por humanos, con `#include` y todo. El `.dtb` —*flattened device tree*, FDT— es el binario que se le pasa al kernel. Se compilan con `dtc` para los dos lados. El `.dtb` tiene tres bloques:

| Bloque | Qué es |
|---|---|
| Encabezado | Dónde empieza cada bloque y cuánto mide. Empieza con el número mágico `0xd00dfeed`. |
| Estructura | Una secuencia de fichas: "abre nodo", "propiedad", "cierra nodo", "fin". |
| Cadenas | Los nombres de las propiedades, uno atrás de otro. |

Una propiedad **no lleva su nombre**: lleva un desplazamiento al bloque de cadenas. Así `compatible`, que aparece en cada nodo, se guarda una sola vez.

**3. Cuánto mide una dirección lo dice el nodo padre.** En `#address-cells` y `#size-cells`, contando en celdas de 32 bits. Un `reg` es una lista de pares (dirección, tamaño) medidos en esas unidades. **Dar por sentado que son dos y dos anda en [[QEMU]] y falla en la mitad de las placas reales**, que es exactamente la clase de suposición que P4 viene a sacar.

### Cómo se reconoce un aparato

Por la propiedad `compatible`, que es una lista de cadenas pegadas, de la más específica a la más genérica: `"arm,pl011", "arm,primecell"`. El [[Driver|driver]] dice con qué cadenas es compatible y el kernel las cruza. Una placa nueva que reusa un bloque conocido anda sin tocar el kernel.

## Cuál de los dos dialectos se usa **no lo elige el kernel**

Esta es la frase central de la nota. No hay una preferencia de diseño ni una configuración: **es cuál dejó el firmware**. En UEFI los dos llegan por el mismo lugar —la tabla de configuración— cada uno con su GUID, y puede estar uno, el otro, o los dos.

Un kernel que corre en las dos clases de máquina tiene que saber leer los dos formatos, y elegir mirando qué hay. No es portabilidad de cortesía: sin device tree, una placa sin ACPI es una máquina sobre la que el kernel no sabe nada.

## Cómo lo hace Linux

El árbol que la máquina está usando aparece como un **directorio**, donde cada nodo es una carpeta y cada propiedad es un archivo con los bytes crudos:

```bash
ls /sys/firmware/devicetree/base/                     # sólo si la máquina usa DT
cat /sys/firmware/devicetree/base/model; echo
cat /sys/firmware/devicetree/base/compatible | tr '\0' '\n'
xxd /sys/firmware/devicetree/base/cpus/cpu@0/reg      # y ahí se ve el big-endian

sudo apt install device-tree-compiler
dtc -I fs -O dts /sys/firmware/devicetree/base | less  # el árbol vivo, como texto
dtc -I dtb -O dts placa.dtb -o placa.dts               # un archivo suelto
```

En el kernel, el parser vive en `drivers/of/` (*OF* por *Open Firmware*, de donde viene el formato) y las funciones se llaman `of_find_compatible_node`, `of_property_read_u32`, `of_address_to_resource`. Los `.dts` de todas las placas soportadas están en el árbol de fuentes, en `arch/arm64/boot/dts/`.

Un x86 no tiene nada de esto: `/sys/firmware/devicetree/` no existe. Y muchas máquinas ARM de servidor tienen las dos cosas.

## Cómo lo hace Kornelia

| | |
|---|---|
| **Decisiones** | D24 (es formato de la máquina, no del arranque), D8 (el SMMU sale del árbol), D22/D23 (las dos arquitecturas siempre en verde, y verificado) |
| **Dónde vive** | `kernel-core/src/fdt.rs:168#pub unsafe fn read`, `kernel-core/src/tables.rs:122#pub unsafe fn describe` |

El parser es propio y chico, y saca del árbol exactamente lo mismo que `acpi.rs` saca de las tablas, en el mismo vocabulario normalizado: núcleos, controlador de [[Interrupcion|interrupciones]], puerto serie, ventana de PCIe, [[IOMMU]].

- El número mágico se verifica antes de creerle a nada (`kernel-core/src/fdt.rs:43#const MAGIC`).
- `#address-cells` y `#size-cells` **se leen del padre**, no se suponen (`kernel-core/src/fdt.rs:266#"#address-cells"`).
- El controlador se reconoce por `compatible`, y **la versión importa**: `arm,gic-v3` no se programa igual que `arm,gic-400`.
- El cable serie sale del nodo `arm,pl011` (`kernel-core/src/fdt.rs:371#"arm,pl011"`), así que el kernel deja de creerle a la dirección horneada también en una placa sin ACPI.
- El número de una interrupción se calcula, no se copia: vienen de a tres celdas, y un número compartido (SPI) arranca en 32 porque los 32 de abajo se los reserva la arquitectura (`kernel-core/src/fdt.rs:419#unsafe fn spi_of`).
- Los nombres de los nodos que **no** se saben leer se informan igual, como se informan las firmas de ACPI: que exista algo que el kernel todavía no entiende es más útil que callarlo (P4).

La elección entre los dos dialectos está escrita **en un solo lugar** (`kernel-core/src/tables.rs:122#pub unsafe fn describe`): si hay ACPI se usa ACPI, si no, device tree, y si no hay ninguno el kernel lo dice. Vive ahí y no en cada lugar que lo necesita porque la decisión se toma en dos momentos muy separados —al armar el mapa de memoria y al describir el hardware— y **tenerla escrita dos veces es tenerla escrita mal una vez**.

### Y se comprueba, que es lo que lo hace distinto de una intención

El portón bootea aarch64 **una segunda vez, sin ACPI** (`scripts/run-aarch64.sh:42#acpi=off`): entonces el firmware pasa un device tree en su lugar. Y no se conforma con que arranque — le exige llegar hasta el IOMMU contra un [[Aparato|aparato]] que hace [[DMA]] de verdad (`scripts/check.sh:413#--no-acpi`).

Está elegido así a propósito: **el DMA es la prueba que usa todo lo que sale de la descripción junto** —los núcleos, el controlador de interrupciones, dónde se configura PCIe y dónde está el IOMMU—, y encima contra hardware. Si algo saliera mal del árbol, eso no cierra. Es D23 aplicado: una regla que no se comprueba es una intención.

**Qué se quitó:** no hay *overlays* (parches al árbol en caliente, que es como se agrega una placa hija), no hay `/chosen` ni línea de comandos —el kernel no toma parámetros de arranque—, y no se leen reguladores, relojes ni GPIOs. Un árbol real describe cientos de cosas; este kernel lee las cinco que necesita para entregarle la máquina al agente, y lo demás lo nombra sin interpretarlo (P2: la capa se deja vacía).

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| Todos los números salen enormes o absurdos. | Se leyó en little-endian. El device tree es big-endian aunque la máquina no lo sea. |
| Anda en QEMU y falla en una placa real. | Se supusieron dos celdas de dirección y dos de tamaño en vez de leer `#address-cells` y `#size-cells` del padre. |
| La máquina arranca muda en una placa sin ACPI. | No se leyó el nodo del [[UART]]: quedó la dirección horneada, que es de otra placa. |
| El kernel no encuentra el controlador de interrupciones que sí está. | Se buscó una sola cadena `compatible`. GICv2 aparece como `arm,cortex-a15-gic` o `arm,gic-400`, y GICv3 como `arm,gic-v3`. |
| Se encuentra un solo [[Bus|bus]] PCIe en la máquina que tiene 256. | La cuenta de buses se recortó a un byte **antes** de restarle uno, y 256 no entra: daba cero (`kernel-core/src/fdt.rs:390#clamp(1, 256)`). El síntoma aparece justo en la máquina más grande. |
| El parser recorre memoria sin fin. | Un blob corrupto sin fichas de cierre. Por eso hay un tope de profundidad. |
| En Linux, `/sys/firmware/devicetree/` no existe. | La máquina usa ACPI. No es un error: es el otro dialecto. |

## Práctica

- [[P16-Leer-un-device-tree-y-bootear-sin-ACPI]] — *(romper)* `dtc -I fs -O dts` sobre una VM ARM, y después bootear Kornelia con `NO_ACPI=1 ./scripts/run-aarch64.sh` y comparar `describe` con la corrida normal.
- [[P01-Preguntarle-a-Linux-que-maquina-es]] — *(mirar)* comprobar cuál de los dos dialectos usa tu máquina.

## Recordar #flashcards/conceptos

¿Qué problema resolvió el device tree en Linux/ARM?::Que había un archivo de C por modelo de placa, con las direcciones a mano, compilado adentro del kernel. Separar la descripción del hardware deja un solo binario sirviendo para todas.

¿Por qué el device tree es big-endian?::Porque el formato viene de PowerPC (Open Firmware) y se quedó así, aunque la máquina que lo lee no lo sea.

¿Qué son `#address-cells` y `#size-cells`, y por qué no se pueden suponer?::Dicen cuántas celdas de 32 bits mide una dirección y un tamaño en los `reg` de los nodos hijos. Suponer dos y dos anda en QEMU y falla en la mitad de las placas reales.

¿Cómo se reconoce un aparato en el árbol?::Por la propiedad `compatible`, que trae varias cadenas pegadas de la más específica a la más genérica. El mismo bloque reusado en una placa nueva anda sin tocar el kernel.

¿Quién elige entre ACPI y device tree?::No el kernel: **el firmware**, según cuál dejó. Kornelia tiene esa decisión escrita en un solo lugar, porque se toma en dos momentos muy separados del arranque.

¿Cómo se comprueba que el camino de device tree anda de verdad?::Booteando la misma máquina con `acpi=off` y exigiéndole llegar hasta el IOMMU contra un aparato que hace DMA — la prueba que usa todo lo que sale de la descripción junto.

## Ver también

- [[ACPI]] — el otro dialecto, y qué trae cada tabla.
- [[Firmware]] · [[UEFI]] · [[PCIe]]
- [[Device-tree]] · [[59-Fronteras-verificadas]]
