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

Hito 1: arranca por UEFI en **x86_64 y aarch64**, y habla por el cordón
umbilical (UART). Nada más todavía.

## Requisitos

```bash
rustup target add x86_64-unknown-uefi aarch64-unknown-uefi
sudo apt install qemu-system-x86 qemu-system-arm ovmf qemu-efi-aarch64
```

## Correr

```bash
./scripts/run-x86_64.sh     # Ctrl-A luego X para salir de QEMU
./scripts/run-aarch64.sh
./scripts/check-frontera.sh # D23: verifica que kernel-core no dependa de una arquitectura
```

Salida esperada, idéntica en las dos:

```
== kernel agente-centrico ==
arquitectura: x86_64

El cordon umbilical esta vivo.
Sin procesos. Sin archivos. Sin shell. Sin usuarios.
```

## Estructura

| Ruta | Qué es |
|---|---|
| `kernel-core/` | El kernel portable. **Sin una línea específica de arquitectura** (D23). |
| `kernel-core/src/platform.rs` | La frontera: el trait `Platform` es todo lo que una arquitectura debe proveer. |
| `kernel-x86_64/` | Arranque UEFI + UART 16550 en puertos de E/S. |
| `kernel-aarch64/` | Arranque UEFI + UART PL011 en MMIO. |
| `scripts/` | Correr en QEMU y verificar la frontera. |
| `docs/DISENO.md` | El documento vivo de diseño. |

## Lo que sigue

1. `describe` — devolver el hardware real (mapa de memoria, núcleos, PCIe).
2. El protocolo CBOR sobre el UART, reemplazando el texto de arranque.
3. `mem.claim` y `exec`.
