---
tipo: practica
clase: construir
contra: linux
estado: pendiente
riesgo: ninguno
conceptos: [Registro, Espacio-de-direcciones]
---

# P03 · Ver dónde se acaban los registros

> [!success] Qué vas a ver si funciona
> Tres cosas, en el desensamblado de **tu** compilador:
> 1. Dos funciones que hacen la misma cuenta, y una toca la memoria solo porque alguien pidió una dirección.
> 2. La línea exacta donde la convención de llamada se queda sin registros y empieza a leer de la pila.
> 3. Un contador: cuántas de las instrucciones de una función son derrame.

Sin `sudo`, sin riesgo, sin instalar nada más que `gcc` y `binutils`. El código está en `libro/practicas/codigo/registros.c`.

## Compilar y abrir

```bash
cd libro/practicas/codigo
cc -O2 -c -o /tmp/registros.o registros.c
objdump -d /tmp/registros.o | less
```

`-c` compila sin enlazar (no hay `main`, no queremos un ejecutable) y `objdump -d` **desensambla**: traduce los bytes de vuelta a texto de ensamblador. Los bytes *son* el programa; el texto es para vos.

**`-O2` no es opcional.** Sin optimizar, el compilador manda todo a la pila y las tres demostraciones desaparecen: verías memoria por todos lados y no aprenderías nada. Vale la pena comprobarlo al final.

---

## Parte 1 · Pedir una dirección echa al valor del registro

```bash
objdump -d /tmp/registros.o | sed -n '/<sin_direccion>:/,/ret/p'
objdump -d /tmp/registros.o | sed -n '/<con_direccion>:/,/ret/p'
```

Las dos funciones calculan `x * 3 + 1`. La única diferencia en el fuente es una línea: `usar(&y)`.

| | Tu salida |
|---|---|
| Instrucciones de `sin_direccion` | |
| ¿Alguna toca `%rsp`? | |
| Instrucciones de `con_direccion` | |
| ¿Cuántas tocan `%rsp`? | |

En la máquina donde se escribió el capítulo, `sin_direccion` son **dos instrucciones** y ninguna toca memoria: un solo `lea` hace la multiplicación y la suma juntas. `con_direccion` reserva pila, escribe el valor, llama, y lo vuelve a leer.

**Qué estás viendo:** un registro **no tiene dirección**. En cuanto el programa necesita una, el valor tiene que estar en memoria. No es que sea más lento tenerlo en el registro — es que ahí no se puede apuntar.

> [!question] Para probar
> Sacá el `usar(&y)` y recompilá. ¿Vuelve a ser de dos instrucciones? ¿Y si en vez de pasar `&y` a otra función lo guardás en una variable local que nunca usás?

---

## Parte 2 · Dónde corta la convención de llamada

```bash
objdump -d /tmp/registros.o | sed -n '/<doce_argumentos>:/,/ret/p'
```

Vas a ver una fila de `xor`, y **la mitad viene de registros y la mitad de `0xNN(%rsp)`**.

| | Tu salida |
|---|---|
| ¿Cuántos `xor` leen de un registro? | |
| ¿Cuántos leen de `(%rsp)`? | |
| ¿Qué registros son, en orden? | |

**Qué estás viendo:** el acuerdo de System V para x86_64 pasa los primeros **seis** argumentos por `rdi, rsi, rdx, rcx, r8, r9`, y del séptimo en adelante por la pila. No porque sea mejor: porque no hay más registros que gastar.

> [!warning] Ese seis no lo pone la arquitectura
> Kornelia corre en el mismo silicio y su convención pasa **cuatro**, por `RCX, RDX, R8, R9`, porque el target `x86_64-unknown-uefi` usa el acuerdo de Windows. Es el bug que costó caro: una llamada que entraba a la función correcta y veía punteros nulos. Ver [[27-La-ABI-la-pone-el-target-no-el-silicio]].
>
> Comprobalo vos, si tenés el target instalado:
> ```bash
> rustup target add x86_64-unknown-uefi
> ```
> …o simplemente mirá qué publica el kernel: `./scripts/client.py --what exec`.

---

## Parte 3 · Contar el derrame

```bash
objdump -d /tmp/registros.o | sed -n '/<muchos_vivos>:/,/ret/p' > /tmp/mv.txt
grep -c $'\t' /tmp/mv.txt            # instrucciones totales
grep -c 'rsp)' /tmp/mv.txt           # las que tocan la pila
grep 'rsp)' /tmp/mv.txt | head
```

| | Tu salida |
|---|---|
| Instrucciones totales | |
| Que tocan la pila | |
| Proporción | |

En la máquina de referencia: **76 instrucciones, 12 tocan la pila.** Eso es el compilador quedándose sin registros con veinte valores vivos y dieciséis lugares.

**Qué estás viendo:** el derrame. Fijate que las escrituras usan offsets *negativos* (`-0x10(%rsp)`): eso es la *red zone* de System V, 128 bytes debajo del puntero de pila que una función hoja puede usar **sin reservar nada**. Un detalle que en el kernel no existe — ahí la red zone se desactiva, porque una interrupción puede llegar en cualquier momento y pisarla.

> [!question] El experimento que cierra la parte
> Editá `muchos_vivos` para que sean **ocho** valores en vez de veinte y volvé a contar. Deberían desaparecer los accesos a pila: ocho entran en dieciséis registros. Ahí ves el umbral moviéndose con tus manos.

---

## Parte 4 · Comprobar que `-O2` era el punto

```bash
cc -O0 -c -o /tmp/registros-O0.o registros.c
objdump -d /tmp/registros-O0.o | sed -n '/<sin_direccion>:/,/ret/p'
grep -c 'rsp)\|rbp)' <(objdump -d /tmp/registros-O0.o)
```

Sin optimizar, **cada variable vive en la pila** y se sube a un registro solo para operarla. La función de dos instrucciones pasa a tener seis o siete, todas moviendo cosas.

**Qué estás viendo:** por qué comparar rendimiento con `-O0` no significa nada, y por qué el desensamblado de un binario de depuración es tan fácil de leer y tan poco parecido a lo que corre de verdad.

---

## Qué mirar cuando no sale

| Síntoma | Causa probable |
|---|---|
| `objdump: command not found` | `sudo apt install binutils`. |
| Las funciones no aparecen | Compilaste sin `-c` y falló el enlazado por falta de `main`. |
| `sin_direccion` toca la pila igual | Estás en `-O0`, o en una arquitectura distinta. En aarch64 el desensamblado se lee distinto pero la conclusión es la misma. |
| `doce_argumentos` no muestra el corte en 6 | Estás en aarch64 (ahí son **ocho**: `x0`–`x7`) o en Windows/[[UEFI]] (cuatro). El número es del acuerdo, no del silicio — que es justo el punto. |
| No hay derrame en `muchos_vivos` | Tu compilador fue más astuto: revisá que los valores sigan vivos hasta el `return`. Si alguno se puede descartar, no derrama. |

## Anotaciones

*(Tuyas. El número que conviene recordar es cuántos argumentos van por registro en tu plataforma — vas a chocarte con eso cada vez que leas ensamblador.)*
