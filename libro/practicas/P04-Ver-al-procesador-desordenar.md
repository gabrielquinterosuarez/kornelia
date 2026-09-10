---
tipo: practica
clase: construir
contra: linux
estado: pendiente
riesgo: ninguno
conceptos: [Ejecucion-fuera-de-orden, Ciclo, Registro]
---

# P04 · Ver al procesador desordenar

> [!success] Qué vas a ver si funciona
> **La misma cantidad de sumas, tres o cuatro veces más rápidas** solo por no depender unas de otras. Ese factor **es** el desorden, medido: si el procesador ejecutara una instrucción por ciclo en el orden escrito, los dos números serían idénticos.
>
> Y de yapa: la frecuencia real de tu núcleo, deducida sin creerle a ningún archivo de `/sys`.

Sin `sudo` y sin instalar nada. El código está en `libro/practicas/codigo/fuera-de-orden.c`.

## La idea

Dos bucles con **exactamente** la misma cantidad de sumas:

```c
/* UNA cadena: cada suma necesita el resultado de la anterior. */
for (i = 0; i < N; i++) SUMA(a);

/* OCHO cadenas: la misma cantidad de sumas, sin depender entre si. */
for (i = 0; i < N/8; i++) { SUMA(b0); SUMA(b1); ... SUMA(b7); }
```

En la primera **no hay nada que reordenar**: la suma número mil no puede empezar hasta que termine la 999. En la segunda hay ocho hilos de trabajo independientes y el procesador puede meter varios por ciclo.

> [!warning] Por qué las sumas van en ensamblador
> ```c
> #define SUMA(x) __asm__ volatile("add $1, %0" : "+r"(x))
> ```
> El primer intento de escribir esta práctica usó C normal, y **el compilador convirtió `for (i) a = a + 1` en `a += N`**. Midió 0,03 ns por suma: ocho sumas por ciclo, imposible. El ensamblador en línea obliga a que la suma exista de verdad.
>
> Es la trampa clásica de los microbenchmarks: **si el resultado es demasiado bueno, no mediste lo que creías.**

## Correrla

```bash
cd libro/practicas/codigo
cc -O2 -o /tmp/ooo fuera-de-orden.c && /tmp/ooo
```

Tarda unos segundos. Salida en la máquina donde se escribió el capítulo:

```
             ns/suma  sumas/ciclo
1 cadena       0.293         1.00
8 cadenas      0.081         3.63

mismas 1000000000 sumas, 3.6x de diferencia
frecuencia deducida de la cadena: 3.41 GHz
```

Llená con lo tuyo:

| | Tu máquina |
|---|---|
| ns por suma, encadenadas | |
| ns por suma, independientes | |
| Factor de diferencia | |
| Frecuencia deducida | |

---

## Qué significa cada número

**`1.00 sumas/ciclo` en la cadena** no es un resultado: es la definición. Una suma que depende de la anterior tarda exactamente un ciclo, que es la latencia del `add`, y no hay desorden ni superescalaridad que la acelere — no hay nada que adelantar. Por eso sirve de patrón.

**`3.63 sumas/ciclo` en paralelo** sí es un resultado, y es el que prueba las dos cosas a la vez:

- El procesador **termina varias instrucciones en el mismo ciclo** (superescalar): tiene unas cuatro unidades aritméticas.
- Y para alimentarlas tiene que **elegir de una pileta** cuáles pueden avanzar, que es exactamente [[Ejecucion-fuera-de-orden|ejecutar fuera de orden]].

**El factor 3,6** es la respuesta corta a "¿cuánto importa esto?". Es el mismo código, la misma cantidad de trabajo, la misma máquina. Lo único que cambia es si las instrucciones se pisan entre sí.

---

## La parte que no esperabas: la frecuencia

Como la cadena dependiente hace exactamente una suma por ciclo, **es un frecuencímetro**:

```
frecuencia = 1 / (tiempo por suma encadenada)
```

Comparalo con lo que dicen los archivos:

```bash
lscpu | grep -i "mhz"
cat /sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq
cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq
```

En la máquina de referencia, los cuatro números fueron **distintos**:

| Fuente | Valor |
|---|---|
| `lscpu` MHz máx | 1.800 |
| `scaling_cur_freq` (mientras corría) | 1.800 |
| `cpuinfo_max_freq` | 4.000 |
| **Medido con la cadena** | **3.410** |

Los dos primeros mentían: con el gobernador `intel_pstate` la frecuencia la decide el hardware ciclo a ciclo, y esos archivos devuelven el valor de la **política**, no el real.

> [!tip] La moraleja, que vale para todo el libro
> **Un número que la máquina publica no siempre es un número que la máquina cumple.** Es la misma lección que P4 en el kernel: preferir lo que se puede comprobar sobre lo que se puede deducir — y cuando se puede medir, medir.

---

## Para jugar

1. **Bajá de 8 cadenas a 2, 3, 4, 6.** ¿Dónde deja de mejorar? Ese punto es cuántas unidades aritméticas tiene tu procesador. Con más cadenas que unidades, no gana nada.
2. **Cambiá `add` por `imul`.** La multiplicación tiene latencia 3 y throughput 1: la cadena debería ir tres veces más lento, y las independientes casi igual que antes. Ahí ves la diferencia entre *latencia* y *throughput* con las manos.
3. **Fijá el núcleo y la frecuencia** y volvé a medir:
   ```bash
   taskset -c 2 /tmp/ooo
   ```
4. **Contá instrucciones por ciclo de verdad**, si querés instalar `perf`:
   ```bash
   sudo apt install linux-perf
   sudo sysctl kernel.perf_event_paranoid=1
   perf stat -e cycles,instructions /tmp/ooo
   ```
   La columna `insn per cycle` tiene que confirmar lo que dedujiste. (En Debian 13, `perf_event_paranoid` viene en 3, que bloquea a los usuarios sin privilegio.)

---

## Qué mirar cuando no sale

| Síntoma | Causa probable |
|---|---|
| Las dos mediciones dan casi igual | El bucle de las ocho cadenas se está limitando por otra cosa: el contador del `for`, o la memoria. Revisá que las ocho variables estén en registros (`objdump -d /tmp/ooo`). |
| Más de 6 sumas por ciclo | Sospechá. Ningún procesador de escritorio pasa de 4-6 ALUs. Probablemente el compilador colapsó algo. |
| La frecuencia deducida da absurda | La cadena no es una cadena: si el compilador la rompió, el número no significa nada. Comprobalo con `objdump`: tiene que haber un `add` por iteración sobre el **mismo** registro. |
| Los números cambian mucho entre corridas | La frecuencia se está moviendo (temperatura, gobernador, otro proceso). Corré tres veces y quedate con la mejor. |
| `error: unknown register name` al compilar | Estás en aarch64: cambiá el `add $1, %0` por `add %0, %0, #1`. La conclusión es la misma. |

## Anotaciones

*(Tuyas. Los dos números que conviene recordar son tu factor de paralelismo y tu frecuencia real — no la que dice `lscpu`.)*
