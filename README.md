# Kernel agente-céntrico

Kernel experimental mínimo que supone un **agente de IA como usuario** y quita
todas las capas posibles entre ese agente y el hardware.

El diseño completo está en [`docs/DISENO.md`](docs/DISENO.md) — 30 decisiones
tomadas, cada una con su justificación. Lo ya descartado, con sus motivos, en
[`docs/DESCARTADO.md`](docs/DESCARTADO.md). Qué cambió y por qué importa, en
[`CHANGELOG.md`](CHANGELOG.md).

**Para seguir con Claude Code:** abrí una sesión en esta carpeta (`cd` acá y
`claude`). El archivo `CLAUDE.md` se carga solo y trae todo el contexto: los
principios, las decisiones cerradas, las reglas de código y lo que sigue. No hace
falta volver a explicar nada.

## Para estudiantes de Computación II

Si llegaste acá por la convocatoria, empezá por estas tres cosas.

**Qué es.** Un sistema operativo mínimo, escrito desde cero, que se pregunta algo
que todavía nadie probó en serio: los sistemas que usamos —Linux, Windows,
macOS— fueron diseñados suponiendo que del otro lado hay una persona. Procesos,
archivos, usuarios, permisos: todas esas ideas existen porque un humano necesita
organizarse así. ¿Y si el que opera la máquina es un agente de IA, que puede
escribir código y decir de antemano qué va a hacer? ¿Cuánto de todo eso sigue
haciendo falta?

Este kernel es el experimento que intenta responderlo. Tiene **once operaciones**
y nada más: ni procesos, ni sistema de archivos, ni drivers. Los drivers los
escribe el agente, usando solo esas once. Ya andan uno de disco y uno de red,
escritos así.

**Qué vas a aprender.** Lo que en la materia vemos como concepto, acá se toca:
qué hace realmente el hardware cuando arranca, cómo se habla con un dispositivo
sin que haya un sistema operativo en el medio, por qué existe la memoria virtual,
qué pasa de verdad cuando un programa falla. Vas a leer y escribir código que
corre sin nada debajo.

**Qué hace falta saber.** Computación II cursada o en curso. No hace falta saber
Rust, ni haber tocado un kernel antes: eso se aprende acá. Lo que sí hace falta
es aguante para leer documentación técnica y ganas de entender cómo funciona algo
por dentro.

**Por dónde entrar.** Cloná el repo, seguí *Requisitos* y corré
`./scripts/run-x86_64.sh`. Vas a ver un sistema operativo arrancando desde cero
en tu propia máquina. Después probá `./scripts/client.py --exec`: ahí se ve la
tesis del proyecto en ocho bytes de código máquina.

El diseño completo, con las treinta decisiones y por qué se tomó cada una, está
en [`docs/DISENO.md`](docs/DISENO.md). Lo que se descartó, y por qué, en
[`docs/DESCARTADO.md`](docs/DESCARTADO.md) — ese archivo es probablemente el más
útil para entender cómo se piensa un sistema.

---

## Estado

Arranca por UEFI en **x86_64 y aarch64**, le toma la máquina al firmware
(`ExitBootServices`) y **habla CBOR por el cordón umbilical** (D6). Corre sobre
pila y tablas de páginas propias, y captura los faults en vez de reiniciarse.

**Los once verbos andan, y enteros en las dos arquitecturas:**

`describe` · `mem.claim` · `mem.read` · `mem.write` · `core.claim` · `exec` ·
`irq.install` · `irq.install_raw` · `dma.allow` · `release` · `listen`

El único con asterisco es `irq.install_raw`, que en aarch64 devuelve un error
porque **no hay** un camino más crudo que el que ya se usa — y la máquina lo dice
en vez de callarlo.

Un agente puede preguntarle a la máquina qué es —memoria, núcleos, controlador
de interrupciones, PCIe, reloj—, reclamar memoria física, subirle código máquina
y **correrlo con el privilegio que declara** (D27). Si ese código falla, el fault
vuelve como respuesta en vez de matar la máquina: ni siquiera destruyendo el
puntero de pila, porque las excepciones entran en una pila aparte. Y si no
vuelve, el plazo que el agente declaró lo corta.

La máquina se describe por los **dos dialectos**: ACPI donde hay ACPI, y device
tree donde no. Cuál usar no lo elige el kernel — es cuál dejó el firmware, y se
comprueba booteando la misma máquina con `acpi=off`.

El IOMMU arranca **encendido y vacío** (D8), así que sin declarar nada ningún
aparato llega a la memoria: VT-d en x86_64, SMMUv3 en aarch64. Comprobado en las
dos con un aparato de verdad que hace DMA.

### Lo que el agente ya escribió encima

Esto **no es el kernel** (D4): son drivers y transporte del lado del agente, que
usan sólo los once verbos.

- Un **driver de NVMe** que lee y escribe el disco, así que un programa se puede
  dejar grabado y aparece solo en el próximo arranque.
- Un **driver de red** (Intel 82540EM) que manda y recibe paquetes, comprobado
  con un ARP de ida y vuelta contra el otro extremo del cable.
- **El transporte de D5**: encima del driver de red corre un bucle de código
  máquina en un núcleo reclamado que mueve bytes entre la placa y el buzón que
  el agente le entregó con `listen`. **El kernel contesta el protocolo por red**
  sin enterarse de que del otro lado hay una red. El pedido no sale por el
  cable: sale de un socket del host, cruza la red, y la respuesta vuelve por el
  mismo camino.

### Y `blob.bin` (D19/D20)

El firmware trae un blob de la partición y el kernel lo corre antes de escuchar
el cable, sin privilegio. Antes de saltar **avisa y espera dos segundos**:
cualquier byte lo cancela, que es lo que hace que un blob roto no deje la máquina
inútil en cada arranque.

Y el blob **se compila** (`blob/`): un crate `no_std` que sale a binario plano
para las dos arquitecturas. Adentro tiene un cargador NVMe completo, así que
**trae del disco un programa que no está en la partición y lo corre** — recorre
el bus, encuentra el controlador por su clase, lo resetea, le arma las colas, le
declara el DMA, comprueba la suma y salta.

## Requisitos

```bash
rustup target add x86_64-unknown-uefi aarch64-unknown-uefi
sudo apt install qemu-system-x86 qemu-system-arm ovmf qemu-efi-aarch64
```

Para compilar `blob/` hacen falta además los dos targets de bare-metal, pero no
hay que instalarlos a mano: `build-blob.sh` los agrega solo. **No** están en
`rust-toolchain.toml` a propósito — ahí rustup resincroniza el toolchain entero
en vez de agregar un componente, y en una instalación con un conflicto viejo eso
deja de compilar todo, no sólo el blob.

## Correr

```bash
./scripts/run-x86_64.sh
./scripts/run-aarch64.sh
./scripts/check-boundary.sh # D23/D24: verifica los dos ejes de la frontera
./scripts/check.sh          # el porton completo: frontera + tests + compila y BOOTEA las dos
```

`check.sh` es lo que corre CI, y conviene correrlo antes de commitear. No se
conforma con que compile: bootea las dos arquitecturas en QEMU y verifica lo
que dicen por el serie. Que compile no prueba nada — un puntero mal leído
compila perfecto.

Los scripts pasan a QEMU cualquier argumento extra, así que `./scripts/run-x86_64.sh -m 1G`
arranca con 1 GiB y el mapa de memoria tiene que reflejarlo.

**Para salir de QEMU: `Ctrl-C`.**

No es `Ctrl-A X`, y eso tiene una razón de peso (D26): ese atajo lo implementa
un multiplexor que **se come el byte `0x01` como escape junto con el que le
sigue**. Por el serie viaja CBOR, donde `0x01` es un byte como cualquier otro.
Se descubrió cuando el primer pedido del cliente empezaba con `83 01 68` y QEMU
contestó su pantalla de ayuda: había leído `Ctrl-A h`.

Si hace falta el monitor de QEMU, va por otro lado y no por el serie:

```bash
./scripts/run-x86_64.sh -monitor telnet:127.0.0.1:5555,server,nowait
```

## Hablarle al kernel

`scripts/client.py` es el primer programa que usa el kernel como lo va a usar
un agente: arranca QEMU, espera la marca `-- CBOR --` y habla el protocolo.

```bash
./scripts/client.py                       # el indice de lo que se puede pedir (D16)
./scripts/client.py --what memory         # el mapa de memoria
./scripts/client.py --what cpus,interrupts,pcie
./scripts/client.py --what tables --raw   # mostrando los bytes que viajan
./scripts/client.py --arch aarch64
./scripts/client.py --memory             # el lazo: claim, write, read, release
./scripts/client.py --exec                # sube codigo maquina y lo corre
./scripts/client.py --smp 4 --cores     # arranca los otros nucleos
./scripts/client.py --mailbox                # arma el segundo canal y le habla por ahi
./scripts/client.py --doorbell               # el agente despierta al kernel
./scripts/client.py --handler              # el agente atiende una interrupcion
./scripts/client.py --dma                 # el IOMMU hace cumplir lo declarado (D8)
./scripts/client.py --nvme                # un driver de disco con los once verbos
./scripts/client.py --net                 # un driver de red: un ARP de ida y vuelta
./scripts/client.py --udp                 # un datagrama del host al agente y la vuelta
./scripts/client.py --transport           # D5: el kernel contesta el protocolo por red
```

Y el blob, que se compila aparte y se le pasa a la máquina:

```bash
./scripts/build-blob.sh x86_64                        # blob/ -> binario plano
./scripts/client.py --arch x86_64 --write-payload /tmp/payload.bin
BLOB=target/blob-x86_64.bin PAYLOAD=/tmp/payload.bin ./scripts/run-x86_64.sh
#   -> the blob returned, leaving 0xc0ffee
```

Ese `0xc0ffee` lo deja **el programa que estaba en el disco**, que el blob no
tiene adentro: para llegar ahí tuvo que manejar el controlador NVMe entero.

Con `--exec` se ve la tesis del proyecto en ocho bytes de código máquina:

```
un programa que anda: 48c7c0eeffc000c3        # mov rax, 0xC0FFEE ; ret
  faulted=False
  rax=0xc0ffee

un programa que falla: 48b80000000000400000488900c3
  faulted=True
  fault: {'cause': 'page-fault', 'raw': 14, 'detail': 2, 'address': 70368744177664}

la maquina sigue viva despues del fault
```

En un sistema operativo normal, el segundo programa sería un SIGSEGV y el
proceso se moriría. Acá **es un valor de retorno**: causa, dirección tocada y
todos los registros del instante exacto en que falló. El agente lo lee y corrige.

Con `--memory` se ve el primer momento en que el agente no solo mira la
máquina sino que la **usa**:

```
mem.claim  ok     {'handle': 1, 'start': 1073741824, 'bytes': 4096, 'kind': 'free'}
mem.write  ok     {'written': 8}
mem.read   ok     {'bytes': b'\xde\xad\xbe\xef\x00\x11"3'}
mem.read   ERROR  {'error': 'out-of-bounds'}
mem.claim  ERROR  {'error': 'already-claimed'}
release    ok     {'released': 1}
mem.read   ERROR  {'error': 'no-such-handle'}
```

Con `--raw` se ve el intercambio completo, que son 12 bytes de ida:

```
-> 8301686465736372696265a0
<- 8301f5a46461726368667838365f36346873656374696f6e7382666d656d6f7279...

respuesta id=1 ok=True
  arch: x86_64
  sections: ['memory', 'tables']
  memory: {'regions': 110, 'free': 127528960}
  tables: {'acpi': True, 'device_tree': False, 'smbios': True}
```

Eso es D16 en acción: **sin argumentos el kernel no vuelca todo, devuelve el
índice** de lo que hay para pedir. Volcar todo ahogaría a un cliente chico y
resumir le sacaría información a uno grande, así que cada uno pide la
profundidad que quiere y el kernel no tiene que suponer con quién habla.

El cliente trae su propio CBOR en unas 60 líneas a propósito: si usara una
biblioteca, un desacuerdo entre el kernel y esa biblioteca se leería como "el
kernel está bien" cuando quizá los dos estén mal de la misma manera.

Salida esperada (recortada — el mapa real trae decenas de regiones, y son
distintas en cada arquitectura porque son de máquinas distintas):

```
== kernel agente-centrico ==
arquitectura: aarch64
memoria: 81 regiones, 518576 KiB libres
tablas: acpi=si device-tree=no smbios=si

Sin procesos. Sin archivos. Sin shell. Sin usuarios.
-- CBOR --
```

El banner es corto a propósito: alcanza para saber si la máquina está viva y
qué encontró. El detalle va por `describe`, que manda lo que le pidan. Desde la
marca `-- CBOR --`, lo que sale es binario.

Ese volcado se puede contrastar contra el device tree que genera QEMU, que es
una fuente independiente:

```bash
qemu-system-aarch64 -machine virt,dumpdtb=virt.dtb -display none
grep -ao '[a-z0-9-]*@[0-9a-f]*' virt.dtb | sort -u
#   memory@40000000   <- donde el kernel reporta la primera region
#   pl031@9010000     <- la pagina que el kernel reporta como mmio
```

## Estructura

| Ruta | Qué es |
|---|---|
| `kernel-core/` | El kernel portable. **Sin una línea específica de arquitectura ni de cómo se arrancó** (D23/D24). |
| `kernel-core/src/platform.rs` | La frontera: el trait `Platform` es todo lo que una arquitectura debe proveer. |
| `kernel-core/src/memory.rs` | El vocabulario normalizado del mapa de memoria, que hablan todos los entornos de arranque. |
| `kernel-core/src/machine.rs` | Lo que se sabe de la máquina: regiones y dónde están ACPI y el device tree. |
| `kernel-core/src/tables.rs` | Lee y **verifica** los encabezados de ACPI y del device tree. |
| `kernel-core/src/cbor.rs` | El formato binario del protocolo (D6), escrito a mano. |
| `kernel-core/src/protocol.rs` | Los once verbos, uno por función. |
| `kernel-core/src/handlers.rs` | Los handlers de interrupción del agente (D9). |
| `kernel-core/src/channel.rs` | El segundo canal: el buzón que arma el agente (D17, D28). |
| `kernel-core/src/serial.rs` | El buffer entre el timbre del cable y el bucle. |
| `kernel-core/src/claims.rs` | La tabla de handles: qué tiene reclamado el agente (D14). |
| `kernel-core/src/fault.rs` | Los faults como datos (P5): causa, dirección y registros. |
| `kernel-core/src/acpi.rs` | Recorre las tablas de ACPI: núcleos, interrupciones y PCIe. |
| `kernel-core/src/cores.rs` | Los núcleos que el agente tiene reclamados (D13). |
| `kernel-core/src/paging.rs` | El **plan** de mapeo: qué va cacheable y qué no (D12). |
| `kernel-core/src/stack.rs` | La pila propia del kernel, verificada contra el mapa real. |
| `kernel-core/src/tests.rs` | 104 tests que corren en la máquina de desarrollo, sin bootear nada. |
| `kernel-core/src/work.rs` | El buzón por núcleo: cómo se le da trabajo a un núcleo reclamado. |
| `boot-uefi/` | El entorno de arranque UEFI, compartido por las dos arquitecturas. Sin `asm!`. |
| `kernel-x86_64/` | Arranque UEFI + UART 16550 en puertos de E/S. |
| `kernel-aarch64/` | Arranque UEFI + UART PL011 en MMIO. |
| `blob/` | `blob.bin`: la distribución reemplazable (D20). **No es el kernel** — es código del agente pre-armado, con un cargador NVMe adentro. Sale a binario plano, sin GOT y sin `.bss`. |
| `scripts/` | Correr en QEMU, `client.py` para hablarle, y `check.sh`, que es el portón que corre CI. |
| `docs/DISENO.md` | El documento vivo de diseño. |

## Qué NO hay

Esto es un experimento, y la lista de lo que falta es parte de la descripción.
Nada de esto es un descuido: está acá para que nadie se forme una idea
equivocada leyendo lo de arriba.

- **Nunca corrió en silicio de verdad.** Todo lo que dice este README está
  comprobado en QEMU, en las dos arquitecturas. QEMU perdona: los tiempos de una
  placa emulada no son los de una real, y su modelo de memoria en ARM es más
  benigno que el del hardware. Es la clase de diferencia que encuentra bugs que
  ninguna prueba encuentra.
- **No hay un `blob.bin` versionado en el repo.** Y no debería haberlo: D20 dice
  que el blob es reemplazable, borrable e ignorable. Se compila con
  `./scripts/build-blob.sh`.
- **El driver de red vive del lado del cliente, en Python.** El de NVMe ya está
  también adentro del blob; el de red todavía no. O sea que **una máquina no
  arranca con red sola**: para eso hay que mudarlo, que es lo que sigue.
- **Un solo modelo de placa de red.** El driver es para la Intel 82540EM, porque
  una placa de red no se puede manejar por clase como un NVMe: `02.00.00` sólo
  quiere decir "ethernet" y abajo de esa clase cada modelo tiene registros que no
  se parecen en nada. D20 ya lo anticipaba al hablar de "una lista conocida": la
  lista existe porque no hay forma de no tenerla.
- **No hay INTx**, el camino viejo de interrupciones de PCI. Saber qué cable le
  toca a un aparato pide interpretar AML, que es un lenguaje entero adentro de
  ACPI. MSI lo hace innecesario y es lo que usan los aparatos de hoy.
- **En aarch64 no se puede cortar un núcleo que enmascaró las interrupciones.**
  No es una deuda: este GIC no sabe reconocer una interrupción del Grupo 1, y el
  diagnóstico está medido y cerrado en `CLAUDE.md`. El kernel **publica hasta
  dónde llega** (`describe exec` trae `cancel`) en vez de prometer un corte que
  no llega.
- **El nombre es provisorio.** Ver `libro/_El-nombre.md`.

## Lo que sigue

**Los once verbos andan en las dos arquitecturas y no queda ninguna deuda
abierta** (`docs/DISENO.md` §7 está entera en resuelto). El kernel está terminado
en el sentido que importa: el agente sube código máquina, lo corre con el
privilegio que declara, y si falla recibe el fault como dato.

Lo que sigue no es el kernel: es **llenar el espacio que el kernel deja vacío**
(P2).

1. **Mudar el driver de red al blob.** El de NVMe ya está adentro y con eso el
   blob es un cargador: trae del disco un payload tan grande como haga falta. La
   red va en ese payload, no en el blob. Es lo que haría que una máquina arranque
   con red **sin que haya nadie del otro lado del cable**.
2. **Correrlo en una máquina de verdad**, que es lo que va a decir cuánto de todo
   esto era cierto y cuánto era QEMU siendo amable.

## Licencia

Doble MIT ([LICENSE-MIT](LICENSE-MIT)) o Apache-2.0
([LICENSE-APACHE](LICENSE-APACHE)), a elección — que es la convención del
ecosistema Rust.

Nada de este código sale de Linux, y no por descuido: Linux es GPLv2, pero sobre
todo sus drivers están tejidos con su propia infraestructura. Acá un driver
tiene que hablar los once verbos —`mem.claim`, `dma.allow`, `irq.install`— y eso
no lo hace ningún driver existente. Las especificaciones (NVMe, virtio) son
públicas y es de ahí que sale lo que se escribe.
