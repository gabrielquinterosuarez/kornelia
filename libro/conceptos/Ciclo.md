---
tipo: concepto
estado: pendiente
dificultad: 1
principios: [P4]
decisiones: []
practicas: [P02-Medir-el-reloj-y-las-latencias, P04-Ver-al-procesador-desordenar]
capitulos: [01-El-reloj-y-el-transistor, 03-Que-hace-realmente-una-instruccion]
---

# Ciclo

> Un tic del reloj del procesador: el instante en que la lógica ya se asentó y los resultados se guardan. La unidad de tiempo del silicio.

## Qué problema resuelve

La lógica no da su resultado de inmediato: durante unos picosegundos la salida es basura mientras las señales se propagan. El reloj define **cuándo mirar**. Ver [[Flip-flop]], que es la pieza que hace cumplir esa regla.

Como unidad de medida sirve para algo que los segundos no: **es la misma en todas las máquinas de la misma familia**. "Un acceso a RAM cuesta 250 ciclos" se puede comparar entre procesadores; "cuesta 80 nanosegundos" no, porque depende de la frecuencia.

## Lo que un ciclo NO es

Tres cosas que casi todo el mundo cree y son falsas en cualquier procesador de los últimos treinta años:

| Creencia | Por qué es falsa |
|---|---|
| "Un ciclo es una instrucción" | Una instrucción atraviesa varias etapas, una por ciclo, y hay varias en vuelo a la vez. Ver [[03-Que-hace-realmente-una-instruccion]]. |
| "Como mucho una instrucción por ciclo" | Un procesador superescalar termina 3, 4 o 6. Medido en [[P04-Ver-al-procesador-desordenar]]: 3,6. |
| "Como mínimo una instrucción por ciclo" | Una que espera un dato de RAM tarda ~250. |

**Lo que sí es**: el intervalo en que *cada pedazo* del procesador hace un paso. Muchos pasos distintos, de instrucciones distintas, en el mismo ciclo.

## Cuánto dura

A 3 GHz, un ciclo dura **0,33 nanosegundos**. Vale la pena tener el número al lado del otro que importa: en un ciclo, una señal eléctrica recorre unos **7 centímetros** de cobre. La RAM está más lejos que eso, y de ahí sale toda la [[Jerarquia-de-memoria|jerarquía de memoria]].

## Cómo lo hace Linux

Acá aparece la trampa que hace difícil este concepto: **la frecuencia no es un número, son tres, y los tres se llaman parecido.**

```bash
lscpu | grep -i mhz                                      # el limite de la POLITICA
cat /sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq  # el limite del SILICIO
cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq  # lo que dice que corre ahora
grep -o 'constant_tsc\|nonstop_tsc' /proc/cpuinfo | sort -u
```

En la máquina donde se escribió este libro, los tres dieron distinto y **el que se ve más "actual" era el que mentía**:

| | Valor | Qué es |
|---|---|---|
| `lscpu` MHz máx | 1.800 | El techo que impuso el gobernador, no el del chip. |
| `cpuinfo_max_freq` | 4.000 | El techo del silicio. |
| `scaling_cur_freq` | 1.800 | Decía eso **mientras el núcleo corría a 3,4 GHz**. |
| Frecuencia real medida | **3,41 GHz** | Deducida con una cadena de sumas dependientes. |

> [!warning] `scaling_cur_freq` puede mentir
> Con el gobernador `intel_pstate`, la frecuencia la decide el hardware ciclo a ciclo y ese archivo suele devolver el valor de la política en vez del real. La forma confiable de saber a qué velocidad va un núcleo es **medirla**: ver abajo.

Y el **TSC**, que es de lo que uno lee el "tiempo", es un cuarto número: tictaquea a un ritmo **fijo** (1,992 GHz en esa máquina) que no cambia cuando el núcleo sube o baja. Eso es lo que lo hace útil como reloj e inútil como contador de ciclos. Ver [[P02-Medir-el-reloj-y-las-latencias]].

### El truco para medir la frecuencia de verdad

Una suma que depende del resultado de la anterior tarda **exactamente un ciclo**: esa es la latencia del `add`, y no se puede acelerar ni con desorden ni con superescalaridad, porque no hay nada que adelantar. Entonces una cadena larga de sumas dependientes **es un frecuencímetro**:

```
frecuencia = 1 / (tiempo por suma encadenada)
```

Está implementado en [[P04-Ver-al-procesador-desordenar]].

## Cómo lo hace Kornelia

El kernel no cuenta ciclos: cuenta **tiempo**, con el contador que ya viene andando (`TSC` en x86_64, `CNTPCT_EL0` en aarch64), leído con una instrucción y sin driver.

Y ahí aplica P4 en su forma más chica: **ARM informa el ritmo en un registro** (`CNTFRQ_EL0`), **x86 puede no decirlo** y entonces el kernel lo **mide** contra el contador de frecuencia fija de ACPI. Si nadie lo dice, el kernel dice que no lo sabe (`describe {what:["clock"]}`) en vez de calcular un tiempo falso — un tiempo inventado es peor que no tener tiempo, porque parece un dato.

Antes de tener reloj, las esperas del kernel se medían **en vueltas de un bucle**, que es una unidad que cambia con la máquina y con el compilador. Con reloj, la ventana de rescate del blob dura **dos segundos de verdad** y `exec {deadline_ms}` significa algo.

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| Una medición da menos ciclos de los físicamente posibles. | Convertiste tiempo a ciclos con la frecuencia equivocada. Casi siempre `lscpu` o `scaling_cur_freq`. |
| Dos medidas seguidas salen al revés en el tiempo. | Falta una barrera antes de leer el contador: el procesador adelantó la lectura. Ver [[Ejecucion-fuera-de-orden]]. |
| El mismo código tarda distinto en cada corrida. | La frecuencia cambió (gobernador, temperatura, otro núcleo despertándose). Por eso se mide muchas veces y se mira la mediana. |
| Un tiempo medido en una VM es absurdo. | El contador puede no ser `constant_tsc`, o el huésped fue desalojado en medio de la medición. |

## Práctica

- [[P02-Medir-el-reloj-y-las-latencias]] — *(mirar)* leer el contador con una instrucción y descubrir que el TSC no cuenta ciclos del núcleo.
- [[P04-Ver-al-procesador-desordenar]] — *(construir)* usar una cadena dependiente como frecuencímetro, y contar instrucciones por ciclo.

## Recordar #flashcards/conceptos

¿Qué es exactamente un ciclo?::El intervalo entre dos flancos del reloj: el tiempo en que cada pedazo del procesador da un paso. No es "una instrucción" — hay varias en vuelo, cada una en una etapa distinta.

¿Por qué medir en ciclos y no en nanosegundos?::Porque los ciclos se comparan entre máquinas de la misma familia y los nanosegundos no: dependen de la frecuencia, que cambia sola.

¿Cómo se mide la frecuencia real de un núcleo sin creerle a `/sys`?::Con una cadena larga de sumas **dependientes**: cada una tarda exactamente un ciclo (la latencia del `add`), así que la frecuencia es 1 dividido el tiempo por suma.

¿Por qué el TSC no sirve para contar ciclos del núcleo?::Porque tictaquea a un ritmo fijo derivado de un cristal, independiente de la frecuencia a la que corra el núcleo. Eso es justo lo que lo hace confiable como reloj.

## Ver también

- [[Flip-flop]] — por qué hace falta un reloj.
- [[Ejecucion-fuera-de-orden]] · [[Jerarquia-de-memoria]]
