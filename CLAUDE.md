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

**29 decisiones (D1–D29) están cerradas en `docs/DISENO.md`, cada una con su
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
- **D24** La frontera son **dos ejes**: arquitectura (`asm!`) y entorno de arranque (UEFI). El código UEFI va en `boot-uefi/`, compartido; los tipos normalizados en `kernel-core/`.
- **D25** Al firmware se le pide todo (mapa de memoria, ACPI/DT, blob) **antes** de `ExitBootServices`, que se llama una sola vez. Después no hay segunda oportunidad, y solo el firmware sabe leer FAT32.
- **D29** En el núcleo del kernel **manda el kernel**: una interrupción ahí tiene prioridad sobre el código del agente. En un núcleo `dedicated` la prioridad la decide el agente. Implica que en el núcleo del protocolo el agente corre `supervised` — si quiere `raw`, que reclame uno propio.
- **D28** `listen` es el verbo **once**: el agente arma un buzón en memoria y se lo entrega como segundo canal. Se agregó en vez de esconderlo en un acuerdo implícito — un número redondo no es un principio.
- **D27** El agente **declara** si su código corre `supervised` (anillo bajo, no puede colgar la máquina) o `raw` (privilegio completo). El kernel ofrece los dos y no elige (P6): `mode` es obligatorio en `exec`, porque un valor por omisión sería el kernel eligiendo. **Solo cubre `exec`:** un handler de `irq.install` corre siempre privilegiado porque el hardware no entrega interrupciones sin privilegio.
- **D26** El serie va **crudo**: `-serial stdio`, nunca `mon:stdio`. El multiplexor se come el `0x01` como escape y por ahí viaja CBOR. Se sale de QEMU con `Ctrl-C`.

## Superficie del kernel

Esto es el kernel entero. **Son once** (D28: el once se agregó porque D17
prometía algo que los diez no podían pedir).

`describe` · `mem.claim` · `mem.read` · `mem.write` · `core.claim` · `exec` ·
`irq.install` · `irq.install_raw` · `dma.allow` · `release` · `listen`

## Reglas de código

1. **Ni `kernel-core/` ni `boot-uefi/` pueden contener código específico de
   arquitectura.** Nada de `#[cfg(target_arch)]`, `core::arch` ni `asm!`. Todo pasa
   por el trait `Platform`. `./scripts/check-boundary.sh` lo verifica y CI debe fallar
   si se rompe. Además `kernel-core/` no puede nombrar a `boot-uefi` (D24: el núcleo
   no sabe cómo arrancó).
2. **Las dos arquitecturas arrancan siempre.** Un cambio que rompe una de las dos no
   se mergea. No es solo portabilidad: x86 tiene modelo de memoria fuerte y esconde
   barreras faltantes que ARM expone.
3. **El protocolo no lleva nombres de registros horneados.** x86_64 tiene RAX, ARM64
   tiene X0–X30, RISC-V tiene x0–x31. La máquina informa qué tiene (P4).
4. **Salida del UART en ASCII puro.** Manda bytes, no texto: los acentos salen rotos.
5. Sin dependencias externas salvo que haya una razón fuerte. El kernel es `no_std`.
6. **El código va en inglés; el español es solo para humanos.** Nombres de
   archivos, tipos, campos, funciones, constantes, variables, etiquetas de
   ensamblador y nombres de test: inglés. Comentarios y documentación: español.
   Los textos que salen por el UART también en español, porque son para leer en
   una terminal — pero en ASCII puro (regla 4).

   **Lo verifica `./scripts/check-language.py`, dentro del portón.** Esta regla
   estuvo escrita acá y se rompió igual, dos veces: el proyecto ya sabe que una
   regla que no se comprueba es una intención (D23). El chequeo es una lista de
   palabras y por eso no es completo — cuando se cuela una que no está, se
   agrega a la lista y deja de poder volver.

## Estado actual

Arranca por UEFI en x86_64 y aarch64, le toma la máquina al firmware y **habla
CBOR** por el cordón umbilical. Corre sobre pila y tablas de páginas propias, y
captura los faults en vez de reiniciarse. **El núcleo que atiende duerme entre
pedidos**: el cable serie tiene timbre (interrupción), así que ya no gira
preguntando.

**Diez de los once verbos andan:** `describe`, `mem.claim`, `mem.read`,
`mem.write`, `release`, `exec`, `core.claim`, `listen` y **`irq.install` /
`irq.install_raw`** (el `raw` solo en x86_64). El agente sube código máquina, lo corre, y
si falla **el fault vuelve como respuesta en vez de matar la máquina** (P5) —
ni siquiera destruyendo el puntero de pila, porque las excepciones entran en una
pila aparte (IST en x86_64, `SP_EL1` en aarch64).

**D27 está entero:** el agente declara con qué privilegio corre y `exec supervised`
entra a anillo 3 / EL0. Ahora `cli` —lo único de lo que el kernel no podía
volver— vuelve como fault estructurado. La pila sale del final del reclamo del
agente y se vuelve por una ventanilla (`int 0x80` / `svc #0`) cuyos bytes
publica `describe`, así el agente no los tiene horneados (P4).

`describe` sirve mapa de memoria, tablas, reclamos, núcleos, controlador de
interrupciones y PCIe, leídos de ACPI — más el acuerdo de `exec`: qué modos hay
y con qué bytes se vuelve de `supervised`.

`core.claim` arranca los otros núcleos: PSCI en aarch64, INIT/SIPI más un
trampolín de 16→32→64 bits en x86_64. **Y ahora reciben trabajo:** `exec {core}`
deja el pedido en un buzón por núcleo, el núcleo **duerme** hasta que lo
despierta un IPI/SGI, corre y contesta. Lo comprueba el código del agente
diciendo en qué núcleo está.

Falta uno: `dma.allow`, el IOMMU.

El portón es `./scripts/check.sh`: frontera + idioma + 89 tests + compila las
dos + las bootea en QEMU y les habla el protocolo con `scripts/client.py`.
Corrélo antes de commitear; CI corre exactamente ese script.

## Lo que sigue

Queda **un solo verbo** sin hacer. Las preguntas abiertas están en
`docs/DISENO.md` §8.

1. **`dma.allow`** — el IOMMU, el único verbo que falta. El más grande del proyecto y el más
   específico de cada fabricante; es lo que más gana con silicio real.
2. **Hacer cumplir la regla de D29**, que recién ahora se puede: en el núcleo del protocolo el
   agente debería correr siempre `supervised`, y si quiere `raw` que reclame un núcleo. Antes
   exigirlo dejaba `raw` sin ningún lugar donde correr; ahora `exec` elige núcleo, así que no.
   Es un cambio chico en el verbo y grande en las pruebas — casi todas corren `raw` en el núcleo
   que atiende.
3. **`exec` en otro núcleo es sincrónico** (deuda 13): el del protocolo espera con un tope, así
   que un trabajo largo se informa igual que un núcleo perdido. Falta la forma asincrónica.

Deudas anotadas en `docs/DISENO.md` §7. La más viva: **`exec` en otro núcleo es
sincrónico y con tope**, así que un trabajo largo se informa igual que uno
perdido.

## Cómo correrlo

```bash
./scripts/run-x86_64.sh      # Ctrl-C para salir (NO Ctrl-A X: ver D26)
./scripts/run-aarch64.sh
./scripts/client.py --what memory   # hablarle el protocolo
./scripts/client.py --supervised    # D27: correr sin privilegio y ver el fault
./scripts/client.py --smp 4 --on-core   # mandar el codigo a otro nucleo
./scripts/check.sh                  # el porton entero
```
