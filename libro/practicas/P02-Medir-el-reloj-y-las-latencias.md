---
tipo: practica
clase: mirar
contra: ambos
estado: pendiente
riesgo: ninguno
conceptos: [Registro, Ciclo, Jerarquia-de-memoria]
---

# P02 · Medir el reloj y las latencias

> [!success] Qué vas a ver si funciona
> Dos cosas, y las dos son números tuyos, no del libro:
> 1. La frecuencia de tu contador de ciclos, medida por vos con una instrucción.
> 2. Una tabla de latencias con **escalones**, y cada escalón cayendo exactamente en el tamaño de una de tus cachés — que vas a poder verificar contra `lscpu -C`. El segundo resultado es el que importa: vas a **deducir el tamaño de tus cachés midiendo**, sin preguntárselo a nadie.

De solo lectura, sin `sudo`, sin riesgo. El código está en `libro/practicas/codigo/`.

---

## Parte 1 · Leer el contador de ciclos

```bash
cd libro/practicas/codigo
cc -O2 -o /tmp/tsc tsc.c && /tmp/tsc
```

Salida en la máquina donde se escribió esta práctica:

```
ciclos contados : 597762510
segundos reales : 0.300083
frecuencia      : 1.992 GHz
un ciclo dura   : 0.502 ns
en un ciclo la senal recorre unos 10.0 cm de cobre
```

**Qué estás viendo:** `__rdtsc()` es **una instrucción** que devuelve el valor de un contador que el silicio mantiene solo. No hay driver, no hay llamada al sistema, no hay kernel en el medio. Es lo más cerca del hardware que se puede estar desde un programa normal, y es exactamente lo que hace Kornelia (`kernel-x86_64/src/main.rs:505#"lfence",`).

Fijate en la última línea: en un ciclo, la señal recorre diez centímetros de cobre. Tu RAM está a más que eso. Ver [[01-El-reloj-y-el-transistor]].

### El `lfence` no es decorativo

El programa pone una barrera antes de leer el contador, con el mismo comentario que el kernel. Probá sacarla:

```bash
sed 's/__builtin_ia32_lfence();//' tsc.c > /tmp/tsc-sin-barrera.c
cc -O2 -o /tmp/tsc-sin /tmp/tsc-sin-barrera.c && for i in 1 2 3 4 5; do /tmp/tsc-sin | grep frecuencia; done
for i in 1 2 3 4 5; do /tmp/tsc | grep frecuencia; done
```

Con 300 ms de medición la diferencia es ruido. **Es un buen resultado, no un fracaso**: la barrera importa cuando lo que medís dura pocos ciclos, porque el procesador ejecuta fuera de orden y puede adelantar la lectura del contador unas cuantas instrucciones. Si medís algo que dura cien ciclos, adelantarse diez es un 10% de error; si medís 600 millones, no se ve.

**La moraleja es la que aplica a todo el libro:** un mecanismo puede estar bien puesto y no mostrar diferencia en tu prueba. Que no se vea no significa que no haga falta — significa que tu prueba no lo mide. Ver [[58-Una-prueba-que-no-puede-pasar-por-accidente]].

### El número que no cierra, y por qué es lo más interesante de la parte 1

```bash
lscpu | grep -iE "MHz"
```

En la máquina de arriba:

```
CPU MHz máx.:    1800,0000
CPU MHz mín.:     400,0000
```

**El TSC midió 1,992 GHz y el procesador no pasa de 1,8 GHz.** No es un error de medición: es que en un procesador moderno **el TSC no cuenta ciclos del núcleo**. Cuenta a un ritmo fijo, derivado de un cristal, que no cambia cuando el núcleo sube o baja de frecuencia ni cuando se duerme. Se comprueba así:

```bash
grep -o 'constant_tsc\|nonstop_tsc' /proc/cpuinfo | sort -u
```

| Bandera | Qué garantiza |
|---|---|
| `constant_tsc` | El ritmo no cambia con la frecuencia del núcleo. |
| `nonstop_tsc` | No se detiene cuando el núcleo entra en estados de bajo consumo. |

Y ahí está la razón por la que un kernel puede usar el TSC como reloj: **si contara ciclos reales, sería inútil para medir tiempo**, porque el tiempo por ciclo cambiaría todo el tiempo. Históricamente fue así, y por eso el kernel de Linux tiene una máquina de estados entera para decidir si le cree al TSC (`dmesg | grep -i tsc`).

Es también por qué la columna "ciclos" de la parte 2 dice **aproximados**: convertir nanosegundos a ciclos del núcleo requiere saber a qué frecuencia estaba corriendo el núcleo en ese momento, que es un dato que se mueve.

> [!question] Duda para anotar si te queda picando
> Kornelia informa `clock {kind, hz}` y dice "no sé" cuando nadie le dice la frecuencia. ¿Debería informar también si el contador es `constant`/`nonstop`, o eso ya es una opinión sobre para qué sirve? Es exactamente la clase de pregunta que va a `dudas/`.

---

## Parte 2 · Medir la jerarquía de memoria

```bash
cc -O2 -o /tmp/lat latencias.c && /tmp/lat 1.992
#                                          ^ los GHz que midio la parte 1
```

Tarda unos minutos. Salida real de la misma máquina (sin la columna de ciclos):

```
    tamano   ns/acceso
      4 KiB        2.86
      8 KiB        2.86
     16 KiB        2.88
     32 KiB        4.04     <- L1d = 32K
     64 KiB        7.22
    128 KiB        7.75
    256 KiB       15.27     <- L2 = 256K
    512 KiB       22.32
   1024 KiB       27.67
   2048 KiB       45.51
   4096 KiB       74.03
   8192 KiB       96.36     <- L3 = 8M
  16384 KiB      108.87
  32768 KiB      113.28
  65536 KiB      119.83
 131072 KiB      128.70     <- RAM, meseta
```

Y las cachés de esa máquina, según la máquina:

```bash
lscpu -C
```

```
NAME ONE-SIZE ALL-SIZE WAYS TYPE        LEVEL COHERENCY-SIZE
L1d       32K     128K    8 Data            1             64
L1i       32K     128K    8 Instruction     1             64
L2       256K       1M    4 Unified         2             64
L3         8M       8M   16 Unified         3             64
```

**Los escalones caen en 32K, 256K y 8M. Los tamaños de L1d, L2 y L3.**

Eso es el resultado de la práctica: **medir una propiedad física del chip desde un programa normal**, sin preguntársela a nadie. Si `lscpu` mintiera, la medición seguiría siendo verdad.

Llená con lo tuyo:

| | Tu máquina |
|---|---|
| L1d, L2, L3 según `lscpu -C` | |
| Dónde están los escalones de tu medición | |
| ns por acceso en el primer escalón (L1) | |
| ns por acceso en la meseta (RAM) | |
| Cuántas veces más lenta es la RAM que la L1 | |

En la máquina del ejemplo: 128,70 / 2,86 = **45 veces**. Compará con la tabla de [[01-El-reloj-y-el-transistor]] y anotá si tu máquina se parece.

### Por qué la lista está desordenada

El programa recorre una lista enlazada **con los saltos en orden aleatorio**, y las dos razones son las que hacen que la medición valga:

1. **Si fuera secuencial**, el *prefetcher* del procesador —que detecta patrones y trae la línea siguiente antes de que la pidas— haría que casi todo pareciera estar en L1. Estarías midiendo el prefetcher.
2. **Cada salto depende del anterior**, así que el procesador no puede lanzar varios accesos en paralelo. Sin eso medirías *ancho de banda* (cuántos datos por segundo) en vez de *latencia* (cuánto tarda uno). Son dos números distintos y se confunden todo el tiempo.

Probá romper la primera a propósito, recorriendo el bloque de a una línea en orden:

```bash
# cambiá el bucle de recorrido por un barrido secuencial y volvé a medir
```

Los escalones se aplanan. **Ese aplanamiento es el prefetcher, visible.**

---

## Parte 3 · El reloj de Kornelia

```bash
cd ~/Proyectos/kornelia
export PATH="$HOME/.cargo/bin:$PATH"
./scripts/client.py --what clock
```

| | Kornelia (x86_64) | Kornelia (aarch64) | Tu Linux |
|---|---|---|---|
| Qué contador usa | `tsc` | `cntpct` | `tsc` (y `CLOCK_MONOTONIC` encima) |
| Cómo sabe la frecuencia | La **mide** contra el contador de ACPI | La **lee** de `CNTFRQ_EL0` | La mide, con más ceremonia |
| Si no la sabe | Dice que no la sabe | — | Descarta el TSC y usa otra fuente |

```bash
./scripts/client.py --arch aarch64 --what clock
```

**Qué estás viendo:** dos arquitecturas que resuelven "qué hora es" de forma distinta, y un kernel que no elige por vos. ARM lo informa en un registro (`kernel-aarch64/src/main.rs:354#cntfrq_el0`); x86 puede no decirlo y entonces hay que medirlo, igual que lo medimos nosotros en la parte 1. Y si nadie lo dice, el kernel **no inventa un número**: un tiempo falso es peor que no tener tiempo, porque parece un dato (P4).

---

## Qué mirar cuando no sale

| Síntoma | Causa probable |
|---|---|
| `x86intrin.h: No such file` | Estás en aarch64. Ahí el contador es `CNTPCT_EL0` y se lee con `mrs`: el programa habría que reescribirlo con `asm volatile("mrs %0, cntvct_el0" : "=r"(v))`. Buen ejercicio. |
| La frecuencia sale absurda (0,001 GHz, o gigantesca) | El `nanosleep` se interrumpió, o estás en una VM sin `constant_tsc` donde el TSC se detiene. Probá con `-enable-kvm -cpu host`. |
| `latencias` se muere con "sin memoria" | Bajá el límite del bucle: `bytes <= 32u << 20`. |
| La tabla de latencias no tiene escalones | ¿Compilaste con `-O2`? Sin optimizar, el costo del bucle tapa el de la memoria. También pasa en una VM con poca RAM o con la memoria del host bajo presión. |
| Los escalones no coinciden con `lscpu -C` | Si tenés varios núcleos compartiendo L3, el número efectivo puede ser menor. Probá fijando el proceso a un núcleo: `taskset -c 0 /tmp/lat`. |

## Anotaciones

*(Tuyas. Los dos números que conviene recordar de memoria son cuántos ns cuesta tu L1 y cuántos tu RAM: son la referencia con la que se lee todo el resto del libro.)*
