# Dónde vive cada cosa

> Para no tener que reconstruirlo leyendo todo. `docs/DISENO.md` dice **por qué**
> es así; esto dice **dónde está**.

## Los cinco crates

| Crate | Qué es | Regla |
|---|---|---|
| `blob/` | `blob.bin`: la distribución reemplazable de D19/D20. **No es el kernel** — es código del agente pre-armado, y sólo puede pedirle cosas a la máquina por los once verbos. | Sale a **binario plano**, no a un ejecutable: sin GOT y sin `.bss`, con la entrada en el byte cero. Lo comprueba `blob/blob.ld`. |
| `kernel-core/` | Todo lo portable: protocolo, reclamos, faults, mapa de memoria, ACPI y device tree. | **Cero `asm!`, cero `target_arch`, y no puede nombrar a `boot-uefi`** (D23/D24). |
| `boot-uefi/` | Cómo se le pide la máquina al firmware. Compartido por las dos arquitecturas. | Sin `asm!`: UEFI no varía por arquitectura. |
| `kernel-x86_64/` | Lo que solo existe en x86: GDT, IDT, APIC, VT-d, trampolín de arranque. | |
| `kernel-aarch64/` | Lo mismo del otro lado: tabla de vectores, GIC, PSCI, SMMUv3. | |

Lo verifica `./scripts/check-boundary.sh`, dentro del portón.

## Por dónde entrar según lo que se quiera tocar

| Si vas a tocar… | Empezá por |
|---|---|
| El protocolo o un verbo | `kernel-core/src/protocol.rs` — están todos, uno por función. |
| Lo que el kernel le pide a una arquitectura | `kernel-core/src/platform.rs` — el trait `Platform` es **la** frontera. |
| Memoria del agente | `kernel-core/src/claims.rs` + `paging.rs` de cada arquitectura. |
| Correr código del agente | `exec.rs` de cada arquitectura. El de x86 tiene el `iretq` a anillo 3. |
| Faults | `kernel-core/src/fault.rs` (formato) + `idt.rs` / `vectors.rs` (captura). |
| Tocar memoria que puede rechazar el acceso | `guarded.rs` de cada arquitectura. Es el punto de recuperación de `exec`, usado afuera de `exec`. |
| Otros núcleos | `kernel-core/src/work.rs` (buzón) + `smp.rs` de cada arquitectura. |
| Interrupciones | `irq.rs` de cada arquitectura + `kernel-core/src/handlers.rs`. |
| IOMMU | `kernel-x86_64/src/iommu.rs` (VT-d) y `kernel-aarch64/src/smmu.rs` (SMMUv3). Hacen lo mismo y no se parecen en nada: empezar por el de x86, que es el más simple. |
| El segundo canal | `kernel-core/src/channel.rs`. |
| La pantalla (D5) | `kernel-core/src/screen.rs` (dibujar) + `boot-uefi/src/lib.rs::find_screen` (dónde está) + `kernel-core/src/font.rs`, que **se genera** con `./scripts/make-font.py`. |
| El reloj | `clock` en el `main.rs` de cada arquitectura. En x86 incluye la calibración contra el contador de ACPI. |
| Lo que va adentro del blob | `blob/src/main.rs` (qué hace) + `blob/src/nvme.rs` (el driver de disco) + `blob/src/kernel.rs` (cómo le pide verbos) + `blob/src/gate.rs` (las dos puertas) + `blob/blob.ld` (cómo se arma). Se compila con `./scripts/build-blob.sh <arch>`. |
| El blob de arranque | `boot-uefi/src/lib.rs::load_blob` (traerlo del disco) + `kernel-core/src/lib.rs::run_blob` (ventana de rescate y ejecución) + `protocol.rs::open_blob_gate` (con qué le pide cosas al kernel). |
| Un driver del agente (NVMe, red) | `scripts/client.py`. **No son del kernel** (D4): usan sólo los once verbos y viven del lado del cliente. Buscar la clase `Nvme` o la clase `E1000`. |
| El transporte de D5 | `scripts/client.py`: `transport_program` (el bucle, en código máquina) y la clase `Asm` que lo ensambla. El buzón que usa es `kernel-core/src/channel.rs`. |
| Lo que el agente ve de la máquina | `kernel-core/src/acpi.rs` y `fdt.rs` (los dos dialectos en que una máquina se describe) + `tables.rs::describe` (elegir cuál) + `protocol.rs::describe` (publicar). |

## El portón

`./scripts/check.sh` es todo lo que CI corre. Nueve pasos, en orden:

1. **Frontera** (`check-boundary.sh`) — que los crates portables no filtren arquitectura.
2. **Idioma** (`check-language.py`) — identificadores en inglés. Es una lista de
   palabras: cuando se cuela una que no está, se **agrega a `FORBIDDEN`** en vez
   de solo corregir el identificador.
3. **Las citas del libro** (`libro/scripts/check-citas.py`) — que lo que el libro
   cita del código siga estando en la línea que dice. Es el más barato: no
   compila nada. Si falla porque el código se corrió, `--fix` lo arregla.
4. **Los tests** de `kernel-core`.
5. **Compilan las dos.**
6. **Arrancan las dos en QEMU y contestan el protocolo**, con `scripts/client.py`.
7. **El blob compilado** (`blob/`) recorre el bus, maneja el disco y dice de qué tamaño es, en las dos.
8. **El blob se carga, corre, le habla al kernel y se puede cancelar** (D18), en las dos.
9. **Y aarch64 arranca una vez más sin ACPI**, para que se describa por device
   tree. Ahí se le exige el IOMMU contra un aparato de verdad, que es la prueba
   que usa todo lo que sale de la descripción junto.

El paso 6 es el que atrapa lo que importa: que compile no prueba nada. Cada
prueba del cliente está escrita para **no poder pasar por accidente** — si el
kernel no hiciera lo que dice, la prueba se cuelga o la máquina se queda muda.

```bash
./scripts/check.sh                   # el portón entero (varios minutos)
SKIP_QEMU=1 ./scripts/check.sh       # sin bootear, para iterar rápido
./scripts/client.py --arch aarch64 --smp 4 --dma --on-core --supervised
./scripts/client.py --arch aarch64 --no-acpi --dma   # el otro dialecto
./scripts/client.py --console                        # hablarle a mano
./scripts/client.py --net --udp --transport          # la placa, y D5 entero
./scripts/client.py --screen                         # que dibuje, leido de una foto
NETPORT=15555 ./scripts/run-x86_64.sh                # fijar el puerto del host
./scripts/client.py --arch x86_64 --write-blob /tmp/blob.bin
BLOB=/tmp/blob.bin ./scripts/run-x86_64.sh           # con blob (D18)
./scripts/build-blob.sh x86_64                       # compilar blob/ de verdad
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
- **Las citas del libro al código** (`libro/`). El libro cita `archivo.rs:línea#ancla`;
  mover código deja la cita apuntando a otra línea. Lo atrapa el paso 3 del portón,
  y `./libro/scripts/check-citas.py --fix` lo arregla.

## El libro

`libro/` es un vault de Obsidian: un libro sobre kernels que usa este kernel como caso de
estudio. **No es documentación del proyecto** — `docs/` sigue siendo la verdad sobre el
diseño; el libro lo cita. La puerta es `libro/00-Empezar-aca.md` y el método,
`libro/El-metodo.md`. Detalles en `CLAUDE.md`, sección *El libro*.
