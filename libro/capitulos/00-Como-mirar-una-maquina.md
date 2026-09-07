---
tipo: capitulo
parte: 0
estado: pendiente
dificultad: 2
conceptos: []
practicas: [P00-Armar-la-VM-de-practicas, P01-Preguntarle-a-Linux-que-maquina-es]
---

# 00 · Cómo mirar una máquina

> [!abstract] Al terminar este capítulo vas a poder
> - Preguntarle a Linux qué hardware tiene, sin adivinar y sin instalar nada raro.
> - Abrir un binario y ver las instrucciones que tiene adentro.
> - Ver, desde afuera, los [[Registro|registros]] del procesador de una [[Maquina-virtual|máquina virtual]] congelada.
> - Depurar algo que se quedó mudo, que es el estado normal de un kernel roto.

Este capítulo va primero porque **todos los demás lo usan**. Un capítulo de sistemas sin herramientas produce una creencia: leés que la [[MMU]] traduce direcciones, asentís, y no cambia nada. Con herramientas produce un número en la pantalla que no estaba antes.

Hay una asimetría que conviene aceptar temprano: **Linux tiene una superficie enorme para mirarse a sí mismo** —`/proc`, `/sys`, `perf`, `ftrace`, `bpftrace`— y Kornelia tiene exactamente una: `describe`. No es pobreza, es la misma idea llevada al final. En Linux la observabilidad se fue agregando por capas durante treinta años; acá el kernel publica lo que sabe por el único canal que hay, porque [[10-Un-kernel-cuyo-usuario-no-es-humano|el que mira es un programa, no una persona con una terminal]].

---

## Las tres máquinas

Vas a trabajar contra tres cosas distintas, y confundirlas es la primera fuente de confusión:

|                      | Qué es                                                   | Para qué                                                      |
| -------------------- | -------------------------------------------------------- | ------------------------------------------------------------- |
| **Tu Debian**        | Hardware de verdad, con tu [[NVMe]], tu [[IOMMU]], tus [[Aparato|aparatos]]. | Mirar. Nunca romper.                                          |
| **Una VM de Linux**  | Un Linux chico en [[QEMU]], desechable.                      | Romper: cargar [[Modulo-de-kernel|módulos]], provocar un `oops`, colgar el kernel. |
| **Kornelia en QEMU** | El kernel de este libro.                                 | Ver la otra respuesta al mismo problema.                      |

La diferencia entre la primera y la segunda importa más de lo que parece: **hay cosas que solo se ven en hardware real**. Tu IOMMU verdadero tiene grupos de dispositivos que QEMU no reproduce; tu NVMe tiene colas que el emulado no tiene. Y hay cosas que solo se pueden hacer en la VM, porque dejan la máquina inservible.

Armar la VM: [[P00-Armar-la-VM-de-practicas]].

---

## Preguntarle a Linux qué máquina es

Linux publica casi todo lo que sabe como **archivos que no existen en ningún disco**. `/proc` y `/sys` son [[Procfs-y-sysfs|sistemas de archivos falsos]]: cuando los leés, el kernel arma la respuesta en el momento. Es la misma idea que `describe` de Kornelia, con otra forma.

### El procesador

```bash
lscpu                      # el resumen legible
cat /proc/cpuinfo          # lo mismo, crudo, por núcleo
```

En `/proc/cpuinfo` mirá dos cosas: **cuántos bloques hay** (uno por núcleo lógico, y de ahí sale lo de [[40-Arrancar-el-segundo-nucleo|arrancar el segundo núcleo]]) y la línea `flags`, que son las capacidades del silicio. Ahí vive todo lo que un kernel tiene que preguntar antes de usar: `pge`, `pcid`, `nx`, `smep`, `tsc_deadline_timer`.

### La memoria física

```bash
sudo cat /proc/iomem       # el mapa: qué hay en cada rango de direcciones fisicas
```

**Esta es la salida más importante del capítulo.** Es literalmente lo mismo que Kornelia publica con `describe {what:["memory"]}`: el mapa de la memoria física, con qué es cada rango. Fijate que la RAM **no es un bloque continuo**: hay huecos, hay rangos reservados por el [[Firmware|firmware]], hay pedazos que son registros de aparatos y no memoria.

```
00000000-00000fff : Reserved
00001000-0009ffff : System RAM
000a0000-000bffff : PCI Bus 0000:00
...
c0000000-cfffffff : 0000:00:02.0        <- los registros de la placa de video
```

Los rangos que dicen `0000:00:02.0` son [[PCIe|BARs]]: direcciones que **no son memoria**, son un aparato. Ver [[Bus]].

Necesita `sudo` porque el mapa de memoria le dice a un atacante dónde apuntar.

### Los aparatos

```bash
lspci                      # qué hay en el bus PCIe
lspci -v                   # con sus BARs y sus interrupciones
sudo lspci -xxx -s 00:02.0 # los 256 bytes crudos del espacio de configuracion
lsusb
ls /sys/bus/pci/devices/   # lo mismo, como arbol de archivos
```

El `lspci -xxx` es el que vale la pena mirar despacio: esos bytes son **el mismo formato** que Kornelia lee en `describe {what:["pci"]}`, y están definidos por la especificación de [[PCIe]], no por Linux. Los primeros 64 bytes tienen el mismo significado en toda máquina PCIe que exista.

### Las interrupciones

```bash
cat /proc/interrupts       # una fila por interrupcion, una columna por nucleo
watch -n1 cat /proc/interrupts
```

Las columnas son **cuántas veces llegó a cada núcleo**. Mové el mouse y mirá subir el contador del mouse: eso es una interrupción, la cosa más difícil de visualizar de todo el libro, convertida en un número que sube. Ver [[Interrupcion]].

La columna del tipo dice `IO-APIC`, `PCI-MSI` o similar: ahí se ve de un vistazo cuántos aparatos ya no usan cable. Ver [[MSI]].

### ACPI y device tree

```bash
ls /sys/firmware/acpi/tables/        # las tablas, tal como las dejo el firmware
sudo cat /sys/firmware/acpi/tables/APIC | xxd | head    # cruda
ls /sys/firmware/devicetree/base/ 2>/dev/null           # si la maquina usa DT
```

Estas tablas son de [[ACPI]] —o de [[Device-tree|device tree]], donde no hay ACPI— y son **la fuente** de lo que hace Kornelia en [[ACPI]]. Si tenés `acpica-tools` instalado, `iasl -d` las decompila a texto legible, y ahí se ve por qué interpretar AML es un problema: es un lenguaje de programación.

### El IOMMU

```bash
ls /sys/class/iommu/                 # si hay, y cual
ls /sys/kernel/iommu_groups/         # los grupos de dispositivos
dmesg | grep -iE 'dmar|iommu|smmu'
```

Los **grupos** son el concepto que no se ve en QEMU y que cambia cómo se piensa el aislamiento: el IOMMU no siempre puede distinguir dos aparatos, y entonces los trata como uno. Ver [[IOMMU]].

### La tabla de arriba, resumida

| Qué querés saber | Comando | En Kornelia |
|---|---|---|
| Cuántos núcleos hay | `lscpu` | `describe {what:["cpus"]}` |
| El mapa de memoria física | `sudo cat /proc/iomem` | `describe {what:["memory"]}` |
| Qué aparatos hay | `lspci -v` | `describe {what:["pcie"]}` |
| Qué [[Interrupcion|interrupciones]] llegan | `cat /proc/interrupts` | `describe {what:["interrupts"]}` |
| Cómo se describe la máquina | `ls /sys/firmware/acpi/tables/` | `describe {what:["tables"]}` |
| Si hay IOMMU | `ls /sys/class/iommu/` | `describe {what:["iommu"]}` |
| A qué ritmo corre el tiempo | `cat /proc/cpuinfo \| grep MHz` | `describe {what:["clock"]}` |
| Qué reclamó quién | — *(no existe)* | `describe {what:["claims"]}` |

Las trece secciones que acepta `describe` son `memory`, `tables`, `claims`, `cpus`, `interrupts`, `pcie`, `cores`, `channel`, `handlers`, `exec`, `iommu`, `clock` y `cable` (`kernel-core/src/protocol.rs:347#Some("memory") => q.memory = true`). Y si le pedís una que no conoce, **contesta un error en vez de ignorarla**: contestar solo con lo que reconoció sería mentir por omisión.

La última fila es la interesante: en Linux **no hay** una pregunta que devuelva "quién es dueño de qué memoria", porque la respuesta es "el kernel, siempre". En Kornelia es una pregunta legítima porque hay [[23-Asignadores-y-por-que-aca-no-hay|reclamos]] y no asignaciones.

---

## Mirar adentro de un binario

Un ejecutable es un archivo con instrucciones y una tabla que dice dónde va cada pedazo en memoria. Estas cuatro herramientas lo abren:

```bash
file ./programa                  # que clase de archivo es
readelf -h ./programa            # la cabecera: arquitectura, entry point
readelf -lS ./programa           # los segmentos (que se carga donde) y las secciones
nm ./programa | head             # los simbolos: nombres de funciones y su direccion
objdump -d ./programa | less     # el desensamblado: las instrucciones
```

**Desensamblar** es traducir los bytes de vuelta a texto de ensamblador. Los bytes *son* el programa; el texto es una representación para que lo lea un humano. En una línea de `objdump -d` vas a ver tres cosas:

```
  1139:	48 89 e5             	mov    %rsp,%rbp
  └ direccion                └ los bytes de verdad    └ como se lee
```

Esa es la pieza que hace concreto el capítulo siguiente: **una instrucción es un puñado de bytes en memoria**, no una idea. Ver [[03-Que-hace-realmente-una-instruccion]].

Para el kernel de Kornelia:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
readelf -h target/x86_64-unknown-uefi/debug/kernel-x86_64.efi 2>/dev/null || \
  objdump -f target/x86_64-unknown-uefi/debug/kernel-x86_64.efi
```

Ojo: el kernel de x86_64 no es un [[ELF-y-PE|ELF]], es un **PE** — el formato de Windows — porque eso es lo que carga [[UEFI]]. Es la primera pista del bug de la ABI que [[27-La-ABI-la-pone-el-target-no-el-silicio|costó caro]].

---

## Mirar el kernel de Linux mientras corre

```bash
dmesg | less                     # el diario del kernel desde el arranque
sudo dmesg -w                    # y en vivo
uname -a                         # que kernel es
sudo cat /proc/kallsyms | head   # los simbolos del kernel corriendo, con su direccion
cat /proc/self/maps              # el espacio de direcciones de un proceso (este)
```

`/proc/self/maps` merece un minuto: cada línea es un rango de direcciones **virtuales** con sus permisos. Ahí se ve que un proceso no tiene "la memoria": tiene pedazos mapeados, con huecos enormes en el medio. Es la salida que hace tangible [[Tabla-de-paginas|la tabla de páginas]].

Y las herramientas de trazado, que son la parte de Linux que Kornelia no tiene ni va a tener:

```bash
sudo apt install linux-perf bpftrace trace-cmd
sudo perf top                            # donde esta gastando tiempo el kernel, en vivo
sudo bpftrace -e 'tracepoint:syscalls:sys_enter_openat { printf("%s %s\n", comm, str(args->filename)); }'
```

Ese `bpftrace` de una línea imprime cada archivo que cualquier programa de la máquina intenta abrir. Miralo correr diez segundos: es la mejor demostración de qué es [[Syscall|una llamada al sistema]] y de cuántas hay.

---

## Mirar la máquina desde afuera: el monitor de QEMU

Cuando el kernel que estás mirando **es el que está roto**, no le podés preguntar nada. Ahí se mira desde afuera: QEMU sabe todo sobre la máquina que emula.

```bash
qemu-system-x86_64 ... -monitor stdio      # o -monitor telnet:...
```

Y en el monitor:

| Comando | Qué muestra |
|---|---|
| `info registers` | **Todos los registros del procesador**, congelados. |
| `info mem` | Las traducciones activas: qué virtual apunta a qué física. |
| `info mtree` | El árbol de memoria de la máquina emulada: RAM, [[MMIO]], cada aparato. |
| `info pci` | Los aparatos y sus [[BAR|BARs]]. |
| `info irq` | Cuántas interrupciones se entregaron. |
| `x/16xb 0x1000` | Volcar memoria física cruda. |
| `stop` / `cont` | Congelar y seguir. |

`info registers` sobre una máquina colgada es lo más parecido a un debugger que hay cuando no hay debugger. Si el `RIP` (el puntero de instrucción) apunta a `0x0`, saltó a la nada; si apunta a la misma dirección dos veces con diez segundos de diferencia, está en un bucle.

> [!warning] En Kornelia el monitor no puede ir a la consola
> Los scripts de este proyecto usan `-serial stdio`, **nunca `mon:stdio`** (D26): el multiplexor se come el byte `0x01` como escape, y por ahí viaja CBOR. Si querés el monitor, sacalo por otro lado (`-monitor telnet:127.0.0.1:55555,server,nowait`). Y se sale de QEMU con `Ctrl-C`, no con `Ctrl-A X`.

### Un debugger de verdad, contra QEMU

QEMU puede hacerse pasar por un servidor de GDB, y entonces sí hay debugger:

```bash
qemu-system-x86_64 ... -s -S      # -s abre el puerto 1234, -S arranca congelado
gdb
  (gdb) target remote :1234
  (gdb) info registers
  (gdb) x/10i $pc                 # las proximas 10 instrucciones
  (gdb) stepi                     # una instruccion
```

Con eso se puede ir paso a paso desde **la primera instrucción del firmware**. Es la única forma cómoda de entender el arranque, y la vamos a usar en la **Parte III**.

---

## Cómo se depura algo que se quedó mudo

Este es el estado normal de un kernel roto: **no dice nada**. No hay `println!` que funcione, porque lo que se rompió puede ser justo el camino por el que se imprime.

La técnica de este proyecto es marcar el camino con letras:

```rust
p.uart_write_byte(b'A');
```

Y leer la traza. El caso que la justifica: la traza fue `1ST234KJ2Da2`, y **ninguna `b`**. Eso —una letra ausente— dijo que los dos núcleos hacían su parte y que el reclamado moría entre terminar el trabajo y guardar la respuesta. Ninguna otra herramienta lo hubiera dicho, porque el núcleo muerto no puede contestar preguntas. Las letras se sacan antes de commitear.

Dos trampas, las dos aprendidas a golpes:

1. **Instrumentar puede cambiar el fenómeno.** Escribir una letra desde el [[Handler|handler]] del reloj movió los tiempos lo suficiente para que un `exec` dejara de volver. Si el bug depende de tiempos, el dato va a un estático y se publica por `describe`.
2. **Las letras rompen el protocolo.** Salen después del marcador, y el cliente se las come como CBOR. El error aparece en otro lugar.

Y la técnica que resolvió más casos que cualquier otra: **separar las dos mitades**. Si el aparato escribe y la interrupción no llega, hacé sonar la interrupción a mano, sin aparato. Una de las dos mitades anda, y ya sabés cuál. Así se encontró que [[MSI|el GIC trataba un pulso como nivel]].

Todo esto vive junto en [[Indice-de-sintomas]], que es la nota a la que vas a volver más veces de todo el vault.

---

## Hablarle a Kornelia

```bash
export PATH="$HOME/.cargo/bin:$PATH"

./scripts/run-x86_64.sh              # arrancar (Ctrl-C para salir)
./scripts/client.py                  # el indice de lo que se puede preguntar
./scripts/client.py --what memory    # el mapa de memoria
./scripts/client.py --what tables --raw   # los bytes crudos de la respuesta
./scripts/client.py --console        # una terminal para hablarle a mano
./scripts/check.sh                   # el porton entero
SKIP_QEMU=1 ./scripts/check.sh       # sin bootear, para iterar rapido
```

`client.py` es el primer programa que usa el kernel **como lo va a usar un agente**, y trae su propio CBOR en unas 60 líneas a propósito: si usara una biblioteca, un desacuerdo entre el kernel y esa biblioteca se leería como "el kernel está bien" cuando quizá los dos estén mal de la misma manera. Ese razonamiento —**no compartir la implementación entre lo que se prueba y lo que prueba**— vale para cualquier prueba que escribas.

---

## Las herramientas, de una vez

```bash
sudo apt install -y \
  pciutils usbutils util-linux \
  binutils elfutils \
  qemu-system-x86 qemu-system-arm qemu-utils ovmf qemu-efi-aarch64 \
  gdb \
  linux-perf bpftrace trace-cmd \
  acpica-tools \
  xxd hexyl 2>/dev/null || true
```

Y para Kornelia, Rust —que **no está en el PATH** en esta máquina:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo --version
```

Conviene ponerlo en `~/.bashrc` de una vez.

---

## Las prácticas de este capítulo

- [[P00-Armar-la-VM-de-practicas]] — *(construir)* la máquina desechable donde se puede romper.
- [[P01-Preguntarle-a-Linux-que-maquina-es]] — *(mirar)* el recorrido completo de `/proc` y `/sys`, comparado con lo que contesta Kornelia.

---

## Recordar #flashcards/parte-00

¿Qué es `/proc`?::Un sistema de archivos que no existe en ningún disco: cuando lo leés, el kernel arma la respuesta en el momento. Es la superficie por la que Linux se describe a sí mismo, el equivalente de `describe` en Kornelia.

¿Qué archivo de Linux es el equivalente de `describe {what:["memory"]}`?::`/proc/iomem`: el mapa de la memoria física con qué es cada rango, incluidos los rangos que no son RAM sino registros de aparatos.

¿Qué te dice `info registers` en el monitor de QEMU que no te puede decir el kernel?::Los registros de una máquina **colgada**. Cuando el kernel se rompió no puede contestar nada, y QEMU sí porque mira desde afuera.

¿Por qué los scripts de Kornelia usan `-serial stdio` y nunca `mon:stdio`?::Porque el multiplexor del monitor se come el byte `0x01` como escape, y por el serie viaja CBOR, que lo usa como dato.

¿Cuál es el riesgo de depurar escribiendo letras por el cable?::Que cambia los tiempos y puede hacer aparecer o desaparecer el bug, y que las letras salen después del marcador del protocolo, así que el cliente las lee como CBOR.

Si un aparato hace [[DMA]] y la interrupción no llega, ¿cuál es la primera técnica?::Separar las dos mitades: hacer sonar la interrupción a mano, sin aparato. Una de las dos mitades anda y así se sabe cuál.

## Qué sigue

[[01-El-reloj-y-el-transistor]]
