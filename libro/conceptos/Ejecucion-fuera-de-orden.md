---
tipo: concepto
estado: pendiente
dificultad: 3
principios: [P5]
decisiones: []
practicas: [P04-Ver-al-procesador-desordenar]
capitulos: [03-Que-hace-realmente-una-instruccion, 42-Ordenamiento-de-memoria]
---

# Ejecución fuera de orden

> El procesador ejecuta las instrucciones **en el orden en que se van pudiendo**, no en el orden en que están escritas — y confirma los resultados en el orden original, para que nadie se entere.

## Qué problema resuelve

Una instrucción que espera un dato de RAM tarda unos 250 [[Ciclo|ciclos]]. Si el procesador ejecuta estrictamente en orden, **todo lo que viene atrás espera**, aunque no tenga nada que ver:

```asm
mov rax, [rbx]     ; 250 ciclos esperando la RAM
add rax, 1         ; NECESITA el resultado de arriba: no hay opcion
add rcx, 5         ; no depende de nada — y sin embargo espera
```

Esa tercera instrucción podría haberse hecho mil veces mientras la primera viajaba. Ejecutar fuera de orden no es astucia: es no tirar 250 ciclos de silicio por cada lectura.

## Cómo funciona

Tres piezas, y las tres trabajan **en el mismo ciclo**:

| Pieza | Qué hace | Orden |
|---|---|---|
| **Traer y decodificar** | Lee instrucciones y las tira a una pileta. | **En orden** de programa |
| **El planificador** | Cada ciclo elige de la pileta las que ya tienen todos sus operandos y las manda a las unidades libres. | **Desordenado** |
| **El buffer de reordenamiento** | Guarda resultados terminados hasta que les toca. | **En orden** de nuevo |

O sea: entra en orden, se ejecuta como se puede, sale en orden.

> [!important] Esto no contradice al reloj
> La confusión más común es pensar que si el procesador es sincrónico tiene que ser secuencial. Son cosas independientes. **En cada flanco del reloj, todas las piezas dan un paso a la vez**; lo que el desorden cambia es *cuál* instrucción entra a cada unidad, no *cuándo* ocurren las cosas.
>
> Y visto así se da vuelta la intuición: el reloj **no** es lo que fuerza a hacer una cosa por vez, es lo que hace manejable hacer doscientas a la vez. Sin un ritmo común no se podrían coordinar cientos de instrucciones en vuelo.

### El renombrado de registros

Hay un obstáculo que no es una dependencia real:

```asm
mov rax, [rbx]     ; lento
add rcx, rax       ; usa rax
mov rax, 7         ; PISA rax, pero no depende de nadie
```

La tercera no depende de las anteriores; solo *reusa el nombre* `rax`. Como hay [[Registro|pocos registros]], el compilador los recicla todo el tiempo, y eso crearía dependencias falsas.

La solución del silicio: hay muchos más registros físicos que nombres —doscientos y pico contra dieciséis— y el procesador **renombra**. Cada escritura a `rax` estrena un registro físico distinto. Las dependencias falsas desaparecen y quedan solo las verdaderas.

Es una consecuencia bonita de [[02-Registros-y-RAM-no-son-lo-mismo|que un registro sea un nombre]]: como es un nombre y no un lugar, se lo puede redirigir.

## Dónde se filtra la ilusión

El retiro en orden hace que **tu propio programa** no note nada. Cuatro lugares donde sí se nota:

1. **Medir tiempo.** Leer el contador es una instrucción y se puede adelantar respecto de lo que querías cronometrar. Por eso hay una barrera antes: `kernel-x86_64/src/main.rs:513#"lfence",`.
2. **Otro núcleo mirando.** El orden se garantiza para vos, no para los demás. Ver [[42-Ordenamiento-de-memoria]].
3. **Un [[Aparato|aparato]] mirando.** Escribirle "arrancá" antes de que llegue el dato es un bug real; de ahí `volatile` y las barreras en [[MMIO]].
4. **La especulación deja rastro.** Frente a un `if`, el procesador **predice** y sigue. Si erró, descarta el resultado — pero lo que trajo a la [[Cache|caché]] queda, y esa huella se puede medir. Toda la familia Spectre sale de ahí.

## Cómo lo hace Linux

No lo hace: **no es del sistema operativo**. Ningún kernel enciende ni apaga la ejecución fuera de orden; es del silicio y no tiene interruptor.

Lo que Linux sí hace es **convivir** con ella, y se puede mirar:

```bash
sudo apt install linux-perf
sudo sysctl kernel.perf_event_paranoid=1
perf stat -e cycles,instructions ./tu-programa    # IPC > 1 prueba superescalaridad
grep -o 'ibrs\|ibpb\|stibp\|md_clear' /proc/cpuinfo | sort -u   # mitigaciones de Spectre
cat /sys/devices/system/cpu/vulnerabilities/*     # que vulnerabilidades tiene tu chip
```

Ese último comando es el más ilustrativo del concepto: es una lista de formas en que **el desorden y la especulación se filtraron**, con la mitigación que Linux aplica a cada una y cuánto rendimiento cuesta.

## Cómo lo hace Kornelia

Igual que Linux: no hace nada, porque no hay nada que hacer. Lo que sí aparece son las consecuencias, y una es la más didáctica del proyecto:

**`client.py --kvm` es una prueba distinta, no solo más rápida.** Emulado, el código del agente lo interpreta un programa que ejecuta estrictamente en orden y nunca especula. Con KVM lo ejecuta el silicio de verdad, con su planificador y su predictor. Un kernel que anda emulado y se rompe con KVM tiene casi siempre un bug de ordenamiento de memoria — y el emulador se lo estaba tapando. Ver [[Maquina-virtual]].

Y la otra: el retiro en orden es lo que hace posible P5. Cuando un [[Fault|fault]] ocurre, el estado que se captura es exactamente el de antes de esa instrucción — *excepciones precisas*. Sin eso no habría "el estado en el momento del fault" que devolver por el cable, porque habría medio resultado de instrucciones posteriores ya aplicado.

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| Dos medidas de tiempo seguidas salen al revés. | Falta la barrera antes de leer el contador. |
| Anda emulado y falla con KVM. | Falta una barrera de memoria: el emulador ejecutaba en orden y la tapaba. |
| Anda en x86 y falla en ARM. | Lo mismo. x86 tiene modelo de memoria fuerte y **esconde** barreras faltantes que ARM expone. Es la razón de fondo de D22/D23. |
| Un aparato hace las cosas en el orden equivocado. | El procesador reordenó dos escrituras a MMIO. Falta una barrera, o el mapeo no está marcado como dispositivo. |
| Un bucle "vacío" para esperar no espera nada. | El compilador lo borró — y aunque no lo borre, el procesador no lo va a ejecutar en orden respecto de lo que venía midiendo. |

## Práctica

- [[P04-Ver-al-procesador-desordenar]] — *(construir)* la misma cantidad de sumas, encadenadas y en paralelo. La diferencia **es** el desorden, medido.

## Recordar #flashcards/conceptos

¿Cómo puede el procesador ejecutar fuera de orden si es sincrónico?::Ser sincrónico y ser secuencial son cosas independientes. En cada flanco todas las piezas dan un paso a la vez; el desorden cambia *cuál* instrucción entra a cada unidad, no *cuándo* pasan las cosas.

¿Cuáles son las tres piezas y en qué orden trabaja cada una?::Traer y decodificar, **en orden**; el planificador, que elige de una pileta las que tienen operandos listos, **desordenado**; y el buffer de reordenamiento, que confirma los resultados **en orden** otra vez.

¿Qué es el renombrado de registros y qué problema resuelve?::Hay muchos más registros físicos que nombres, así que cada escritura estrena uno. Elimina las dependencias **falsas**, las que existen solo porque el compilador recicló un nombre.

¿Por qué el retiro en orden es necesario para que un fault sea útil?::Porque da excepciones precisas: el estado capturado es exactamente el de antes de la instrucción que falló. Sin eso no habría un "estado en el momento del fault" que devolver.

¿Por qué un bug de ordenamiento aparece con KVM y no emulado?::Porque el emulador ejecuta estrictamente en orden y no especula. El silicio real reordena, y ahí se ve la barrera que falta.

## Ver también

- [[03-Que-hace-realmente-una-instruccion]] — el capítulo de donde sale esto.
- [[Ciclo]] · [[Cache]] · [[Registro]] · [[42-Ordenamiento-de-memoria]]
