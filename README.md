# Kernel agente-céntrico

Kernel experimental mínimo que supone un **agente de IA como usuario** y quita
todas las capas posibles entre ese agente y el hardware.

El diseño completo está en [`docs/DISENO.md`](docs/DISENO.md) — 29 decisiones
tomadas, cada una con su justificación. Lo ya descartado, con sus motivos, en
[`docs/DESCARTADO.md`](docs/DESCARTADO.md).

**Para seguir con Claude Code:** abrí una sesión en esta carpeta (`cd` acá y
`claude`). El archivo `CLAUDE.md` se carga solo y trae todo el contexto: los
principios, las decisiones cerradas, las reglas de código y lo que sigue. No hace
falta volver a explicar nada.

## Estado

Arranca por UEFI en **x86_64 y aarch64**, le toma la máquina al firmware
(`ExitBootServices`) y **habla CBOR por el cordón umbilical** (D6). Corre sobre
pila y tablas de páginas propias, y captura los faults en vez de reiniciarse.

**Diez de los once verbos andan:** `describe`, `mem.claim`, `mem.read`,
`mem.write`, `release`, `exec`, `core.claim`, `listen` e **`irq.install`**. Un agente ya puede preguntarle a la máquina
qué es —memoria, núcleos, controlador de interrupciones, PCIe—, reclamar memoria
física, subirle código máquina y **correrlo**. Y si ese código falla, el fault
vuelve como respuesta en vez de matar la máquina: ni siquiera destruyendo el
puntero de pila, porque las excepciones entran en una pila aparte.

Falta uno: `dma.allow`, el IOMMU.

## Requisitos

```bash
rustup target add x86_64-unknown-uefi aarch64-unknown-uefi
sudo apt install qemu-system-x86 qemu-system-arm ovmf qemu-efi-aarch64
```

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
```

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
| `kernel-core/src/protocol.rs` | Los verbos. Hoy diez de los once. |
| `kernel-core/src/handlers.rs` | Los handlers de interrupción del agente (D9). |
| `kernel-core/src/channel.rs` | El segundo canal: el buzón que arma el agente (D17, D28). |
| `kernel-core/src/serial.rs` | El buffer entre el timbre del cable y el bucle. |
| `kernel-core/src/claims.rs` | La tabla de handles: qué tiene reclamado el agente (D14). |
| `kernel-core/src/fault.rs` | Los faults como datos (P5): causa, dirección y registros. |
| `kernel-core/src/acpi.rs` | Recorre las tablas de ACPI: núcleos, interrupciones y PCIe. |
| `kernel-core/src/cores.rs` | Los núcleos que el agente tiene reclamados (D13). |
| `kernel-core/src/paging.rs` | El **plan** de mapeo: qué va cacheable y qué no (D12). |
| `kernel-core/src/stack.rs` | La pila propia del kernel, verificada contra el mapa real. |
| `kernel-core/src/tests.rs` | 102 tests que corren en la máquina de desarrollo, sin bootear nada. |
| `boot-uefi/` | El entorno de arranque UEFI, compartido por las dos arquitecturas. Sin `asm!`. |
| `kernel-x86_64/` | Arranque UEFI + UART 16550 en puertos de E/S. |
| `kernel-aarch64/` | Arranque UEFI + UART PL011 en MMIO. |
| `scripts/` | Correr en QEMU, `client.py` para hablarle, y `check.sh`, que es el portón que corre CI. |
| `docs/DISENO.md` | El documento vivo de diseño. |

## Lo que sigue

**Los once verbos andan en las dos arquitecturas y no queda ninguna deuda
abierta** (`docs/DISENO.md` §7 está entera en resuelto). El kernel está
terminado en el sentido que importa: el agente sube código máquina, lo corre con
el privilegio que declara, y si falla recibe el fault como dato.

Lo que sigue no es el kernel: es **llenar el espacio que el kernel deja vacío**
(P2). Hoy `blob.bin` tiene el mecanismo —se carga, se ejecuta, se puede cancelar,
y le puede pedir cosas al kernel— pero no tiene contenido. Los drivers que
nombran D19 y D20 son lo que *va* a ir ahí:

1. Un **cargador NVMe** en el blob, para que el payload no tenga que entrar en
   los 256 KiB del blob ni en un archivo FAT32.
2. Un **driver de red**, que es lo que convierte al UART en lo que D5 dice que
   es: un cordón umbilical y no el transporte.

Lo que falta y **no se puede** está anotado igual, con el número que lo prueba:
el segundo escalón para cortar un núcleo en aarch64 no existe porque este GIC no
sabe reconocer una interrupción del Grupo 1, y el kernel lo publica en vez de
prometerlo.

## Licencia

Doble MIT ([LICENSE-MIT](LICENSE-MIT)) o Apache-2.0
([LICENSE-APACHE](LICENSE-APACHE)), a elección — que es la convención del
ecosistema Rust.

Nada de este código sale de Linux, y no por descuido: Linux es GPLv2, pero sobre
todo sus drivers están tejidos con su propia infraestructura. Acá un driver
tiene que hablar los once verbos —`mem.claim`, `dma.allow`, `irq.install`— y eso
no lo hace ningún driver existente. Las especificaciones (NVMe, virtio) son
públicas y es de ahí que sale lo que se escribe.
