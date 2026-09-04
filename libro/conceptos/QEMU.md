---
tipo: concepto
estado: pendiente
dificultad: 2
principios: [P1, P4]
decisiones: [D8, D22, D26]
practicas: [P00-Armar-la-VM-de-practicas, P07-El-monitor-de-QEMU-cuando-el-kernel-se-murio]
capitulos: [00-Como-mirar-una-maquina, 11-Reset-vector-firmware-BIOS-y-UEFI]
---

# QEMU

> El programa que arma la máquina donde corre este kernel: procesador, RAM y una lista de
> aparatos que vos elegís. Es la herramienta, no el concepto.

La teoría —emulación contra virtualización, atrapar y emular, VT-x y EL2, virtio,
passthrough— está en [[Maquina-virtual]] y no se repite acá. **Leela primero.** Esta nota es
sobre *usar* QEMU: qué banderas importan, cómo se mira una máquina que se murió, y qué
aparatos lleva a propósito la máquina de prueba de este proyecto.

## Qué problema resuelve

Escribir un kernel es escribir el programa que manda en la máquina, y **el programa que
manda en la máquina no se puede depurar desde adentro**: cuando se rompe, se lleva puesto
al depurador. QEMU corre el kernel **como un proceso normal de tu Debian**, así que lo que
adentro es "la máquina se colgó", desde afuera es un proceso al que le podés preguntar
todo: los registros, la memoria física, el mapa del bus.

Y arma la máquina **por partes**. Que el IOMMU y el motor de DMA sean dos banderas de
línea de comandos es lo que hace posible probar contra hardware que no tenés.

## Cómo funciona

Una invocación de QEMU es una lista de piezas. Las que importan:

| Bandera | Qué arma | Ojo con |
|---|---|---|
| `-machine` | La **placa**: qué chipset, dónde arranca la RAM, qué hay cableado. `q35` en x86, `virt` en ARM. | En ARM no hay un default razonable: sin `-machine` no arranca nada. Sub-opciones con coma: `virt,iommu=smmuv3,acpi=off`. |
| `-cpu` | El modelo de procesador, o sea **qué capacidades tiene**. | El default `qemu64` es **más austero que cualquier silicio real**: no tiene páginas de 1 GiB. `-cpu max` = todo lo que QEMU sepa emular. `-cpu host` = las del anfitrión (solo con KVM). |
| `-m` | RAM. `-m 512` son 512 MiB. | El default es chico (128 MiB) y no avisa. |
| `-smp` | Cuántos núcleos. `-smp 4`. | Sin esto hay uno solo y `core.claim` no tiene qué reclamar. |
| `-device` | Enchufar un aparato al bus. | Es la bandera que hace útil a QEMU para un kernel: elegís el hardware. |
| `-drive` | Un archivo del anfitrión que adentro se ve como un disco. | `if=none,id=x` lo deja **sin conectar**, para que un `-device` lo tome. |
| `-serial` | A dónde sale el puerto serie. | Ver D26 más abajo: `stdio` **crudo**. |
| `-accel kvm` | Que el silicio ejecute de verdad. | Solo misma arquitectura. Ver [[Maquina-virtual]]. |
| `-display none` | Sin ventana. | Sin esto se abre una ventana vacía. |
| `-no-reboot` | Que un triple fault **apague** en vez de reiniciar. | Sin esto, un kernel roto reinicia para siempre y no se ve el error. |
| `-s -S` | Servidor de gdb en el puerto 1234, **congelado antes de la primera instrucción**. | Ver abajo. |

La forma normal de `-drive` + `-device` engaña la primera vez: son **dos** piezas, el medio
y el controlador. El archivo se declara sin conexión y después se le enchufa un
controlador NVMe, o SATA, o virtio. Es exactamente cómo está armado en una máquina real,
y por eso `-drive file=...,if=none,id=payload` y `-device nvme,...,drive=payload` son dos
líneas y no una.

## El monitor: lo más parecido a un debugger cuando el kernel se murió

QEMU tiene una consola propia que habla **de la máquina**, no de lo que corre adentro. Es
la herramienta más importante de esta nota, porque funciona igual con la máquina colgada.

Se llega con `-monitor stdio`, `-monitor telnet:127.0.0.1:5555,server,nowait`, o
—si el serie no está crudo— con `Ctrl-A C`.

| Comando | Qué muestra |
|---|---|
| `info registers` | Todos los registros, incluidos los de control: `CR3`, `CR0`, `TTBR0_EL1`, `SCTLR_EL1`. Es lo primero que se mira. |
| `info mem` | Qué rangos virtuales están mapeados y con qué permisos, **leyendo las tablas de páginas de verdad**. Ver [[MMU]]. |
| `info mtree` | El árbol de ruteo del bus: qué dirección física llega a quién. Es `/proc/iomem` desde afuera. Ver [[MMIO]]. |
| `info pci` | Los aparatos del bus, con sus BARs asignados. |
| `info irq` / `info pic` | Cuántas interrupciones se entregaron por vector. |
| `x/16xb 0x1000` | Volcar memoria. `x` toma direcciones **virtuales**, `xp` toma **físicas**. |
| `stop` / `cont` | Congelar y seguir. |
| `system_reset` / `quit` | Reiniciar, salir. |

Dos que valen por sí solas:

- **`info mtree` contesta "¿por qué mi lectura devuelve ceros?"** sin tocar el kernel. Si
  la dirección no aparece en el árbol, no hay nadie del otro lado.
- **`info registers` con la máquina muda** dice si el procesador está girando en un
  handler, esperando en `hlt`/`wfi`, o parado en una dirección que no es de tu código.

Y para lo pesado, `-s -S`: QEMU levanta un servidor de gdb y **no ejecuta ni una
instrucción** hasta que alguien se conecte. Del otro lado, `gdb -ex 'target remote :1234'`.
Así se puede poner un breakpoint en la primerísima instrucción del firmware, mucho antes de
que exista tu kernel — que es la única forma de depurar el arranque.

> [!warning] gdb necesita símbolos y tu kernel es un PE
> `target remote` funciona sin nada, pero solo da direcciones. Para ver nombres de
> funciones hay que cargarle los símbolos aparte, y ahí aparece que el binario es
> `.efi`, no un ELF: ver [[ELF-y-PE]].

## Qué se ve distinto entre emulado y con KVM

No es solo la velocidad, y por eso el proyecto tiene las dos formas de correr
(`./scripts/client.py --kvm`). Lo que cambia:

| | Emulado (TCG) | Con KVM |
|---|---|---|
| Otra arquitectura | Sí: aarch64 sobre x86. Es lo que sostiene D22. | No. |
| Ordenamiento de memoria | **Prácticamente secuencial.** Un bug de barreras faltantes no aparece. | El del silicio de verdad. Los bugs aparecen. Ver [[42-Ordenamiento-de-memoria]]. |
| Tiempo | 10 a 100 veces más lento, y el reloj virtual **no avanza igual** que el de afuera. | Casi nativo. |
| Registros de tiempo | El `TSC` / `CNTPCT_EL0` los inventa el emulador: suben parejo, sin ruido. | Los del procesador real, con su ruido y sus saltos. |
| El monitor | Ve todo. | Ve todo igual: `info registers` funciona porque KVM se los pide al kernel del anfitrión. |
| Un `hlt` / `wfi` | El proceso deja de quemar CPU. | Idem, pero la latencia de despertar es real. |

De ahí salen dos síntomas simétricos, y saber cuál es cuál ahorra un día:

- **Anda emulado, se rompe con KVM** → casi siempre falta una barrera. Es un bug **real**
  que la emulación tapaba.
- **Anda con KVM, se rompe emulado** → casi siempre un plazo. `exec {deadline_ms}` medido
  en tiempo real se vence cuando todo va cien veces más lento. Ver [[39-Plazos-y-cortes]].

## Cómo lo hace Kornelia

Las dos máquinas de prueba están en `scripts/run-x86_64.sh` y `scripts/run-aarch64.sh`, y
**llevan aparatos a propósito**. No son adorno: cada uno existe para que algo del kernel se
pueda probar contra hardware en vez de contra la palabra del kernel (P4, y
[[58-Una-prueba-que-no-puede-pasar-por-accidente]]).

| Bandera | Por qué está |
|---|---|
| `scripts/run-x86_64.sh:80#-cpu max` | El default `qemu64` **no tiene páginas de 1 GiB**, que es lo que D12 usa para el identity map. El default de QEMU es más austero que el hardware real, no al revés. |
| `scripts/run-x86_64.sh:84#-device intel-iommu` y `scripts/run-aarch64.sh:40#MACHINE=virt,iommu=smmuv3` | D8: el IOMMU va encendido. Sin él, `dma.allow` no se puede probar contra nada. Los dos hacen lo mismo y no se parecen: ver [[47-IOMMU-VT-d-y-SMMUv3]]. |
| `scripts/run-x86_64.sh:90#-device edu,dma_mask=0xffffffffffff` | `edu` es un **motor de DMA que se maneja con cuatro escrituras**: es el aparato que hace el DMA que el IOMMU tiene que bloquear. El `dma_mask` no es opcional — ver abajo. |
| `scripts/run-x86_64.sh:95#-device nvme,serial=kornelia,drive=payload` | Un disco NVMe de verdad: es de donde D19 dice que el cargador se trae el resto, y es el aparato contra el que se prueba PCIe entero, BARs incluidos. Ver [[NVMe]]. |
| `scripts/run-aarch64.sh:87#-cpu cortex-a57 -m 512` | Un modelo concreto de ARM, con RAM explícita: en `virt` el default no alcanza. |
| `NO_ACPI=1` → `virt,...,acpi=off` | Arranca **sin tablas de ACPI**, así el firmware deja un device tree en su lugar. Es la única forma de ejercitar el otro dialecto, y el portón lo exige. Ver [[16-El-otro-dialecto-device-tree]]. |
| `"$@"` al final de las dos | Todo lo que le pases al script se le pasa a QEMU. Ahí van `-smp 4`, `-s -S`, `-monitor telnet:...`. |

### D26: el serie va crudo

`scripts/run-x86_64.sh:73#SERIAL=(-serial stdio)` — **sin** el prefijo `mon:`, y no es un
olvido.

Con `mon:stdio`, QEMU multiplexa su monitor sobre la misma terminal, y ese multiplexor
**se come el byte `0x01`** (Ctrl-A) como escape junto con el que le sigue. Por ese puerto
viaja CBOR (D6) y, más adelante, código máquina: ahí `0x01` es un byte tan legítimo como
cualquier otro, y perderlo **corrompe el mensaje en silencio**.

El costo es que `Ctrl-A X` no sale. **De Kornelia se sale con `Ctrl-C`.** Y si querés el
monitor, se pide por otro lado: `-monitor telnet:127.0.0.1:5555,server,nowait`.

Con `SOCKET=<ruta>` el cable sale por un socket Unix en vez de por la terminal, y entonces
la máquina **sobrevive a que el cliente se vaya** (D14): mandás algo largo, cortás, y
volvés a buscar el resultado.

## Cómo se ve roto

> [!danger] Una prueba que pasa porque no pasa nada
> El aparato `edu` **recorta la dirección de DMA a 28 bits** si no se le dice otra cosa. En
> aarch64 la RAM arranca en 1 GiB, así que ningún destino podía llegar nunca: el IOMMU
> parecía estar bloqueando perfectamente, y en realidad no ocurría nada.
>
> "Bloqueado" y "nunca pasó nada" **se ven idénticos desde afuera**. Se encontró booteando
> sin IOMMU y mirando: si la cosa bloqueada no ocurre tampoco sin el bloqueo, la prueba no
> prueba. En x86_64 no se veía porque ahí la RAM arranca en cero. De ahí
> `dma_mask=0xffffffffffff`, en las dos.

| Síntoma | Causa |
|---|---|
| El kernel se reinicia una y otra vez y no se lee nada. | Falta `-no-reboot`. Con él, el triple fault apaga y el último mensaje queda en pantalla. |
| Un `mem.write` grande corrompe el mensaje. | Puede ser el multiplexor comiéndose el `0x01`: revisá que `-serial` **no** tenga `mon:` (D26). |
| Un mecanismo "no funciona" en QEMU. | Puede no estar implementado. Antes de culpar al kernel, buscá si el aparato emulado tiene esa capacidad — el GIC de esta máquina no tiene extensiones de seguridad, y por eso el FIQ no sirvió. |
| `Could not access KVM kernel module`. | Falta `/dev/kvm`, el usuario no está en el grupo `kvm`, o ya estás adentro de una VM sin virtualización anidada. |
| El kernel se cuelga usando páginas de 1 GiB. | El `-cpu` por omisión no las tiene. `-cpu max`. |
| El aparato aparece en `info pci` pero su BAR devuelve ceros. | O el ancho del acceso está mal ([[MMIO]]), o el BAR no está mapeado en las tablas del kernel. `info mtree` decide cuál de las dos. |
| `Ctrl-A X` no sale de QEMU. | Es a propósito (D26). `Ctrl-C`. |
| Todo anda pero `core.claim` dice que no hay núcleos. | Falta `-smp`. |

## Práctica

- [[P00-Armar-la-VM-de-practicas]] — *(construir)* la VM desechable, con snapshots para volver después de romperla.
- [[P07-El-monitor-de-QEMU-cuando-el-kernel-se-murio]] — *(romper)* colgar Kornelia a propósito y sacarle `info registers`, `info mem` y `info mtree` al cadáver.
- [[P03-Emulado-contra-KVM]] — *(mirar)* el mismo `exec` de las dos formas.

## Recordar #flashcards/conceptos

¿Por qué `-drive` y `-device` son dos banderas separadas para un solo disco?::Porque son dos piezas: el medio (un archivo del anfitrión, declarado `if=none,id=x`) y el controlador que lo expone al bus (`-device nvme,drive=x`). Es cómo está armada una máquina real.

¿Cuál es el comando del monitor que contesta "¿por qué mi lectura devuelve ceros?"::`info mtree`, que muestra el árbol de ruteo del bus: qué dirección física llega a quién. Si la dirección no está ahí, no hay nadie del otro lado.

¿Qué hace `-s -S`?::Levanta un servidor de gdb en el puerto 1234 y **congela la máquina antes de la primera instrucción**, así se puede poner un breakpoint en el arranque del firmware, mucho antes de que exista tu kernel.

¿Por qué Kornelia usa `-serial stdio` y no `mon:stdio`?::Porque el multiplexor del monitor se come el byte `0x01` como escape, y por ese cable viaja CBOR y código máquina, donde `0x01` es un byte legítimo. Perderlo corrompe el mensaje en silencio (D26). El costo es que se sale con `Ctrl-C` y no con `Ctrl-A X`.

¿Por qué el default `-cpu qemu64` es un problema?::Porque es **más austero que cualquier silicio real**: no tiene páginas de 1 GiB, que es lo que usa el identity map de D12. Cualquier x86_64 de verdad las tiene desde 2008.

¿Qué aparatos lleva a propósito la máquina de prueba y para qué?::`intel-iommu` / `iommu=smmuv3` para D8; `edu` (un motor de DMA de cuatro escrituras) para que haya un DMA real que el IOMMU tenga que bloquear; un disco `nvme` para probar PCIe y BARs de verdad.

¿Qué hace `dma_mask=0xffffffffffff` en el aparato `edu`?::Impide que recorte las direcciones de DMA a 28 bits. Sin eso, en aarch64 —donde la RAM arranca en 1 GiB— ningún DMA podía llegar nunca, y el IOMMU parecía bloquear perfectamente cuando en realidad no pasaba nada.

## Ver también

- [[Maquina-virtual]] — la teoría: emulación contra virtualización, trap-and-emulate, virtio.
- [[MMIO]] · [[MMU]] · [[NVMe]] · [[ELF-y-PE]]
- [[00-Como-mirar-una-maquina]] · [[47-IOMMU-VT-d-y-SMMUv3]] · [[Indice-de-sintomas]]
