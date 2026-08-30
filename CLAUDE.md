# Kernel agente-céntrico — contexto del proyecto

> Este archivo se carga solo al abrir Claude Code en esta carpeta.
> **Leé `docs/DISENO.md` antes de proponer cualquier cambio de arquitectura.**

## Qué es

Un kernel experimental, mínimo, que **supone un agente de IA como usuario** y quita
todas las capas posibles entre ese agente y el hardware. El agente usa el cómputo con
una lógica que no tiene por qué ser la humana, sin las abstracciones ni los
guardarraíles heredados de POSIX.

No es un sistema operativo de propósito general. Es un experimento sobre qué queda de
un kernel cuando el operador deja de ser una persona.

**El autor no es programador de sistemas.** Explicá los términos técnicos cuando
aparezcan (qué es un BAR, qué es DMA, qué es un blob) sin que te lo pidan, y en pocas
líneas. Escribí en español. Los comentarios del código van en español.

## Los seis principios

| # | Principio |
|---|---|
| P1 | El kernel nunca es la razón por la que no se puede usar un dispositivo. |
| P2 | Cada capa que se saca no se reemplaza: se deja vacía para que el agente la llene si quiere. |
| P3 | El agente no es un participante en tiempo de ejecución. Es un **compilador**: escribe código que corre sin él. |
| P4 | La máquina se describe a sí misma. El agente no asume nada sobre ella. |
| P5 | Los faults son datos, no muerte. |
| P6 | El hardware hace cumplir lo que **el agente declaró**, no una política del kernel. |

## Decisiones ya tomadas

**23 decisiones (D1–D23) están cerradas en `docs/DISENO.md`, cada una con su
justificación. No las reabras sin motivo nuevo.** Las más importantes:

- **D1** El agente es externo (cliente), no residente — pero la puerta a residente queda abierta.
- **D4** El agente escribe sus propios drivers. El kernel no tiene ninguno salvo el UART.
- **D5** El UART es el cordón umbilical, no el transporte. El agente escribe el transporte rápido.
- **D6** Protocolo binario CBOR, nunca JSON (se transportan código máquina y volcados).
- **D7/D11** Los faults devuelven registros + causa + log. El kernel no deshace nada: el rollback real es imposible, no caro.
- **D8** IOMMU encendido por defecto. No es guardarraíl: hace cumplir lo que el agente declaró con `dma.allow`, y sin él un DMA mal apuntado es corrupción silenciosa.
- **D12** Identity map de toda la RAM. MMIO no-cacheable.
- **D13** Un solo agente. Se multiplica solo reclamando varios núcleos.
- **D15** El kernel es agnóstico sobre quién está del otro lado. Sin identidad ni autenticación.
- **D20** Separación kernel / distribución: `kernel.efi` (cero drivers) y `blob.bin` (reemplazable).
- **D22/D23** x86_64 **y** aarch64 en verde desde el primer commit; la frontera la verifica CI.

## Superficie del kernel

Esto es el kernel entero. No hay más verbos.

`describe` · `mem.claim` · `mem.read` · `mem.write` · `core.claim` · `exec` ·
`irq.install` · `irq.install_raw` · `dma.allow` · `release`

## Reglas de código

1. **`kernel-core/` no puede contener código específico de arquitectura.** Nada de
   `#[cfg(target_arch)]`, `core::arch` ni `asm!`. Todo pasa por el trait `Platform`.
   `./scripts/check-frontera.sh` lo verifica y CI debe fallar si se rompe.
2. **Las dos arquitecturas arrancan siempre.** Un cambio que rompe una de las dos no
   se mergea. No es solo portabilidad: x86 tiene modelo de memoria fuerte y esconde
   barreras faltantes que ARM expone.
3. **El protocolo no lleva nombres de registros horneados.** x86_64 tiene RAX, ARM64
   tiene X0–X30, RISC-V tiene x0–x31. La máquina informa qué tiene (P4).
4. **Salida del UART en ASCII puro.** Manda bytes, no texto: los acentos salen rotos.
5. Sin dependencias externas salvo que haya una razón fuerte. El kernel es `no_std`.

## Estado actual

**Hito 1 completo:** arranca por UEFI en x86_64 y aarch64, y habla por el cordón
umbilical. Verificado en QEMU en las dos arquitecturas. ~4 KB por kernel.

## Lo que sigue

1. `describe` — devolver el hardware real: mapa de memoria (de UEFI antes de
   `ExitBootServices`), núcleos, dispositivos PCIe. Consultable, no un volcado fijo (D16).
2. El protocolo CBOR sobre el UART, reemplazando el texto de arranque.
3. `mem.claim` y `exec`: reclamar memoria física, subir código máquina, saltar,
   devolver el estado de los registros.
4. Captura de faults como datos estructurados (D7).

## Cómo correrlo

```bash
./scripts/run-x86_64.sh      # Ctrl-A luego X para salir
./scripts/run-aarch64.sh
./scripts/check-frontera.sh
```
