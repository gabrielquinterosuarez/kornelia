---
tipo: practica
clase: mirar
contra: ambos
estado: pendiente
riesgo: necesita-root
conceptos: [MMIO, Registro]
---

# P01 · Preguntarle a Linux qué máquina es

> [!success] Qué vas a ver si funciona
> La misma información, dos veces: una vez como Linux la publica y otra como Kornelia la contesta. Al final vas a tener una tabla llena con **los números de tu máquina**, y vas a poder señalar en `/proc/iomem` un rango que no es memoria sino un [[Aparato|aparato]].

Todo esto es de solo lectura. Corre en tu Debian sin riesgo.

---

## Parte 1 · El procesador

```bash
lscpu
```

Llená esto con lo que salió:

| | Tu máquina |
|---|---|
| Modelo | |
| Núcleos físicos / lógicos | |
| Frecuencia | |
| Tamaño de L1d, L1i, L2, L3 | |
| Tiene `vmx` o `svm` en las banderas *(virtualización)* | |
| Tiene `nx` *(páginas no ejecutables)* | |

```bash
grep -c ^processor /proc/cpuinfo      # nucleos logicos
lscpu -C                              # las caches, en una tabla
```

**Qué estás viendo:** la columna `COHERENCY-SIZE` de `lscpu -C` es el tamaño de la línea de [[Cache|caché]], casi siempre 64 bytes. Ese número es la unidad real en la que tu máquina mueve memoria: no existe leer un byte. Ver [[05-Caches-y-la-primera-mentira-util]].

---

## Parte 2 · El mapa de memoria física

```bash
sudo cat /proc/iomem | head -40
sudo cat /proc/iomem | grep -i "System RAM"
sudo cat /proc/iomem | grep -iE "0000:[0-9a-f]{2}:"    # los rangos que son aparatos
```

| | Tu máquina |
|---|---|
| ¿Dónde arranca el primer `System RAM`? | |
| ¿Es la RAM un bloque continuo o hay huecos? | |
| Un rango marcado `Reserved` | |
| Un rango que pertenece a un aparato [[PCIe]] | |

> [!question] La pregunta de esta parte
> Sumá los rangos de `System RAM`. ¿Da lo que dice `free -h`? **No va a dar**, y la diferencia es lo que se quedó el [[Firmware|firmware]], lo que reserva el kernel y los agujeros del mapa. Un kernel no puede suponer que "la RAM" es un número: tiene que leer el mapa. Es la razón de ser de `describe {what:["memory"]}` y de que en Kornelia el agente [[23-Asignadores-y-por-que-aca-no-hay|reclame rangos]] en vez de pedir cantidades.

---

## Parte 3 · Encontrar un aparato y su BAR

```bash
lspci
lspci -v | head -30
```

Elegí uno que te interese (la placa de red, el controlador [[NVMe]]) y anotá su dirección de [[Bus|bus]], del estilo `00:1f.6`:

```bash
DEV=00:1f.6                          # cambialo por el tuyo
lspci -v -s $DEV
sudo lspci -xxx -s $DEV | head -5    # los primeros 64 bytes del espacio de configuracion
ls -l /sys/bus/pci/devices/0000:$DEV/
cat /sys/bus/pci/devices/0000:$DEV/resource
```

| | Tu aparato |
|---|---|
| Qué es | |
| Su `Memory at ...` *(el [[BAR]])* | |
| Cuántos bytes ocupa | |
| Su IRQ, y si dice `MSI` | |

**Qué estás viendo:** ese `Memory at f7d00000 (32-bit, non-prefetchable) [size=128K]` es [[MMIO]]: 128 KiB de direcciones que **no son memoria**. Buscá esa misma dirección en `/proc/iomem` y vas a encontrarla atribuida al aparato. Ver [[17-PCIe-buses-funciones-y-BARs]].

Los bytes de `lspci -xxx` son **el mismo formato** que lee Kornelia: los definió la especificación de PCIe, no Linux. Los primeros cuatro bytes son *vendor ID* y *device ID*: `8086` es Intel, y está guardado al revés (little-endian), así que en el volcado lo vas a ver como `86 80`.

---

## Parte 4 · Las interrupciones, en vivo

```bash
cat /proc/interrupts
```

Ahora, en dos terminales:

```bash
# terminal 1
watch -n0.5 'grep -iE "i8042|xhci|nvme|eth|wlan" /proc/interrupts'
# terminal 2: mové el mouse, tocá el teclado, copiá un archivo grande
```

**Qué estás viendo:** el número más difícil de visualizar de todo el libro convertido en un contador que sube. Cada unidad es un aparato que **paró al procesador** para que lo atendiera.

| | Tu máquina |
|---|---|
| ¿Cuántas filas dicen `PCI-MSI`? | |
| ¿Cuántas dicen `IO-APIC`? | |
| ¿Qué fila sube al mover el mouse? | |
| ¿Se reparten entre núcleos o van todas al 0? | |

La proporción de `PCI-MSI` contra `IO-APIC` te dice cuántos de tus aparatos **ya no tienen cable**. Ver [[35-MSI-interrupciones-sin-cable]].

---

## Parte 5 · Cómo se describe tu máquina

```bash
ls /sys/firmware/acpi/tables/
sudo cp /sys/firmware/acpi/tables/DSDT /tmp/dsdt.dat
iasl -d /tmp/dsdt.dat && wc -l /tmp/dsdt.dsl && head -40 /tmp/dsdt.dsl
```

**Qué estás viendo:** el `DSDT` decompilado son **miles de líneas de un lenguaje de programación** (AML) embebido en las tablas de tu firmware. Mirá el tamaño: eso es lo que un kernel tendría que interpretar para averiguar, por ejemplo, qué cable de interrupción le toca a un aparato. Es exactamente la razón por la que Kornelia **no implementa INTx** y usa [[MSI]], que lo hace innecesario.

Y las que sí lee Kornelia:

```bash
ls /sys/firmware/acpi/tables/ | grep -E "APIC|MCFG|SPCR|DMAR"
```

| Tabla | Para qué la usa un kernel | ¿La tenés? |
|---|---|---|
| `APIC` (MADT) | Cuántos núcleos hay y cómo despertarlos | |
| `MCFG` | Dónde está la ventana de configuración de PCIe | |
| `SPCR` | Dónde está la consola serie | |
| `DMAR` | Dónde está el [[IOMMU]] de Intel | |

`SPCR` es la que hace que Kornelia **se mude** del [[UART]] horneado al que dice la máquina (P4). Si tu máquina no la tiene, es porque tiene pantalla y no le hace falta.

---

## Parte 6 · Lo mismo, contra Kornelia

```bash
cd ~/Proyectos/kornelia
export PATH="$HOME/.cargo/bin:$PATH"

./scripts/client.py --what memory
./scripts/client.py --what cpus
./scripts/client.py --what pcie
./scripts/client.py --what interrupts
./scripts/client.py --what clock
./scripts/client.py --what tables
./scripts/client.py --what claims
```

Y la comparación, que es el punto de la práctica:

| Pregunta | Linux | Kornelia | ¿En qué se parecen las respuestas? |
|---|---|---|---|
| Mapa de memoria | `/proc/iomem` | `--what memory` | |
| Núcleos | `lscpu` | `--what cpus` | |
| Aparatos | `lspci` | `--what pcie` | |
| [[Interrupcion|Interrupciones]] | `/proc/interrupts` | `--what interrupts` | |
| Tablas del firmware | `/sys/firmware/acpi/tables/` | `--what tables` | |

> [!question] Las dos preguntas para las que **no hay** equivalente
> - En Linux no existe `--what claims`: no hay una pregunta que devuelva "quién es dueño de qué memoria", porque la respuesta es siempre "el kernel". En Kornelia es una pregunta legítima. ¿Por qué? Ver [[23-Asignadores-y-por-que-aca-no-hay]].
> - En Kornelia no existe `/proc/interrupts` **con contadores históricos por núcleo**. ¿Debería? Anotalo como duda si te parece que sí; es exactamente la clase de hueco que este libro quiere discutir.

Y una que solo tiene sentido acá:

```bash
./scripts/client.py --what cable
```

Eso trae, entre otras cosas, **el contador de bytes que el cable perdió**. Existe porque hubo un bug donde el buffer del serie era más chico que el pedido más grande y **descartaba en silencio**: el contador estaba, pero nadie podía verlo. Ahora se publica y el portón exige que sea cero. Ver [[Indice-de-sintomas]].

---

## Qué mirar cuando no sale

| Síntoma | Causa probable |
|---|---|
| `/proc/iomem` sale todo en ceros | Falta `sudo`. Sin permiso el kernel te da la estructura pero no las direcciones. |
| `iasl: command not found` | `sudo apt install acpica-tools`. |
| `client.py` no contesta | ¿Está el kernel corriendo? Necesita `./scripts/run-x86_64.sh` en otra terminal, o dejá que `client.py` arranque su propio [[QEMU]]. |
| `cargo: command not found` | `export PATH="$HOME/.cargo/bin:$PATH"`. |
| `/sys/firmware/acpi` no existe | La máquina arrancó por BIOS legacy, o es una VM sin [[ACPI]]. Ahí la descripción vendría por [[16-El-otro-dialecto-device-tree|device tree]]. |

## Anotaciones

*(Tuyas. Guardá la tabla llena: los números de **tu** máquina son la referencia con la que vas a leer el resto del libro.)*
