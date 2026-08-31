# Kernel agente-céntrico

Kernel experimental mínimo que supone un **agente de IA como usuario** y quita
todas las capas posibles entre ese agente y el hardware.

El diseño completo está en [`docs/DISENO.md`](docs/DISENO.md) — 23 decisiones
tomadas, cada una con su justificación. Lo ya descartado, con sus motivos, en
[`docs/DESCARTADO.md`](docs/DESCARTADO.md).

**Para seguir con Claude Code:** abrí una sesión en esta carpeta (`cd` acá y
`claude`). El archivo `CLAUDE.md` se carga solo y trae todo el contexto: los
principios, las decisiones cerradas, las reglas de código y lo que sigue. No hace
falta volver a explicar nada.

## Estado

Arranca por UEFI en **x86_64 y aarch64**, le toma la máquina al firmware
(`ExitBootServices`) y vuelca por el cordón umbilical el **mapa de memoria
física real** de la máquina. Los diez verbos del protocolo todavía no existen.

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
```

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
```

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
| `kernel-core/src/memoria.rs` | El vocabulario normalizado del mapa de memoria, que hablan todos los entornos de arranque. |
| `boot-uefi/` | El entorno de arranque UEFI, compartido por las dos arquitecturas. Sin `asm!`. |
| `kernel-x86_64/` | Arranque UEFI + UART 16550 en puertos de E/S. |
| `kernel-aarch64/` | Arranque UEFI + UART PL011 en MMIO. |
| `scripts/` | Correr en QEMU y verificar la frontera. |
| `docs/DISENO.md` | El documento vivo de diseño. |

## Lo que sigue

1. Capturar la Configuration Table de UEFI: es donde viven los punteros a ACPI
   y al device tree, y sin eso no hay núcleos, ni PCIe, ni interrupciones.
2. El protocolo CBOR sobre el UART, reemplazando el texto de arranque.
3. `mem.claim` y `exec`.

Las deudas anotadas están en [`docs/DISENO.md`](docs/DISENO.md) §7 — la más
importante es que la pila del kernel vive dentro de memoria que hoy se informa
como libre.
