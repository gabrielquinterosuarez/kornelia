---
tipo: concepto
estado: pendiente
dificultad: 3
principios: [P1, P4]
decisiones: [D12]
practicas: [P02-Medir-el-reloj-y-las-latencias, P15-Ver-el-false-sharing]
capitulos: [05-Caches-y-la-primera-mentira-util, 42-Ordenamiento-de-memoria, 45-Un-registro-no-es-RAM]
---

# Caché

> Memoria chica y rápida que guarda copias de la grande y lenta, y le miente al programa
> sobre cuál está usando. Es la primera mentira útil de la máquina.

La caché es invisible por diseño: ningún programa la nombra y todos dependen de ella. Un
kernel es de los pocos que **tiene que dejar de creerle**, porque hay memoria que no se
puede cachear y hay código que se escribe como dato.

## Qué problema resuelve

En la máquina donde se escribió [[P02-Medir-el-reloj-y-las-latencias]], la RAM está a
128,70 ns por acceso y la L1 a 2,86: **45 veces**. A 1,992 GHz eso son unos 260 ciclos de
espera contra 6.

Sin caché, el procesador estaría parado la enorme mayoría del tiempo, y todo el trabajo de
ejecutar fuera de orden y especular sería para nada. Con caché, el 95% de los accesos ni se
entera de que existe la RAM. Ver [[Jerarquia-de-memoria]].

## Cómo funciona

### 1. La unidad es la línea, no el byte

**No existe leer un byte de la RAM.** El bus mueve **líneas** de 64 bytes, y pedir un byte
trae los 64 que lo rodean. De ahí salen dos cosas de golpe:

- **La localidad espacial es gratis.** Recorrer un arreglo en orden paga un viaje cada 64
  bytes, no cada byte. Recorrerlo salteado paga uno por elemento.
- **La línea es la unidad de todo lo demás**: de la coherencia, de la invalidación, del
  desalojo. Cuando más abajo aparezca "la línea rebota", es esta línea.

El tamaño lo dice la máquina: la columna `COHERENCY-SIZE` de `lscpu -C`, que es 64 en
prácticamente todo lo que existe hoy.

### 2. La asociatividad: dónde puede ir una línea

Una caché no puede buscar en todos lados, porque comparar 8 MiB de etiquetas por acceso
sería más lento que la RAM. Entonces cada dirección tiene un **conjunto** fijo donde puede
vivir, y dentro del conjunto hay N lugares (*vías*).

| Asociatividad | Cuántos lugares por dirección | Problema |
|---|---|---|
| Directa (1 vía) | uno solo | dos direcciones que chocan se echan siempre, aunque la caché esté vacía |
| N vías | N | choca solo cuando hay N+1 direcciones activas en el mismo conjunto |
| Totalmente asociativa | todos | carísima; solo en cachés muy chicas (el TLB) |

En la máquina de la práctica: L1d de 8 vías, L2 de 4, L3 de 16 (`lscpu -C`, columna `WAYS`).

**La consecuencia práctica que muerde:** el conjunto se elige con unos bits del medio de la
dirección, así que recorrer un arreglo con un paso que es potencia grande de dos hace que
todos los accesos caigan en el mismo conjunto. Un arreglo de 4096 bytes de paso puede ser
diez veces más lento que uno de 4160, con la misma cantidad de accesos.

### 3. La política de escritura

| | Qué hace | Dónde se usa |
|---|---|---|
| *Write-through* | escribe en la caché **y** en el nivel de abajo | poco: gasta ancho de banda |
| *Write-back* | escribe solo en la caché y marca la línea **sucia**; baja al desalojarla | casi siempre |

Con write-back, un dato escrito hace rato **puede no haber salido nunca** de la caché. Para
la RAM eso no se nota. Para un aparato, sí.

Y antes de la caché hay todavía otra cosa: el **store buffer**, donde una escritura espera
sin haber llegado ni siquiera a la L1. Es lo que hace que dos núcleos vean las escrituras del
otro en distinto orden, y el motivo de que existan las barreras. Eso es
[[42-Ordenamiento-de-memoria]], no esta nota.

### 4. La coherencia entre núcleos (MESI, por arriba)

Con varios núcleos, la misma línea puede estar copiada en varias cachés. Si uno escribe, los
demás tienen que enterarse — y **se enteran, sin que el software pida nada**.

El protocolo clásico le da a cada línea, en cada caché, uno de cuatro estados:

| Estado | Quiere decir |
|---|---|
| **M**odified | la tengo yo, la cambié, la RAM está vieja. Soy el único. |
| **E**xclusive | la tengo yo sola, igual que la RAM. Puedo escribirla sin avisar. |
| **S**hared | la tenemos varios, igual que la RAM. Nadie puede escribir sin avisar. |
| **I**nvalid | no la tengo. |

Escribir exige pasar a `M`, y para eso hay que invalidársela a todos los demás. Eso es
tráfico entre núcleos, y es lo que cuesta.

> [!important] La caché es coherente; el [[TLB]] no
> Este es el contraste que conviene fijar. Las cachés de datos se mantienen solas por
> hardware: escribís y el otro núcleo ve lo nuevo. La caché de traducciones **no**: cambiás
> la tabla de páginas y hay que invalidar a mano. Dos cachés del mismo chip, dos contratos
> opuestos.

### 5. False sharing

La coherencia funciona **por línea**, no por variable. Dos núcleos que escriben dos
variables distintas que cayeron en la misma línea de 64 bytes se la sacan mutuamente: cada
escritura invalida la copia del otro.

No hay error de corrección —los datos salen bien— pero el programa puede ir órdenes de
magnitud más lento, y **no se ve en el código**: las dos variables son independientes. El
arreglo es alinear cada una a 64 bytes, o separarlas con relleno.

### 6. Y hay memoria que no se puede cachear

Porque no es memoria: es un aparato. Un registro de dispositivo cambia solo, y leer o
escribir tiene efectos. Está explicado entero en [[MMIO]] y no se repite acá. Lo único que
suma esta nota es **dónde se decide**: no en el acceso, sino en el atributo del mapeo, en la
tabla de páginas. Un puntero no sabe si lo que apunta se cachea; la MMU sí.

### 7. La otra caché, la que se olvida

Hay una caché de datos y una de **instrucciones**. Si escribís código como dato y después
saltás ahí, lo que escribiste puede estar sucio en la de datos y la de instrucciones puede
tener lo viejo — y el procesador ejecuta lo viejo.

En x86_64 no pasa: las dos cachés son coherentes entre sí, por compatibilidad con décadas de
código automodificante. En aarch64 **no lo son**, y hay que sincronizarlas a mano. Un kernel
que sube código y lo ejecuta se choca con esto en una arquitectura y no en la otra.

## Cómo lo hace Linux

Para mirar la máquina:

```bash
lscpu -C                                        # niveles, tamaño, vías, tamaño de línea
ls /sys/devices/system/cpu/cpu0/cache/          # index0..index3, uno por caché
cat /sys/devices/system/cpu/cpu0/cache/index0/{level,type,size,ways_of_associativity,coherency_line_size,shared_cpu_list}
getconf -a | grep -i cache
```

`shared_cpu_list` es el dato que más sorprende: dice **qué núcleos comparten esa caché**. La
L1 es de uno, la L3 suele ser de todos, y por eso los escalones medidos pueden no coincidir
con el tamaño nominal si hay otros procesos trabajando.

Para medir:

```bash
perf stat -e cache-references,cache-misses,L1-dcache-load-misses,LLC-load-misses ./programa
perf c2c record ./programa && perf c2c report   # literalmente "cache to cache": false sharing
```

Del lado del kernel: la constante `L1_CACHE_BYTES`, el atributo `____cacheline_aligned` y su
versión `____cacheline_aligned_in_smp` —que son exactamente la defensa contra el false
sharing—, `pgprot_noncached` e `ioremap` para lo que no se puede cachear, y
`dma_sync_single_for_cpu`/`_for_device` en arquitecturas donde el DMA **no** es coherente con
las cachés (en x86 y en el ARM de servidor sí lo es, y esas llamadas no hacen nada).

## Cómo lo hace Kornelia

| | |
|---|---|
| **Decisiones** | D12 (identity map; MMIO no cacheable), D4 (el agente escribe sus drivers) |
| **Dónde vive** | `kernel-x86_64/src/paging.rs:54#PCD: cache disable`, `kernel-aarch64/src/paging.rs:66#const ATTR_DEVICE`, `kernel-aarch64/src/exec.rs:383#dc cvau` |

**No hay política de caché: hay un atributo por bloque de 1 GiB, con dos valores.** RAM
normal write-back, o dispositivo. Eso es todo, y alcanza porque el kernel no administra
memoria: la reclama el agente (P2).

Lo interesante es que **las dos arquitecturas codifican esa misma decisión de forma
distinta**, y es un caso limpio de por qué D22/D23 exigen las dos en verde:

- En x86_64 son **dos bits en la entrada**: `PCD` (cache disable) y `PWT` (write-through)
  prendidos juntos dan "no cacheable" con el PAT por defecto.
- En aarch64 **no hay un bit**: hay un índice de tres bits a `MAIR_EL1`, un registro que
  contiene ocho descripciones de ocho bits cada una. La ranura 0 vale `0xFF` (normal
  write-back) y la ranura 1 vale `0x04` (`Device-nGnRE`). O sea: en ARM el significado de
  "no cacheable" lo define el kernel y la tabla solo lo referencia.
- Y hay un tercer bit que en x86 no existe: `SHAREABLE`
  (`kernel-aarch64/src/paging.rs:61#const SHAREABLE`), *inner shareable*, que se pone **solo
  en la memoria normal**. Es literalmente pedirle al hardware que mantenga la coherencia
  entre núcleos para ese rango. En x86 no se pide porque siempre es así.

**Las dos cachés de código, y el orden que no es negociable.** `exec` sube código máquina
por el cable —o sea, lo escribe como datos— y después salta ahí. En aarch64 eso obliga a
limpiar la caché de datos hasta el punto de unificación (`dc cvau`), esperar con un
`dsb ish`, y **recién después** invalidar la de instrucciones
(`kernel-aarch64/src/exec.rs:392#ic ivau`). Al revés no serviría: invalidaría y volvería a
traer lo viejo, que todavía está sucio en la de datos. En x86_64 no hace falta nada.

**Y el false sharing está evitado a propósito.** El bloque de estado privado de cada núcleo
está declarado `#[repr(C, align(64))]`
(`kernel-x86_64/src/percpu.rs:32#repr(C, align(64))`): 64 es el tamaño de la línea, así que
dos núcleos vecinos en el arreglo nunca comparten una. Sin eso, el punto de recuperación de
un núcleo y el del otro rebotarían entre cachés en cada `exec`.

**Qué NO hay:** ningún asignador que coloree direcciones para repartir conjuntos (no hay
asignador, [[23-Asignadores-y-por-que-aca-no-hay]]); ningún `dma_sync`, porque en las dos
máquinas de prueba el DMA es coherente; y ninguna instrucción de prefetch dirigido. Si el
agente quiere manejar la caché de su driver, tiene `exec` y las instrucciones enteras (D4).

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| La escritura no llega al aparato. | El rango está mapeado como cacheable, o falta una barrera y sigue en el store buffer. Ver [[MMIO]]. |
| Subiste código y ejecutó basura, solo en aarch64. | Falta el `dc cvau` / `ic ivau`, o se hicieron en el orden equivocado. En x86 las dos cachés son coherentes y el bug no aparece. |
| Dos núcleos, y con el segundo va **más lento** que con uno. | False sharing: dos variables independientes en la misma línea de 64 bytes. `perf c2c`. |
| Un barrido con paso de 4096 bytes es diez veces más lento que con 4160. | Conflictos de conjunto: todos los accesos caen en las mismas vías. |
| El microbenchmark da demasiado rápido y sin escalones. | El prefetcher: recorriste en orden y trajo todo antes de que lo pidieras. Por eso [[P02-Medir-el-reloj-y-las-latencias]] recorre una lista desordenada. |
| Los escalones medidos caen antes que el tamaño de `lscpu -C`. | La L3 la comparten varios núcleos, y el resto de la máquina te está robando lugar. `taskset -c 0`. |
| Un registro de dispositivo devuelve el mismo valor siempre. | Se cacheó la primera lectura. Un registro de estado cambia solo; la caché no se entera. |

## Práctica

- [[P02-Medir-el-reloj-y-las-latencias]] — *(mirar)* deducir el tamaño de tus cachés midiendo, sin preguntárselo a nadie.
- [[P15-Ver-el-false-sharing]] — *(construir)* dos hilos incrementando dos contadores vecinos, y los mismos dos separados por 64 bytes. La diferencia se mide en múltiplos.
- [[P05-Leer-un-registro-de-un-aparato-de-verdad]] — *(construir)* el otro lado: memoria que no se puede cachear.

## Recordar #flashcards/conceptos

¿Cuál es la unidad de transferencia de una caché?::La línea, de 64 bytes en casi todo lo actual. No existe leer un byte de la RAM: pedir uno trae los 64 que lo rodean.

¿Qué es la asociatividad de una caché?::Cuántos lugares distintos puede ocupar una misma dirección. Con pocas vías, direcciones que caen en el mismo conjunto se echan entre sí aunque la caché esté casi vacía.

¿Qué es el false sharing?::Dos núcleos escribiendo variables distintas que cayeron en la misma línea. Como la coherencia es por línea, la línea rebota entre cachés. No hay error de datos, solo lentitud, y no se ve en el código.

¿Las cachés de datos son coherentes entre núcleos? ¿Y el TLB?::Las de datos sí, por hardware (MESI): escribís y el otro se entera solo. El TLB no: cambiás la tabla y hay que invalidar a mano. Dos cachés del mismo chip con contratos opuestos.

¿Dónde se decide que un rango no se cachea?::En el atributo del mapeo, en la tabla de páginas — no en el acceso. En x86 son los bits `PCD`/`PWT`; en aarch64 es un índice a `MAIR_EL1`.

¿Por qué en aarch64 hay que limpiar la caché de datos antes de invalidar la de instrucciones?::Porque no son coherentes entre sí. Si se invalida primero, la de instrucciones vuelve a traer lo viejo, que todavía está sucio en la de datos. En x86_64 el problema no existe.

## Ver también

- [[Jerarquia-de-memoria]] · [[MMIO]] · [[TLB]] · [[MMU]]
- [[42-Ordenamiento-de-memoria]] — el store buffer y las barreras, que están **antes** de la caché.
- [[46-DMA-el-aparato-lee-memoria-solo]] — el otro que toca la RAM sin pasar por tu caché.
