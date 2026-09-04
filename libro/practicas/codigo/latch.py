#!/usr/bin/env python3
"""Un bit de memoria, simulado compuerta por compuerta.

No hay nada de alto nivel aca: solo `nor`, que es una tabla de verdad de dos
entradas, y un bucle que deja que las senales se asienten. Todo lo demas
—acordarse, escribir, y el estado prohibido— sale de conectar las salidas a las
entradas.

    ./latch.py
"""


def nor(a, b):
    """Si cualquiera de las dos entradas es 1, la salida es 0."""
    return 0 if (a or b) else 1


def inverter(a):
    """Un NOR al que se le atan las dos patas juntas."""
    return nor(a, a)


# ---------------------------------------------------------------------------
# 1. Dos inversores en circulo: se acuerda, pero no se le puede escribir.
# ---------------------------------------------------------------------------

def ring(start, steps=6):
    """Arranca el anillo en un valor y mira si se queda ahi."""
    a = start
    history = [a]
    for _ in range(steps):
        b = inverter(a)      # el segundo inversor ve la salida del primero
        a = inverter(b)      # y su salida vuelve a la entrada del primero
        history.append(a)
    return history


print("1. DOS INVERSORES EN CIRCULO")
print("   La salida de cada uno entra al otro. No hay entrada de datos.\n")
for start in (0, 1):
    h = ring(start)
    quieto = "se queda" if len(set(h)) == 1 else "cambia"
    print(f"   arranca en {start} -> {h}  ({quieto})")
print("\n   Los dos valores son estables. El anillo se acuerda del que le toco,")
print("   y no hay forma de cambiarlo desde afuera: falta una puerta de entrada.\n")


# ---------------------------------------------------------------------------
# 2. Latch SR: los mismos dos inversores, pero con una pata libre cada uno.
# ---------------------------------------------------------------------------

def settle(q, qn, s, r, limit=20):
    """Deja que el latch se asiente. Devuelve (q, qn, vueltas) o None si oscila.

    Las dos compuertas se evaluan a la vez, que es lo que hace el silicio si
    los dos caminos tardan lo mismo.
    """
    for step in range(1, limit + 1):
        new_q = nor(r, qn)
        new_qn = nor(s, q)
        if (new_q, new_qn) == (q, qn):
            return q, qn, step
        q, qn = new_q, new_qn
    return q, qn, None


print("2. LATCH SR")
print("   Q = NOR(R, Q_negado)   y   Q_negado = NOR(S, Q)\n")
print("    S  R   Q  Q_neg   que hizo")
print("   " + "-" * 44)

q, qn = 0, 1                                  # arranca acordandose de un 0
for s, r, what in [(0, 0, "se acuerda"),
                   (1, 0, "set: pone Q en 1"),
                   (0, 0, "se acuerda"),
                   (0, 1, "reset: pone Q en 0"),
                   (0, 0, "se acuerda")]:
    q, qn, steps = settle(q, qn, s, r)
    print(f"    {s}  {r}   {q}    {qn}     {what}")

print("\n   Fijate en las filas 'se acuerda': con S=0 y R=0 la salida es la que")
print("   habia quedado. Eso es un bit guardado.\n")


# ---------------------------------------------------------------------------
# 3. El estado prohibido, que es de donde sale la moraleja.
# ---------------------------------------------------------------------------

print("3. S=1 Y R=1 AL MISMO TIEMPO")
q, qn, steps = settle(q, qn, 1, 1)
print(f"   Con S=1, R=1  ->  Q={q}, Q_negado={qn}")
print("   Las dos salidas valen lo mismo. Ya no son opuestas: 'Q negado' miente.\n")

print("   Y ahora lo importante: que pasa al soltar las dos patas a la vez.\n")
_, _, steps = settle(q, qn, 0, 0)
if steps is None:
    print("   -> NO SE ASIENTA. Oscila para siempre entre 0 y 1.")
    print("      En el silicio de verdad no oscila eterno: cae para un lado,")
    print("      y para cual depende de cual compuerta fue un picosegundo mas")
    print("      rapida. O sea: del azar.\n")

print("   Si en cambio se sueltan de a una, el resultado es predecible:\n")
for first in ("S", "R"):
    a, b = (0, 1) if first == "S" else (1, 0)
    t_q, t_qn, _ = settle(0, 0, a, b)          # se suelta una
    t_q, t_qn, _ = settle(t_q, t_qn, 0, 0)     # y despues la otra
    print(f"   soltando {first} primero  ->  Q={t_q}")

print("""
   MORALEJA
   Hay combinaciones que el hardware acepta sin quejarse y que no
   significan nada: el resultado no esta mal, esta indeterminado, y
   depende de tiempos. Es la misma familia de problemas que una carrera
   entre dos nucleos, varios pisos mas abajo.
""")
