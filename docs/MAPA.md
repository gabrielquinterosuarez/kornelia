# Dónde vive cada cosa

> Para no tener que reconstruirlo leyendo todo. `docs/DISENO.md` dice **por qué**
> es así; esto dice **dónde está**.

## Los cuatro crates

| Crate | Qué es | Regla |
|---|---|---|
| `kernel-core/` | Todo lo portable: protocolo, reclamos, faults, mapa de memoria, ACPI. | **Cero `asm!`, cero `target_arch`, y no puede nombrar a `boot-uefi`** (D23/D24). |
| `boot-uefi/` | Cómo se le pide la máquina al firmware. Compartido por las dos arquitecturas. | Sin `asm!`: UEFI no varía por arquitectura. |
| `kernel-x86_64/` | Lo que solo existe en x86: GDT, IDT, APIC, VT-d, trampolín de arranque. | |
| `kernel-aarch64/` | Lo mismo del otro lado: tabla de vectores, GIC, PSCI, SMMUv3 (pendiente). | |

Lo verifica `./scripts/check-boundary.sh`, dentro del portón.

## Por dónde entrar según lo que se quiera tocar

| Si vas a tocar… | Empezá por |
|---|---|
| El protocolo o un verbo | `kernel-core/src/protocol.rs` — están todos, uno por función. |
| Lo que el kernel le pide a una arquitectura | `kernel-core/src/platform.rs` — el trait `Platform` es **la** frontera. |
| Memoria del agente | `kernel-core/src/claims.rs` + `paging.rs` de cada arquitectura. |
| Correr código del agente | `exec.rs` de cada arquitectura. El de x86 tiene el `iretq` a anillo 3. |
| Faults | `kernel-core/src/fault.rs` (formato) + `idt.rs` / `vectors.rs` (captura). |
| Otros núcleos | `kernel-core/src/work.rs` (buzón) + `smp.rs` de cada arquitectura. |
| Interrupciones | `irq.rs` de cada arquitectura + `kernel-core/src/handlers.rs`. |
| IOMMU | `kernel-x86_64/src/iommu.rs`. En aarch64 **no existe todavía** (deuda 14). |
| El segundo canal | `kernel-core/src/channel.rs`. |
| Lo que el agente ve de la máquina | `kernel-core/src/acpi.rs` (leer) + `protocol.rs::describe` (publicar). |

## El portón

`./scripts/check.sh` es todo lo que CI corre. Cinco pasos, en orden:

1. **Frontera** (`check-boundary.sh`) — que los crates portables no filtren arquitectura.
2. **Idioma** (`check-language.py`) — identificadores en inglés. Es una lista de
   palabras: cuando se cuela una que no está, se **agrega a `FORBIDDEN`** en vez
   de solo corregir el identificador.
3. **89 tests** de `kernel-core`.
4. **Compilan las dos.**
5. **Arrancan las dos en QEMU y contestan el protocolo**, con `scripts/client.py`.

El paso 5 es el que atrapa lo que importa: que compile no prueba nada. Cada
prueba del cliente está escrita para **no poder pasar por accidente** — si el
kernel no hiciera lo que dice, la prueba se cuelga o la máquina se queda muda.

```bash
./scripts/check.sh                   # el portón entero (varios minutos)
SKIP_QEMU=1 ./scripts/check.sh       # sin bootear, para iterar rápido
./scripts/client.py --arch aarch64 --smp 4 --dma --on-core --supervised
```

`cargo` no está en el PATH: `export PATH="$HOME/.cargo/bin:$PATH"`.

## Cómo se prueba algo nuevo

El patrón que sigue todo el proyecto: **la prueba no le cree al kernel.**

- ¿El agente corre sin privilegio? Que ejecute `cli` y vuelva como fault.
- ¿Corrió en otro núcleo? Que **el código del agente** informe en qué núcleo está.
- ¿El IOMMU bloquea? Que un aparato de verdad intente escribir y la memoria quede
  intacta — y comprobar aparte que sin IOMMU esa escritura sí ocurre, porque
  "bloqueado" y "nunca pasó nada" se ven iguales desde afuera.

Si una prueba solo comprueba lo que el kernel dice de sí mismo, no prueba nada.

## Qué se puede romper sin darse cuenta

- **Los offsets del bloque por núcleo** (`percpu.rs`) los usa el ensamblador a
  mano. Hay `assert!` de tiempo de compilación; agregar un campo los corre.
- **El orden de `REGISTERS`** es el orden en que el ensamblador deja los valores.
  Cambiar uno sin el otro hace que el kernel informe un registro con el nombre de
  otro — ya pasó una vez, en aarch64.
- **Los selectores de la GDT** están escritos a mano en el ensamblador de `exec`,
  con `assert!` que los atan a `gdt.rs`.
- **El serie va crudo** (D26): `-serial stdio`, nunca `mon:stdio`. Con `mon:`,
  QEMU se come el byte `0x01` y por ahí viaja CBOR.
