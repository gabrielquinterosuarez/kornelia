---
tipo: concepto
estado: pendiente
dificultad: 3
principios: [P1, P2, P3, P4, P6]
decisiones: [D4, D8, D19, D20]
practicas: [P14-Desatar-y-reatar-un-driver, P05-Leer-un-registro-de-un-aparato-de-verdad]
capitulos: [49-Escribir-un-driver, 17-PCIe-buses-funciones-y-BARs, 56-Cada-capa-vacia-es-una-decision]
---

# Driver

> El código que sabe hablarle a **un** [[Aparato|aparato]]: encontrarlo, configurarlo, alcanzar sus registros, atender sus avisos y moverle datos.

No es una capa de abstracción: es un **traductor**. Arriba, una interfaz que se parece a la de todos los demás aparatos de su clase; abajo, el dialecto privado de un chip.

## Qué problema resuelve

El hardware no tiene una interfaz común. Dos SSD del mismo tamaño, del mismo año, con el mismo conector, se manejan escribiendo números distintos en registros distintos en un orden distinto. Y no hay forma de arreglarlo desde el silicio: los aparatos se diseñan por separado, en empresas distintas, años antes de que exista el sistema que los va a usar.

Entonces alguien tiene que saber el dialecto. La pregunta interesante no es *si* hace falta un driver, sino **dónde vive** y **con qué privilegio corre** — y ahí es donde Linux y Kornelia contestan distinto.

## Cómo funciona

Todo driver, de cualquier aparato, hace las mismas cinco cosas. Vale la pena tenerlas como lista porque después se mapean una a una contra lo que ofrece cada sistema.

| # | Qué | Por qué no es trivial |
|---|---|---|
| 1 | **Encontrarlo** | El aparato no avisa. Hay que recorrer un [[Bus|bus]], o leer una tabla, y reconocerlo por algo. |
| 2 | **Configurarlo** | Prenderlo, resetearlo, decirle que puede responder a accesos de memoria y que puede iniciar accesos él mismo. |
| 3 | **Alcanzar sus registros** | Mapearlos como no-cacheables y acceder con el **ancho exacto** ([[MMIO]]). |
| 4 | **Atender sus avisos** | Instalar un [[Handler|handler]] para su [[Interrupcion|interrupción]] — o sondear. |
| 5 | **Moverle datos** | Que el aparato alcance la memoria del sistema por [[DMA|DMA]], que hoy casi siempre es [[NVMe|colas en RAM]]. |

Los pasos 2 y 5 tienen una trampa que aparece siempre: un aparato [[PCIe]] no puede leer memoria hasta que alguien le prende el bit de **bus master** en su espacio de configuración. Sin eso el driver arma las colas perfectas y el aparato no las lee nunca.

## Cómo lo hace Linux

Linux no deja que un driver salga a buscar su aparato. Interpone un **bus**, y el modelo es siempre el mismo triángulo: *bus* ↔ *device* ↔ *driver*.

El driver se registra con una **tabla de IDs** y espera. Cuando el bus enumera algo que coincide, el bus llama a `probe` del driver, pasándole el aparato ya encontrado.

```c
static const struct pci_device_id nvme_id_table[] = {
    { PCI_DEVICE_CLASS(PCI_CLASS_STORAGE_EXPRESS, 0xffffff) },
    { 0, }
};
MODULE_DEVICE_TABLE(pci, nvme_id_table);

static struct pci_driver nvme_driver = {
    .name     = "nvme",
    .id_table = nvme_id_table,
    .probe    = nvme_probe,
    .remove   = nvme_remove,
};
```

`MODULE_DEVICE_TABLE` no es decorativo: graba la tabla en el archivo del [[Modulo-de-kernel|módulo]] para que `depmod` la extraiga, y así `modprobe` pueda **cargar el driver a partir del aparato**. Ese es el camino que hace que enchufar algo cargue su driver solo.

Las cinco cosas de arriba, con sus nombres reales:

| Qué | La función |
|---|---|
| Encontrarlo | lo hace el bus; el driver recibe `probe(struct pci_dev *)` |
| Configurarlo | `pci_enable_device`, `pci_set_master` |
| Registros | `pci_iomap` / `devm_ioremap_resource`, después `readl`/`writel` |
| Interrupciones | `pci_alloc_irq_vectors` (MSI-X) + `request_irq` |
| [[DMA]] | `dma_alloc_coherent`, `dma_map_single`, `dma_set_mask_and_coherent` |
| Soltarlo | `remove`, o automático con las variantes `devm_` |

Para mirarlo desde afuera:

```bash
lspci -k                                  # "Kernel driver in use:" por aparato
ls -l /sys/bus/pci/devices/0000:00:1f.2/driver     # a que driver esta atado
cat /sys/bus/pci/devices/0000:00:1f.2/modalias     # la cadena con la que se busca
ls /sys/bus/pci/drivers/nvme/             # bind, unbind, y los aparatos atados
echo 0000:01:00.0 | sudo tee /sys/bus/pci/drivers/nvme/unbind   # desatarlo en caliente
```

En placas sin PCIe el intermediario es otro (`platform_driver` con un `of_match_table` que compara la cadena `compatible` del [[Device-tree|device tree]]), pero el triángulo es idéntico.

**Lo que el modelo compra:** un driver no repite el descubrimiento, ni la energía, ni el desate en caliente, ni la liberación de recursos. **Lo que cuesta:** el driver corre en el kernel, privilegiado, y no puede hacer nada que el bus no le ofrezca.

## Cómo lo hace Kornelia

| | |
|---|---|
| **Decisiones** | D4 (el kernel no tiene ningún driver salvo el UART), D8 ([[IOMMU]] encendido y vacío), D19/D20 (la distribución es reemplazable) |
| **Principios** | P1 (el kernel nunca es la razón por la que no se puede usar un aparato), P2 (la capa que se saca se deja vacía), P6 (el hardware hace cumplir lo declarado, no una política) |

**No hay modelo de drivers. No hay bus intermediario, ni `probe`, ni tabla de IDs, ni `/dev`.** El único driver que el kernel lleva adentro es el [[UART|cable serie]] — `kernel-x86_64/src/uart.rs:4#y por eso es el único que el kernel lleva adentro (D4)` — y es el único porque es el que hace falta para poder contar que todo lo demás falló.

Lo que el kernel ofrece en vez del modelo son los **once verbos**, y resulta que las cinco cosas de un driver caen una a una:

| Lo que hace un driver | Linux | Kornelia |
|---|---|---|
| Encontrarlo | el bus enumera y llama a `probe` | `describe` dice **dónde se pregunta** (la ventana ECAM de PCIe); el agente recorre el bus él mismo |
| Configurarlo | `pci_set_master` | `mem.write` sobre el espacio de configuración |
| Registros | `pci_iomap` + `readl` | `mem.claim` sobre el [[BAR]], `mem.read`/`mem.write` con `width` |
| Interrupciones | `request_irq` | `irq.install` (`{msi:true}` para los aparatos de hoy), o sondear |
| DMA | `dma_alloc_coherent` | `dma.allow` sobre memoria que el agente ya reclamó |
| Soltarlo | `remove` | `release` |

**El kernel publica dónde se pregunta, no la respuesta** (P4). Es la diferencia entera: un `probe` te entrega el aparato ya reconocido según *el criterio del kernel*; `describe` te entrega la ventana de configuración y el criterio lo pone el agente.

### La demostración concreta: [[NVMe]]

No es una promesa de diseño. Hay un driver de NVMe **escrito con los once verbos y nada más**, en `scripts/client.py:1738#class Nvme`, que arranca un controlador de verdad, le crea colas, lee el bloque 0, trae el resto por DMA directo a un reclamo del agente y salta ahí con `exec`. El portón lo corre en las dos arquitecturas contra un `-device nvme`.

Busca el aparato por su **clase** —`scripts/client.py:1620#NVME_CLASS = (0x01, 0x08, 0x02)`— y no por fabricante y modelo, que es P4 aplicado al bus: el aparato dice *qué hace*, y por eso el mismo driver anda contra cualquier NVMe.

### Dónde corre ese driver, y por qué eso todavía va a cambiar

Hoy corre **del otro lado del cable**: es Python en la máquina del agente, mandando verbos. Eso es cómodo para escribirlo y lentísimo para usarlo — cada lectura de un registro es un viaje de ida y vuelta por el [[UART|cordón umbilical]].

P3 dice qué falta: *el agente no es un participante en tiempo de ejecución, es un **compilador***. El final del camino es que el agente **emita el driver como código máquina** y lo suba con `exec`, para que corra sin él. El cargador de D19 ya hace el último paso — reclama un núcleo y salta ahí— y ya evita el cable donde importa: `scripts/client.py:2122#def read_into` trae los bloques **directo a un reclamo por DMA**, sin que los bytes pasen por el serie.

### Qué se quitó

No hay `probe` ni `remove`, así que no hay ciclo de vida que el kernel administre. No hay tabla de IDs, así que el kernel no opina sobre qué driver le corresponde a qué aparato. No hay `devm_`, así que lo que el agente toma, el agente devuelve con `release`. **Las capas no se reemplazaron: se dejaron vacías** (P2). Ver [[56-Cada-capa-vacia-es-una-decision]].

Lo que **no** se quitó es el permiso: el IOMMU arranca encendido y vacío (D8), así que un driver del agente que se olvide de `dma.allow` no corrompe memoria, no llega. Eso no es un guardarraíl: es P6 — el hardware hace cumplir **lo que el agente declaró**.

> [!warning] Del blob está el mecanismo, no el contenido
> D19 y D20 dicen que los drivers van a vivir en `blob.bin`, la distribución reemplazable que el [[Firmware|firmware]] trae de la partición. Ese mecanismo anda entero: se carga, corre, le habla al kernel y se puede cancelar. Pero **no hay un `blob.bin` en el repo**, ni un driver de red. El único blob que existe es el de prueba que genera `client.py`. Los drivers que nombran D19 y D20 son lo que *va* a ir ahí. Y por eso, hoy, el único transporte es el cordón umbilical: el transporte rápido que D5 le deja al agente todavía no lo escribió nadie.

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| El aparato aparece en el bus pero sus registros devuelven ceros. | Ancho de acceso equivocado; o falta prenderle el bit de *memory space*; o el rango no está mapeado como dispositivo. Ver [[MMIO]]. |
| El driver arma todo bien y el aparato no lee nada. | Falta el bit de **bus master**: no tiene permiso de iniciar accesos. |
| El aparato "lee" las colas y no llega nada. | El IOMMU está encendido y esa memoria no está declarada con `dma.allow` (D8). |
| El aparato existe en `lspci` pero su BAR es inalcanzable. | El BAR cae fuera del mapa que dio el firmware. En aarch64 caen en 512 GiB y el mapa llega a 257: por eso Kornelia mapea y reintenta en vez de contestar `unmapped` (P1). |
| Escribe en memoria que no es suya y se corrompe algo lejos, mucho después. | DMA mal apuntado sin IOMMU. El síntoma aparece en otro lado y en otro momento: es el argumento entero de D8. |
| La interrupción del aparato no llega nunca, aunque el DMA sí. | El MSI es un **pulso** y el GIC lo trataba como nivel. Hay que configurarla por flanco. Ver [[MSI]]. |
| En Linux: `lspci -k` no muestra driver en uso. | Ningún módulo declara ese ID. `modprobe` no tiene de dónde sacarlo si falta `MODULE_DEVICE_TABLE`. |
| Se lee el disco y salen ceros, y parece que anduvo. | Un disco nuevo **es** ceros. Hay que escribir algo reconocible antes de creerle a una lectura. Ver [[58-Una-prueba-que-no-puede-pasar-por-accidente]]. |

## Práctica

- [[P14-Desatar-y-reatar-un-driver]] — *(romper, en la VM)* `unbind` de un driver por `/sys/bus/pci/drivers/`, ver el aparato quedar huérfano en `lspci -k`, y volver a atarlo.
- [[P05-Leer-un-registro-de-un-aparato-de-verdad]] — *(construir)* el mismo registro leído por Linux y por Kornelia con `mem.claim` + `mem.read`.

## Recordar #flashcards/conceptos

¿Cuáles son las cinco cosas que hace todo driver?::Encontrar el aparato, configurarlo, alcanzar sus registros, atender sus interrupciones y moverle datos por DMA. Todo lo demás es cómo cada sistema envuelve esas cinco.

¿Qué es `probe` en Linux?::La función que el **bus** le llama al driver cuando enumeró un aparato que coincide con su tabla de IDs. El driver no sale a buscar: se registra y espera.

¿Para qué sirve `MODULE_DEVICE_TABLE`?::Para grabar la tabla de IDs en el archivo del módulo, que `depmod` la extraiga y `modprobe` pueda cargar el driver **a partir del aparato**. Es lo que hace que enchufar algo cargue su driver solo.

¿Cuántos drivers tiene el kernel de Kornelia y por qué (D4)?::Uno: el UART. Es el único que hace falta para poder contar que todo lo demás falló. Los otros los escribe el agente con los once verbos.

En Kornelia, ¿cómo encuentra el agente un aparato si no hay `probe`?::`describe` le dice **dónde se pregunta** —la ventana de configuración de PCIe— y él recorre el bus. El kernel publica el lugar, no la respuesta (P4).

¿Por qué un driver del agente que se olvida de `dma.allow` no corrompe memoria?::Porque el IOMMU arranca encendido y **vacío** (D8): sin declarar nada, ningún aparato llega a ninguna parte. No es un guardarraíl del kernel: es el hardware haciendo cumplir lo que el agente declaró (P6).

## Ver también

- [[NVMe]] — el driver de verdad escrito con los once verbos, de punta a punta.
- [[Modulo-de-kernel]] — el envase con el que Linux carga y descarga drivers sin reiniciar.
- [[UART]] — el único driver que el kernel lleva adentro, y por qué ese.
- [[Driver]] · [[PCIe]] · [[51-El-blob-y-la-ventana-de-rescate]]
- [[Falsos-amigos#9]] — driver, módulo, firmware y blob no son lo mismo.
