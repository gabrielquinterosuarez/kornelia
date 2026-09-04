---
tipo: practica
clase: construir
contra: ninguno
estado: pendiente
riesgo: ninguno
conceptos: [Flip-flop, Registro]
---

# P06 · Simular un latch y ver sus dos estados

> [!success] Qué vas a ver si funciona
> Tres cosas, en orden de importancia:
> 1. Un anillo de dos inversores que **se queda** en el valor que le tocó — un bit guardado, hecho con nada más que una tabla de verdad.
> 2. Un latch SR que se acuerda, y al que se le puede escribir.
> 3. El estado prohibido: una combinación que el circuito acepta y de la que **no se puede salir de forma predecible**. En la simulación se ve literalmente como un bucle que no se asienta.

Esta práctica no toca hardware ni kernel: es cuarenta líneas de Python. Está acá porque el [[Flip-flop]] es el único concepto del libro que **no se puede mirar** con `/proc` ni con QEMU —está abajo de todo lo observable— y simularlo es la única forma de que deje de ser una frase.

## Correrla

```bash
cd libro/practicas/codigo
python3 latch.py
```

No hace falta instalar nada. El programa entero usa una sola primitiva:

```python
def nor(a, b):
    return 0 if (a or b) else 1
```

Todo lo demás —acordarse, escribir, el estado indeterminado— sale de **conectar salidas a entradas**. Ese es el punto de la práctica: la memoria no es una primitiva, es una consecuencia de la realimentación.

## Qué mirar en cada parte

### Parte 1 — el anillo

```
arranca en 0 -> [0, 0, 0, 0, 0, 0, 0]  (se queda)
arranca en 1 -> [1, 1, 1, 1, 1, 1, 1]  (se queda)
```

**Qué estás viendo:** dos valores igual de válidos, y ninguna regla que elija entre ellos. Eso es exactamente por qué la memoria es binaria: dos es la cantidad de estados en que un anillo así se sostiene solo.

Fijate también en lo que **no** se puede hacer: no hay ningún argumento en `ring()` que permita cambiar el valor a mitad de camino. El anillo se acuerda perfecto y es inútil.

### Parte 2 — el latch SR

```
 S  R   Q  Q_neg   que hizo
 0  0   0    1     se acuerda
 1  0   1    0     set: pone Q en 1
 0  0   1    0     se acuerda
 0  1   0    1     reset: pone Q en 0
 0  0   0    1     se acuerda
```

**Qué estás viendo:** las tres filas que dicen "se acuerda" tienen las mismas entradas (`S=0, R=0`) y **salidas distintas**. Un circuito combinacional no puede hacer eso: si las entradas son iguales, la salida es igual. Que acá no lo sea es la prueba de que hay estado.

### Parte 3 — el estado prohibido

```
Con S=1, R=1  ->  Q=0, Q_negado=0
-> NO SE ASIENTA. Oscila para siempre entre 0 y 1.

soltando S primero  ->  Q=0
soltando R primero  ->  Q=1
```

**Qué estás viendo:** el bucle de `settle()` se queda sin vueltas. En el silicio de verdad no oscila eternamente —cae para algún lado— pero **para cuál depende de qué compuerta fue un picosegundo más rápida**. El resultado no está mal: está indeterminado.

Las dos últimas líneas lo confirman: soltando las patas de a una, el resultado es perfectamente predecible. El problema nunca fue el circuito, fue la **simultaneidad**.

> [!tip] Por qué esto vale más que el circuito que enseña
> Es la primera aparición en el libro de un patrón que va a volver en cada parte: **el hardware acepta sin quejarse combinaciones que no significan nada**, y el resultado depende de tiempos. Volvés a ver exactamente esto en [[42-Ordenamiento-de-memoria|las carreras entre núcleos]], en [[36-Nivel-contra-flanco|el pulso que se perdía]], y en el bug de este proyecto que tardó 400 corridas en reproducirse.

## Para jugar

Tres cambios que valen la pena, en orden de dificultad:

1. **Hacé un latch D.** Agregá una función que derive `S = D and enable` y `R = (not D) and enable`. Vas a ver que desaparece el estado prohibido — por construcción, `S` y `R` nunca pueden ser 1 a la vez.
2. **Hacé que las compuertas no tarden lo mismo.** Cambiá `settle()` para que actualice primero una y después la otra. El estado prohibido deja de oscilar y pasa a dar un resultado fijo… que depende de cuál actualizaste primero. Eso es el azar del silicio, escrito.
3. **Armá el flip-flop maestro-esclavo.** Dos latches D en cascada con el `enable` invertido entre ellos, y un bucle que suba y baje un reloj. Comprobá que el dato solo cambia en el flanco, aunque lo muevas veinte veces durante el nivel. Ahí tenés, entero, el mecanismo con el que está hecho un [[Registro|registro]].

## Qué mirar cuando no sale

| Síntoma | Causa probable |
|---|---|
| La parte 1 alterna `[0,1,0,1,...]` | Estás aplicando **un** inversor por vuelta en vez de dos. El anillo tiene dos: la señal tiene que dar la vuelta entera. |
| La parte 3 se asienta en vez de oscilar | Estás actualizando las compuertas de a una. Es correcto físicamente, pero tapa el fenómeno: el punto es la actualización simultánea. |
| `Q` y `Q_negado` valen lo mismo y no era la parte 3 | El latch quedó en el estado prohibido de una prueba anterior. Reiniciá con `settle(0, 1, 0, 0)`. |

## Anotaciones

*(Tuyas.)*
