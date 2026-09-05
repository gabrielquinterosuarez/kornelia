---
tipo: concepto
estado: pendiente
dificultad: 3
principios: [P4]
decisiones: [D22, D26]
practicas: [P00-Armar-la-VM-de-practicas]
capitulos: [00-Como-mirar-una-maquina, 06-El-silicio-tiene-modos]
---

# Máquina virtual

> Una máquina entera —procesador, memoria, [[Aparato|aparatos]]— hecha de software. Adentro corre un kernel de verdad, que no sabe que no está solo.

Es donde vas a correr casi todo lo de este libro, así que conviene entender qué es de verdad, y sobre todo **en qué miente**. Ver también [[QEMU]], que es la implementación concreta que usa el proyecto.

## Qué problema resuelve

Un kernel es el programa que manda en la máquina. Para probar uno hace falta **una máquina entera**, y hasta hace veinte años eso significaba literalmente eso: una segunda computadora, un cable serie entre las dos, y reiniciar a mano cada vez que se colgaba.

Una máquina virtual es esa segunda computadora hecha de software. Lo que cambia no es la comodidad: es que **se puede volver atrás**. Un kernel roto se apaga y se vuelve a arrancar en dos segundos, con un estado idéntico al de la vez anterior. Sin eso, escribir un kernel sería un oficio de laboratorio.

## Cómo funciona

Hay dos formas de correr el código del huésped, y la diferencia importa:

| | **Emulación** | **Virtualización** |
|---|---|---|
| Qué hace | Un programa **lee cada instrucción y la simula**. | El silicio **ejecuta las instrucciones de verdad**. |
| Puede correr otra arquitectura | Sí: aarch64 sobre x86. | No: solo la misma. |
| Velocidad | 10 a 100 veces más lento. | Casi nativa. |
| En QEMU | Por omisión (TCG). | `-accel kvm` / `-enable-kvm`. |

### Cómo puede el silicio ejecutar "de verdad" el código de otro kernel

Esta es la parte que parece imposible: si el kernel huésped ejecuta instrucciones privilegiadas —cambiar la [[Tabla-de-paginas|tabla de páginas]], apagar [[Interrupcion|interrupciones]], tocar [[MMIO|registros de un aparato]]— ¿cómo no rompe la máquina real?

La respuesta clásica es **atrapar y emular** (*trap and emulate*): se corre al huésped **sin privilegio**, y cada vez que intenta algo privilegiado el silicio genera un [[Fault|fault]] que despierta al anfitrión, que lo simula y devuelve el control. Funciona, pero en x86 no alcanzaba: había instrucciones que sin privilegio **fallaban en silencio en vez de atrapar**, y una trampa que no salta no se puede emular.

Por eso Intel y AMD agregaron un modo entero para esto:

| Arquitectura | Cómo se llama | Dónde corre el hipervisor |
|---|---|---|
| x86_64 | VT-x (Intel) / SVM (AMD) | Un modo aparte, "raíz", debajo del anillo 0 |
| aarch64 | Extensiones de virtualización | **EL2**, un nivel entero arriba del kernel (EL1) |

En ARM se ve más limpio y explica mejor la idea: los [[Modo-privilegiado|niveles de excepción]] son EL0 (usuario), EL1 (kernel), **EL2 (hipervisor)**, EL3 ([[Firmware|firmware]]). El hipervisor no es un truco: es un escalón más de privilegio, previsto en el silicio.

Y la memoria necesita lo mismo: el huésped cree que traduce virtual → física, pero su "física" también es virtual. Se resuelve con **dos etapas de traducción** (*EPT* en Intel, *etapa 2* en ARM), que es exactamente el mismo mecanismo de dos etapas que usa un [[IOMMU|SMMU]] — y por eso un bug de configuración de etapa 2 aparece en los dos lados.

### Los aparatos

El procesador se puede virtualizar; los aparatos hay que **inventarlos**. Tres formas:

1. **Emular uno de verdad** — la VM se hace pasar por una placa de red Intel e1000 que existió. Funciona con cualquier huésped, y es lento: cada acceso a un registro es un fault.
2. **Paravirtualizar** (`virtio`) — un aparato que **no existe en el mundo físico**, diseñado para ser rápido de emular: colas en memoria compartida en vez de registros. El huésped necesita un [[Driver|driver]] que sepa de virtio. Es lo que usa la VM de [[P00-Armar-la-VM-de-practicas]] (`if=virtio`).
3. **Pasar el aparato real** (*passthrough*) — darle a la VM un aparato físico de verdad. Acá aparece el [[IOMMU]]: sin él, la VM podría hacer [[DMA]] a cualquier parte de la memoria del anfitrión. Es el uso original y la razón por la que existe el IOMMU en las máquinas de escritorio.

## VM, contenedor y unikernel

Se confunden todo el tiempo:

| | Qué aísla | Cuántos kernels hay |
|---|---|---|
| **Máquina virtual** | Una máquina entera. | Dos: el del anfitrión y el del huésped. |
| **Contenedor** (Docker) | Procesos, con `namespaces` y `cgroups`. | **Uno solo, compartido.** No hay máquina virtual en ningún lado. |
| **Unikernel** | Una aplicación fusionada con su kernel, en una VM. | Uno, y es de un solo programa. |

Un contenedor **no es una VM chiquita**: es un proceso de Linux con la vista recortada. Si el kernel se cuelga, se cuelgan todos los contenedores. Ver [[08-Monolitico-micro-exo-unikernel]].

## El canal que no es una red

Un huésped y su anfitrión necesitan hablarse, y la forma obvia —darle una IP al huésped y usar TCP— arrastra una pila entera: direcciones, ruteo, NAT, cortafuegos, DHCP. Todo eso para conectar dos programas que están **en la misma máquina física**, separados solo por una capa de software.

**AF_VSOCK** es la alternativa: una familia de direcciones entera, al mismo nivel que `AF_INET`, hecha para eso y nada más. En vez de una IP y un puerto, una dirección de vsock es un **CID** (*context ID*: el número del huésped en el bus) y un puerto. No hay ruteo porque no hay a dónde rutear: el único destino posible es el anfitrión.

Lo provee un aparato paravirtualizado (`vhost-vsock-pci`), o sea que **no existe en el mundo físico**: es de la misma familia que `virtio` de más arriba, un aparato inventado para que el huésped hable rápido con quien lo emula.

Se ve en el arranque de cualquier Debian moderno adentro de una VM sin ese aparato:

```
systemd-ssh-generator[5398]: Failed to query local AF_VSOCK CID: Cannot assign requested address
```

systemd intenta ofrecer SSH por vsock —así se entra a una VM que ni siquiera tiene red configurada— y al no encontrar el aparato, se calla y sigue. Inofensivo.

> [!tip] Por qué esto le importa a este libro
> Es la forma exacta del problema que plantea **D5**: *el [[UART]] es el cordón umbilical, no el transporte; el agente escribe el transporte rápido.* Un cable serie a 115.200 baudios alcanza para hablar, no para mover un volcado de memoria.
>
> vsock es cómo se ve ese transporte rápido **cuando la máquina es virtual**: un aparato de cola en memoria, sin cables, sin protocolo de red, y sin nada que descubrir. Y ahí está lo honesto — **Kornelia no lo tiene**. El transporte rápido que D5 le deja al agente no lo escribió nadie todavía, así que hoy el único camino sigue siendo el cordón umbilical.

## Cómo lo hace Linux

```bash
ls -l /dev/kvm                         # la puerta al modo de virtualizacion
grep -o 'vmx\|svm' /proc/cpuinfo | sort -u   # si tu silicio lo soporta
lsmod | grep kvm
```

KVM **no es un hipervisor completo**: es un módulo que le abre a un programa de usuario la puerta al modo de virtualización del silicio. El que arma la máquina —memoria, aparatos, arranque— es un programa normal: [[QEMU]], `crosvm`, `firecracker`. Es una decisión de diseño elegante: la parte que necesita privilegio es chica, y todo lo demás es un proceso que se puede matar.

## Cómo lo hace Kornelia

Kornelia **no virtualiza nada**: es un huésped, no un hipervisor. La VM es dónde vive durante todo el desarrollo.

| | |
|---|---|
| **Emulado por omisión** | `./scripts/run-x86_64.sh`, `./scripts/run-aarch64.sh` |
| **Sobre el silicio** | `./scripts/client.py --kvm` — "que el codigo lo ejecute el silicio de verdad, no la emulacion" (`scripts/client.py:3245#que el codigo lo ejecute el silicio`) |
| **Por qué las dos** | Emulado, corre aarch64 en una máquina x86 — que es lo que hace posible D22 (las dos arquitecturas en verde desde el primer commit) sin tener dos máquinas. |

Que `--kvm` exista como opción aparte no es un detalle de rendimiento: **es una prueba distinta**. Emulado, el código del agente lo interpreta un programa; con KVM lo ejecuta el procesador de verdad, con su [[Cache|caché]] real, su predicción de saltos y su ejecución fuera de orden. Un kernel que anda emulado y no anda con KVM tiene un bug de verdad, casi siempre de [[42-Ordenamiento-de-memoria|ordenamiento de memoria]].

## Cómo se ve roto

> [!danger] La trampa central: el emulador no es la máquina
> Una VM implementa **lo que su autor implementó**, y los huecos no se anuncian. Este proyecto tiene dos casos documentados donde el emulador determinó lo que se podía hacer, y los dos costaron días:

**1. El aparato que recortaba las direcciones en silencio.** El aparato de prueba `edu` recorta la dirección de [[DMA]] a 28 bits si no se le dice otra cosa. En aarch64 la RAM arranca en 1 GiB, así que **ningún destino podía llegar nunca**. El [[IOMMU]] parecía estar bloqueando perfectamente; en realidad no pasaba nada. Se arregló pidiéndole al aparato el ancho que hace falta:

```
-device edu,dma_mask=0xffffffffffff
```

En x86_64 no se veía, porque ahí la RAM arranca en cero. **"Bloqueado" y "nunca pasó nada" se ven idénticos desde afuera** — ver [[Indice-de-sintomas]].

**2. El controlador de interrupciones al que le faltan registros.** El GIC que emula QEMU no tiene extensiones de seguridad, así que `GICC_AIAR` lee cero y una interrupción del Grupo 1 no se puede reconocer. El GIC manda a una puerta que en esa máquina no está construida. Eso no es un bug de Kornelia ni de QEMU: es una limitación de la máquina, y el kernel **la publica** (`describe exec` trae `cancel`) en vez de prometer un corte que no llega. Es P4 aplicado a una carencia.

| Síntoma | Causa |
|---|---|
| Anda emulado, se rompe con `--kvm`. | Falta una barrera de memoria, o el código depende de que la emulación sea secuencial. Es un bug real que el emulador tapaba. |
| Anda con `--kvm`, se rompe emulado. | Casi siempre un timeout: la emulación es 10 a 100 veces más lenta y un plazo medido en tiempo real se vence. |
| Un mecanismo "no funciona" en QEMU. | Puede no estar implementado. Antes de culpar al kernel, buscá si el aparato emulado tiene esa capacidad. |
| El IOMMU parece bloquear todo perfectamente. | Comprobá que la cosa bloqueada **ocurre** sin el bloqueo. |
| `Could not access KVM kernel module`. | Falta `/dev/kvm`, el usuario no está en el grupo `kvm`, o ya estás adentro de una VM sin virtualización anidada. |

> [!tip] La regla que sale de todo esto
> Cuando algo no anda en una VM, hay **tres** sospechosos, no uno: tu código, el kernel, y **la máquina emulada**. El tercero se olvida siempre.

## Práctica

- [[P00-Armar-la-VM-de-practicas]] — *(construir)* la VM desechable, con snapshots para volver después de romperla.
- [[P03-Emulado-contra-KVM]] — *(mirar)* correr el mismo `exec` de las dos formas y comparar tiempos.

## Recordar #flashcards/conceptos

¿Qué es AF_VSOCK y qué problema resuelve?::Una familia de direcciones para que un huésped hable con su anfitrión **sin usar la red**: en vez de IP y puerto, un CID (el número del huésped en el bus) y un puerto. No hay ruteo porque el único destino posible es el anfitrión.

¿Diferencia entre emulación y virtualización?::En emulación un programa lee cada instrucción y la simula, así que puede correr otra arquitectura y va 10-100 veces más lento. En virtualización el silicio ejecuta las instrucciones de verdad, a velocidad casi nativa, pero solo de la misma arquitectura.

¿Cómo hace el silicio para que un kernel huésped no rompa la máquina real?::Atrapar y emular: el huésped corre sin privilegio y cada instrucción privilegiada genera un fault que despierta al anfitrión. En x86 no alcanzaba porque algunas instrucciones fallaban en silencio, y por eso se agregó VT-x/SVM; en ARM el hipervisor vive en EL2, un nivel de privilegio previsto en el silicio.

¿Un contenedor es una máquina virtual chica?::No. Es un proceso de Linux con la vista recortada por `namespaces` y `cgroups`. Hay **un solo kernel**, compartido: si se cuelga, se cuelgan todos los contenedores.

¿Qué es virtio y por qué existe?::Un aparato que no existe en el mundo físico, diseñado para ser rápido de emular: colas en memoria compartida en vez de registros. Emular un aparato real es lento porque cada acceso a un registro es un fault.

¿Por qué `client.py --kvm` es una prueba distinta y no solo más rápida?::Porque con KVM el código lo ejecuta el procesador de verdad, con su caché, su predicción de saltos y su ejecución fuera de orden. Un kernel que anda emulado y falla con KVM tiene un bug real, casi siempre de ordenamiento de memoria.

Cuando algo no anda dentro de una VM, ¿cuántos sospechosos hay?::Tres: tu código, el kernel, y **la máquina emulada**. El tercero se olvida siempre, y en este proyecto determinó dos veces lo que se podía hacer.

## Ver también

- [[QEMU]] · [[IOMMU]] · [[Modo-privilegiado]] · [[Fault]]
- [[08-Monolitico-micro-exo-unikernel]]
