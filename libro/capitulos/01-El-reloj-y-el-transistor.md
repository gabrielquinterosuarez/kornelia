---
tipo: capitulo
parte: 1
estado: pendiente
dificultad: 2
conceptos: [Registro, Flip-flop, Ciclo, Jerarquia-de-memoria]
practicas: [P02-Medir-el-reloj-y-las-latencias, P06-Simular-un-latch-y-ver-sus-dos-estados]
---

# 01 · El reloj y el transistor

> [!abstract] Al terminar este capítulo vas a poder
> - Explicar por qué una computadora avanza a **saltos** y no de forma continua.
> - Decir qué es un ciclo, y cuántos ciclos cuesta cada cosa que hace un kernel.
> - Entender por qué la distancia física es un problema de software.
> - Leer el reloj de tu máquina con una instrucción, en Linux y en Kornelia.

Un kernel administra tres recursos: **tiempo, memoria y aparatos**. Los tres son cosas
físicas antes de ser abstracciones, y este capítulo es sobre el primero, que además es el que ordena a los otros dos.

La pregunta con la que arranca todo: ¿por qué una computadora no puede simplemente
*calcular*, y en cambio avanza a tirones sincronizados por un reloj?

---

## Abajo de todo hay una llave

![[transistores-discretos.jpg|380]]
*Transistores discretos, antes de que entraran millones en un chip. Foto: Marcin Białek, CC BY-SA 3.0.*

Un **transistor** es una llave de luz que se abre y se cierra con electricidad en vez de
con un dedo. Tiene tres patas: por dos pasa (o no pasa) corriente, y la tercera decide.
Eso es todo. No hay una capa más abajo que importe para este libro.

De una llave a un número hay dos saltos:

1. **Un cable con corriente es un 1; sin corriente, un 0.** No hay nada más en un bit: es un voltaje. Los "bits" no existen adentro de la máquina más de lo que existen las palabras adentro de un libro impreso.
2. **Dos llaves conectadas en círculo se acuerdan de algo.** Si la salida de una alimenta la entrada de la otra y viceversa, el par tiene dos estados estables: encendido-apagado y apagado-encendido. Se queda en el que lo dejaste. Eso es un **bit de memoria**, y se
   llama *latch* o *flip-flop*.

> [!info] Este segundo salto tiene su propio anexo
> Cómo se pasa de "dos llaves en círculo" a la pieza con la que están hechos los registros
> —el latch SR, el latch D, el flip-flop disparado por flanco, y por qué el reloj tiene un
> techo— está entero en [[Flip-flop]], con una simulación de cuarenta líneas que se corre:
> [[P06-Simular-un-latch-y-ver-sus-dos-estados]]. Vale la pena leerlo antes de seguir, porque
> la sección siguiente da por sabido que los resultados **se guardan en el flanco**.

![[celdas-sram-90nm.jpg|420]]
*Celdas de SRAM en un chip de 90 nm, al microscopio. Cada celda de estas guarda un bit y usa seis transistores. Foto: [ver `CREDITOS.md`], CC BY 3.0.*

Seis transistores por bit. Un [[Cache|caché]] de 32 KiB son 262.144 bits, o sea más de un millón y
medio de transistores dedicados solo a **acordarse de una copia**. Guardar es caro en
silicio, y esa es la razón de fondo de la jerarquía de memoria que viene más abajo.

> [!note] De dónde viene "core dump"
> ![[memoria-nucleos-ferrita-plano.jpg|300]]
> Antes de los transistores, la memoria era esto: un plano de anillos de ferrita —donuts de hierro— atravesados por cables. Cada anillo guardaba un bit según el sentido en que estuviera magnetizado. Se llamaban *cores*, y volcar la memoria de un programa muerto era literalmente *volcar los núcleos*. El nombre sobrevivió a la tecnología por sesenta años.
> Foto: Konstantin Lanzet, CC BY-SA 3.0. Ver [[Falsos-amigos#1]].

---

## Por qué hace falta un reloj

Acá está la idea que cambia cómo se lee todo el resto.

Cuando una señal entra a un montón de transistores conectados —un sumador, por ejemplo— la respuesta **no aparece de inmediato**. Cada transistor tarda un poco en cambiar de estado,
y las señales tardan en recorrer los cables. Durante esos pocos picosegundos la salida está en cualquier cosa: valores intermedios, oscilaciones, basura. Después se **asienta** en el valor correcto y se queda ahí.

Entonces el problema no es calcular: es **saber cuándo mirar**.

![[reloj-y-tiempo-de-asentamiento.svg]]

La solución es un reloj: una señal que sube y baja a un ritmo fijo. La regla del silicio es
que **los resultados se guardan solo en el flanco de subida**. Entre un flanco y el
siguiente, la lógica tiene permiso para estar en cualquier estado; en el flanco, lo que hay
se congela en [[Registro|registros]] y arranca la etapa siguiente.

De ahí salen tres consecuencias que se usan en todo el libro:

- **El ciclo es la unidad de tiempo del silicio.** Un procesador a 3 GHz hace tres mil
  millones de ciclos por segundo: **un ciclo dura 0,33 nanosegundos**.
- **La frecuencia está limitada por lo más lento.** El período tiene que alcanzar para que se asiente el camino más largo de la lógica. Si querés ir más rápido, tenés que cortar ese camino en etapas más cortas — que es exactamente lo que es un [[03-Que-hace-realmente-una-instruccion|pipeline]].
- **Todo lo que un kernel hace se mide en ciclos**, y los números son brutalmente
  desparejos.

---

## La distancia es un problema de software

En un nanosegundo, la luz recorre 30 centímetros. Una señal eléctrica en cobre va más
despacio: unos 20 cm por nanosegundo.

Un ciclo a 3 GHz son 0,33 ns. **En un ciclo, una señal alcanza a recorrer unos 7
centímetros** — y eso en el mejor caso, sin atravesar un solo transistor.

Mirá una placa madre. La RAM está a diez o quince centímetros del procesador. Ir y volver
son treinta centímetros: **cuatro o cinco ciclos perdidos solo en el viaje**, antes de que
el chip de memoria haga nada. Y esa es la razón física, no de diseño, por la que existe
esto:

![[die-intel-broadwell-un-nucleo.jpg|460]]
*Un núcleo de un Intel Broadwell al microscopio. La zona regular y repetitiva de un costado es caché: casi la mitad del área del núcleo se usa para tener datos cerca. Foto: Fritzchens Fritz, CC BY-SA 2.0.*

Casi la mitad del silicio de un procesador moderno no calcula nada: está ahí para que los
datos no tengan que viajar. **La jerarquía de memoria no es una optimización, es una
consecuencia de la velocidad de la luz.**

---

## La tabla más importante del libro

Los números son aproximados y varían por máquina; lo que importa son los **órdenes de
magnitud**. La última columna traduce todo a escala humana, como si un ciclo fuera un
segundo.

| Ir a buscar un dato a… | Ciclos | Tiempo real (a 3 GHz) | Si un ciclo fuera 1 segundo |
|---|---|---|---|
| Un **registro** | 0 | — | ahora mismo, en tu mano |
| Caché **L1** | ~4 | 1,3 ns | 4 segundos |
| Caché **L2** | ~12 | 4 ns | 12 segundos |
| Caché **L3** (compartida) | ~40 | 13 ns | 40 segundos |
| **RAM** | ~250 | 80 ns | 4 minutos |
| Otro núcleo (línea de caché en conflicto) | ~100–400 | 30–130 ns | 2–7 minutos |
| **[[NVMe]]** (un disco de hoy) | ~250.000 | 80 µs | **3 días** |
| Disco giratorio | ~30.000.000 | 10 ms | **casi un año** |
| Un paquete a otro continente | ~450.000.000 | 150 ms | **14 años** |

Cuatro segundos contra tres días. Esa es la diferencia entre un dato que está en la caché y
un dato que está en el disco, y explica de una sola vez por qué:

- Un kernel se pasa la vida **evitando** ir a buscar cosas lejos.
- Un aparato lento no se atiende esperándolo: se atiende con
  [[34-Del-cable-al-numero-PIC-APIC-GIC|una interrupción]] que avisa cuando terminó. Si el
  procesador esperara al NVMe girando, perdería 250.000 ciclos por lectura.
- Cuando el kernel [[38-Dormir-en-vez-de-girar|duerme en vez de girar]] no está siendo
  prolijo: está devolviendo días de escala humana.
- Un [[DMA]] existe: hacer que el aparato mueva los datos **por su cuenta** en vez de que el
  procesador copie byte por byte desde tan lejos. Ver [[46-DMA-el-aparato-lee-memoria-solo]].

> [!tip] La pregunta que conviene hacerse en cada capítulo
> "¿A cuántos ciclos está la cosa que este mecanismo va a buscar?" Casi todo el diseño de un
> kernel se deduce de esa tabla.

---

## Qué es "el estado" del procesador

En cada flanco del reloj, el procesador guarda su resultado en registros. Los
[[Registro|registros]] son un puñado de celdas —16 de propósito general en x86_64, 31 en
aarch64— que viven adentro del procesador y son la única memoria que no cuesta ciclos
alcanzar.

Y acá aparece por primera vez la naturaleza de un kernel: **el estado visible de un
procesador es un puñado de números**. Los registros, más unos cuantos de control. Eso es
todo lo que hay que guardar para congelar un programa y reanudarlo después, y eso es lo que
hace un cambio de contexto.

Es también por qué un [[32-Los-faults-como-datos|fault]] se puede devolver como dato: cuando
el código del agente se rompe, "qué estaba pasando" se puede escribir entero —los registros
más la causa— y mandar por un cable. No hay nada más que contar.

---

## En Kornelia

El reloj de este kernel es el contador que **ya viene andando** en el silicio: `TSC` en
x86_64, `CNTPCT_EL0` en aarch64. Se lee con una instrucción y no hace falta [[Driver|driver]] ninguno,
que es lo más cerca del silicio que se puede estar:

- `kernel-aarch64/src/main.rs:366#cntpct_el0` — leer el contador en aarch64.
- `kernel-x86_64/src/main.rs:138#fn clock` — lo mismo del otro lado.

Lo interesante no es leerlo: es que **las dos máquinas no coinciden en si te dicen a qué
ritmo sube**.

| | Cómo se sabe la frecuencia |
|---|---|
| **aarch64** | La máquina lo informa en un registro: `CNTFRQ_EL0` (`kernel-aarch64/src/main.rs:354#cntfrq_el0`). Se lee y listo. |
| **x86_64** | Puede no decirlo. Entonces **se mide**: se cuenta cuánto sube el TSC contra el contador de frecuencia fija que informa [[ACPI]]. |

Y si nadie lo dice, el kernel **dice que no lo sabe** en vez de calcular un tiempo falso.
Eso es P4 en su forma más chica: la máquina se describe a sí misma, y cuando no se describe,
el kernel no rellena el hueco con una suposición. Un tiempo inventado es peor que no tener
tiempo, porque parece un dato.

Hay un detalle de silicio escondido ahí que vale la pena, y está comentado en el código
(`kernel-x86_64/src/main.rs:505#"lfence",`): antes de `rdtsc` hay que poner una barrera, porque
**el procesador puede adelantar la lectura del contador** y medir menos de lo que pasó. El
procesador ejecuta fuera de orden, y el reloj no es una excepción. Ver
[[03-Que-hace-realmente-una-instruccion]].

### Para qué le sirve el reloj a este kernel

Antes de tenerlo, las esperas del kernel se medían **en vueltas de un bucle** — que es una
unidad que cambia con la velocidad de la máquina y con el compilador. Con reloj:

- La ventana de rescate del [[51-El-blob-y-la-ventana-de-rescate|blob]] dura **dos segundos
  de verdad**, no un número de vueltas.
- El agente puede declarar `exec {deadline_ms}`, y el kernel corta el código que no vuelve
  (P6: el plazo lo declara el agente, no el kernel). Ver [[39-Plazos-y-cortes]].

---

## Las prácticas de este capítulo

- [[P06-Simular-un-latch-y-ver-sus-dos-estados]] — *(construir)* un bit de memoria hecho con una tabla de verdad, y el estado prohibido que no se asienta.
- [[P02-Medir-el-reloj-y-las-latencias]] — *(mirar)* medir tu propio TSC, ver los tamaños de tus cachés, y comprobar la tabla de latencias en tu máquina en vez de creerla.

---

## Recordar #flashcards/parte-01

¿Por qué una computadora necesita un reloj?::Porque la lógica no da su resultado de inmediato: durante unos picosegundos la salida es basura mientras se asienta. El reloj define **cuándo mirar** — los resultados se guardan solo en el flanco de subida.

¿Cuánto dura un ciclo a 3 GHz?::0,33 nanosegundos. En ese tiempo una señal eléctrica alcanza a recorrer unos 7 centímetros de cobre.

¿Por qué existe la caché?::Por la velocidad de la luz. La RAM está a diez o quince centímetros, y ese viaje solo cuesta varios ciclos antes de que la memoria haga nada. Casi la mitad del área de un núcleo moderno es caché.

¿Cuántos ciclos cuesta un dato en L1 y cuántos uno en RAM?::Unos 4 contra unos 250. En escala humana (un ciclo = un segundo): 4 segundos contra 4 minutos. Y un NVMe son 3 días.

¿Qué es el estado visible de un procesador?::Un puñado de números: los registros de propósito general más unos de control. Eso es todo lo que hay que guardar para congelar un programa, y es por eso que un fault se puede devolver entero como dato.

¿Cómo sabe Kornelia a qué ritmo sube su contador de tiempo?::En aarch64 la máquina lo informa en `CNTFRQ_EL0`. En x86_64 puede no decirlo, y entonces se **mide** contra el contador de frecuencia fija de ACPI. Si nadie lo dice, el kernel dice que no lo sabe en vez de inventar un tiempo.

¿Por qué hay una barrera antes de `rdtsc`?::Porque el procesador ejecuta fuera de orden y puede **adelantar** la lectura del contador, midiendo menos tiempo del que realmente pasó.

## Qué sigue

[[02-Registros-y-RAM-no-son-lo-mismo]]
