# Kernel agente-céntrico

Kernel experimental mínimo que supone un **agente de IA como usuario** y quita
todas las capas posibles entre ese agente y el hardware.

El diseño completo está en [`docs/DISENO.md`](docs/DISENO.md) — 25 decisiones
tomadas, cada una con su justificación. Lo ya descartado, con sus motivos, en
[`docs/DESCARTADO.md`](docs/DESCARTADO.md).

**Para seguir con Claude Code:** abrí una sesión en esta carpeta (`cd` acá y
`claude`). El archivo `CLAUDE.md` se carga solo y trae todo el contexto: los
principios, las decisiones cerradas, las reglas de código y lo que sigue. No hace
falta volver a explicar nada.

## Estado

Arranca por UEFI en **x86_64 y aarch64**, le toma la máquina al firmware
(`ExitBootServices`), vuelca por el cordón umbilical el **mapa de memoria física
real** y dónde la máquina guarda su propia descripción (ACPI, device tree,
SMBIOS). El cordón ya es bidireccional: el kernel escucha.

Los diez verbos del protocolo todavía no existen.

## Requisitos

```bash
rustup target add x86_64-unknown-uefi aarch64-unknown-uefi
sudo apt install qemu-system-x86 qemu-system-arm ovmf qemu-efi-aarch64
```

## Correr

```bash
./scripts/run-x86_64.sh
./scripts/run-aarch64.sh
./scripts/check-frontera.sh # D23/D24: verifica los dos ejes de la frontera
./scripts/check.sh          # el porton completo: frontera + tests + compila y BOOTEA las dos
```

`check.sh` es lo que corre CI, y conviene correrlo antes de commitear. No se
conforma con que compile: bootea las dos arquitecturas en QEMU y verifica lo
que dicen por el serie. Que compile no prueba nada — un puntero mal leído
compila perfecto.

Los scripts pasan a QEMU cualquier argumento extra, así que `./scripts/run-x86_64.sh -m 1G`
arranca con 1 GiB y el mapa de memoria tiene que reflejarlo.

**Para salir de QEMU: `Ctrl-A`, soltar, y después `X`.**

`Ctrl-A` funciona porque los scripts usan `-serial mon:stdio`: el prefijo `mon:`
multiplexa el monitor de QEMU y el puerto serie sobre la misma terminal, y es
ese multiplexor el que implementa los escapes. Sin `mon:` no hay escape ninguno
y el `Ctrl-A` le llega al kernel como un byte más.

Los otros dos que sirven:

| Tecla | Qué hace |
|---|---|
| `Ctrl-A` `X` | Sale de QEMU. |
| `Ctrl-A` `C` | Alterna entre el kernel y el monitor de QEMU. |
| `Ctrl-A` `H` | Lista todo lo demás. |

Desde el monitor se puede mirar la máquina por fuera del kernel — útil para
contrastar lo que el kernel dice contra lo que QEMU sabe:

```
(qemu) info mtree     # el mapa de memoria segun QEMU
(qemu) info registers # el estado del CPU
(qemu) xp /16xb 0x0   # volcar memoria fisica
```

Salida esperada (recortada — el mapa real trae decenas de regiones, y son
distintas en cada arquitectura porque son de máquinas distintas):

```
== kernel agente-centrico ==
arquitectura: aarch64

mapa de memoria: 81 regiones, 518576 KiB libres
  0x0000000040000000      64 MiB  libre
  0x0000000044000000     128 KiB  libre
  ...
  0x000000005fe20000     576 KiB  firmware
  0x0000000009010000       4 KiB  mmio

El cordon umbilical esta vivo.
Sin procesos. Sin archivos. Sin shell. Sin usuarios.

escuchando. cada byte que llegue se informa crudo.
```

**El kernel escucha.** Si tecleás algo en la terminal de QEMU, contesta con el
byte crudo que recibió:

```
rx 0x48  'H'
rx 0x6f  'o'
```

Informa el byte y no la línea a propósito: lo que va a viajar por acá es CBOR
binario, no comandos. **No es una shell** — eso sería la capa antropocéntrica
que el proyecto saca (D10). Es un andamio para probar el camino de entrada, y
desaparece cuando esté el protocolo.

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
| `kernel-core/src/tests.rs` | Tests que corren en la máquina de desarrollo, sin bootear nada. |
| `boot-uefi/` | El entorno de arranque UEFI, compartido por las dos arquitecturas. Sin `asm!`. |
| `kernel-x86_64/` | Arranque UEFI + UART 16550 en puertos de E/S. |
| `kernel-aarch64/` | Arranque UEFI + UART PL011 en MMIO. |
| `scripts/` | Correr en QEMU, y `check.sh`, que es el portón que corre CI. |
| `docs/DISENO.md` | El documento vivo de diseño. |

## Lo que sigue

1. El protocolo CBOR sobre el UART (D6), reemplazando el texto y el andamio de
   escucha. Es lo que convierte esto en algo que un agente puede usar.
2. `describe` sirviendo de verdad lo que ya sabemos: el mapa de memoria y dónde
   están las tablas.
3. Parsear ACPI para sacar núcleos, PCIe y el controlador de interrupciones.
4. `mem.claim` y `exec`.

Las deudas anotadas están en [`docs/DISENO.md`](docs/DISENO.md) §7 — la más
importante es que la pila del kernel vive dentro de memoria que hoy se informa
como libre.
