---
tipo: concepto
estado: pendiente
dificultad: 3
principios: [P3, P4]
decisiones: [D9, D27]
practicas: [P07-Instalar-un-handler-en-Kornelia]
capitulos: [34-Del-cable-al-numero-PIC-APIC-GIC, 49-Escribir-un-driver]
---

# Handler

> La función que atiende una [[Interrupcion|interrupción]]. Corre encima de lo que estuviera pasando, no puede bloquearse y no puede tardar.

## Qué problema resuelve

Ninguno por sí sola: un handler es lo que hay **del otro lado** del timbre. El problema que resuelve es más específico: cuando el [[Aparato|aparato]] avisa, alguien tiene que atenderlo, y ese alguien no puede ser el programa que estaba corriendo, porque el programa no sabe nada del aparato ni pidió que lo molesten.

Así que el handler es código que corre **prestado**: se mete en el medio, hace lo mínimo, y devuelve el control como si nada hubiera pasado.

## Cómo funciona

Lo que hace único al contexto de un handler es lo que **no** hay:

| No hay | Por qué |
|---|---|
| Un proceso | Interrumpió a cualquiera. En Linux, `current` apunta a la tarea que estaba, que no tiene nada que ver con el aparato. |
| Una pila propia (a veces) | Suele correr sobre la pila de quien interrumpió. Si esa pila estaba casi llena, el handler la desborda. |
| Derecho a dormir | Si se bloquea, no hay a quién ceder: el scheduler no puede correr acá. Un `sleep` en un handler cuelga la máquina. |
| Tiempo | Mientras corre, esa interrupción —y las de menos prioridad— están tapadas. Tardar es perder timbres. |

De ahí sale la regla entera: **hacer lo mínimo, anotar, y salir**. Casi siempre el mínimo es leer el registro de estado del aparato (que además limpia la bandera de interrupción), meter lo que llegó en un buffer, y avisarle al controlador que se atendió (**EOI**).

Y hay algo que el hardware impone y conviene saber desde el principio: **un handler corre privilegiado**. No hay forma de que no lo haga. En x86_64 la entrada de la tabla exige anillo 0; en aarch64 la excepción entra en EL1. El silicio no sabe entregar una interrupción a un nivel sin privilegio. Ver [[Modo-privilegiado]].

## Cómo lo hace Linux

Linux resuelve "no puedo tardar" partiendo el handler en dos, y le puso nombre a las mitades.

| Mitad | Cuándo corre | Qué puede hacer |
|---|---|---|
| **Top half** (el handler de verdad, el de `request_irq`) | Ya, con la interrupción tapada. | Casi nada: leer el registro, guardar los datos, agendar la otra mitad. Devuelve `IRQ_HANDLED` o `IRQ_NONE`. |
| **Bottom half** | Después, con los timbres abiertos. | Trabajo largo. Sigue sin poder dormir si es *softirq* o *tasklet*. |

Las formas concretas de bottom half, de más vieja a más nueva:

- **softirq** — fija en tiempo de compilación, hay diez y pico. Se ven en `/proc/softirqs`, y el hilo que las corre cuando se acumulan es `ksoftirqd/N` (aparece en `top` cuando la máquina está bajo carga de red).
- **tasklet** — una softirq genérica que un [[Driver|driver]] puede pedir. En camino de deprecación.
- **workqueue** — corre en un hilo del kernel de verdad, así que **sí puede dormir**. Los hilos se llaman `kworker/...`.
- **threaded IRQ** — `request_threaded_irq(irq, top, hilo, ...)`: el top half decide con `IRQ_WAKE_THREAD` y Linux despierta un hilo dedicado. Es lo que usa `PREEMPT_RT` para *todo*, porque un handler que corre en un hilo se puede desalojar y planificar.

Para mirarlo:

```bash
cat /proc/softirqs          # cuántas de cada clase, por núcleo
ps aux | grep -E 'ksoftirqd|kworker|irq/'
cat /proc/interrupts        # la columna con el nombre es el que pasó request_irq
```

Y `dmesg` avisa cuando un handler se porta mal: `irq NN: nobody cared (try booting with irqpoll)` significa que la interrupción sonó y ningún handler de la cadena dijo "es mía".

## Cómo lo hace Kornelia

| | |
|---|---|
| **Decisiones** | D9 (el agente escribe el handler), D27 (los handlers **no** entran en la declaración de privilegio) |
| **Principios** | P3, P4 |
| **Los verbos** | `irq.install` (el kernel pone prólogo, epílogo y EOI), `irq.install_raw` (el agente pone todo) |
| **Dónde vive** | `kernel-core/src/handlers.rs:43#pub struct Handler`, `kernel-x86_64/src/irq.rs:651#agent_comun:` |

No hay top half ni bottom half, porque no hay scheduler al que ceder (P2). Lo que hay es el patrón que D9 anticipa y el agente implementa: **el handler escribe en un buffer circular y el agente lo lee cuando vuelve.** La máquina junta eventos sola durante horas; el agente aparece después y lee la historia. Eso es posible porque los handles pertenecen a la máquina, no a la conexión (D14).

El "prólogo, epílogo y EOI" que promete `irq.install` son literalmente eso: en x86_64, un stub por ranura que apila su número y salta a un tramo común que salva registros, llama, los restaura y hace `iretq` (`kernel-x86_64/src/irq.rs:651#agent_comun:`). La variante `raw` no restringe menos — `kernel-core/src/protocol.rs:2064#no restringe menos`— son veinte bytes que el agente escribiría igual. **No es un guardarraíl** lo que se saca.

Dos cosas del `Handler` que valen por sí solas:

- **`count`** — cuántas veces se atendió: `kernel-core/src/handlers.rs:52#pub count: u64`, y se incrementa desde el camino de la interrupción (`kernel-core/src/handlers.rs:165#pub fn served`). El agente lo lee con `describe` para saber si su aparato está hablando **sin tener que instrumentar su propio código**. Un contador que nadie puede leer no existe.
- **`trigger`** — las escrituras que hacen sonar esa interrupción **a propósito**. Sirve para probar el handler sin depender de que el aparato se digne a hablar, y es la técnica que resolvió el bug del pulso perdido: *separar las dos mitades*. Ver [[MSI]].

> [!warning] D27 tiene un agujero declarado, y está declarado a propósito
> El agente declara si su código corre `supervised` o `raw`… **pero eso solo cubre `exec`.** Un handler de `irq.install` corre **siempre** privilegiado, porque el hardware no entrega interrupciones sin privilegio, y desde ahí puede enmascarar. Bajarlos costaría una transición de privilegio en **cada** interrupción, y D9 dice justamente que el kernel no está en ese camino. La división que queda es defendible: el **cómputo** es donde el agente genera mucho código, rápido y con errores, y ese va supervisado; los **handlers** son piezas chicas y cuidadas —veinte líneas que llenan un buffer— escritas una vez.

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| La máquina se cuelga apenas suena la interrupción. | El handler se bloqueó, o tardó, o volvió sin EOI. |
| `irq NN: nobody cared` en `dmesg`. | Ningún handler de la cadena reconoció la interrupción como suya. Casi siempre: el aparato equivocado, o un `IRQ_NONE` mal devuelto. |
| El handler corre pero el aparato sigue levantando la mano. | No se leyó el registro que **limpia** la bandera del aparato. Ver [[MMIO]]: leer tiene efecto. |
| Se pierden eventos bajo carga. | El buffer donde el handler deja lo que llegó es más chico que la ráfaga. **Pasó acá:** el anillo del cable serie tenía 4 KiB y el protocolo acepta pedidos de 64 KiB. El síntoma no se parecía a la causa — un `mem.write` colgaba la máquina esperando el resto de un CBOR que ya se había perdido. Ver [[Indice-de-sintomas]]. |
| Silencio total, ni una letra por el cable. | **Bucle de [[Fault|faults]]**: el handler provoca la misma excepción que vino a atender. Caso real en aarch64: un núcleo arrancado por PSCI viene con los registros SIMD atrapados (`CPACR_EL1` en cero), el compilador usa registros anchos para copiar structs, y el handler repite la falla al copiar la suya. |
| Instrumentar el handler hace desaparecer el bug. | Escribir una letra por el cable desde un handler del reloj movió el timing lo suficiente. Para algo que depende de tiempos: dejar el dato en un estático y publicarlo por `describe`. |

## Práctica

- [[P07-Instalar-un-handler-en-Kornelia]] — *(construir)* `./scripts/client.py --handler`: instalar un handler del agente y hacerlo sonar con la escritura que devuelve el propio `irq.install`.
- [[P06-Ver-las-interrupciones-de-una-maquina]] — *(mirar)* `/proc/softirqs` y `ksoftirqd` bajo carga de red, para ver la mitad de abajo trabajando.

## Recordar #flashcards/conceptos

¿Por qué un handler no puede dormir?::Porque no hay a quién ceder: interrumpió a cualquiera y el scheduler no puede correr en ese contexto. Un bloqueo ahí cuelga la máquina.

¿Qué son el top half y el bottom half de Linux?::Las dos mitades en que se parte un handler. El top half corre ya, con la interrupción tapada, y hace lo mínimo. El bottom half (softirq, tasklet, workqueue, threaded IRQ) corre después con los timbres abiertos.

¿Cuál de los bottom halves de Linux sí puede dormir?::La **workqueue** (y el hilo de un threaded IRQ), porque corren en un hilo del kernel de verdad. Las softirqs y los tasklets no.

¿Por qué un handler del agente en Kornelia corre siempre privilegiado?::Porque el hardware no sabe entregar una interrupción a un nivel sin privilegio: en x86_64 la entrada de la IDT exige anillo 0 y en aarch64 la excepción entra en EL1. D27 lo dice explícitamente en vez de esconderlo.

¿Para qué sirve el campo `count` de un handler instalado?::Para que el agente sepa si su aparato está hablando sin instrumentar su propio código: lo lee con `describe`. Un contador que nadie puede leer no existe.

¿Qué es el `trigger` que devuelve `irq.install`?::Las escrituras con las que se hace sonar esa interrupción a propósito. Deja probar el handler sin el aparato — que es la técnica de "separar las dos mitades" con la que se encontró el bug del pulso perdido.

## Ver también

- [[Interrupcion]] · [[MSI]] · [[Modo-privilegiado]] · [[MMIO]]
- [[Falsos-amigos#12]] — IRQ, línea, vector, MSI.
- [[Driver]] — el modelo de Linux contra los once verbos.
