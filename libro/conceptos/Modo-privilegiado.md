---
tipo: concepto
estado: pendiente
dificultad: 3
principios: [P6, P5, P2]
decisiones: [D27, D29, D12]
practicas: [P10-Correr-cli-sin-privilegio]
capitulos: [06-El-silicio-tiene-modos, 25-Como-se-baja-de-privilegio, 28-Que-es-un-proceso-y-que-queda-sin-procesos]
---

# Modo privilegiado

> El privilegio no es una convención ni una comprobación del kernel: son **bits en el silicio**, y el que hace cumplir es el procesador.

## Qué problema resuelve

Hay instrucciones que, ejecutadas por cualquiera, terminan el juego: enmascarar [[Interrupcion|interrupciones]] (`cli`), cambiar la [[Tabla-de-paginas|tabla de páginas]] (`mov cr3`), apagar la [[Cache|caché]], hablarle a un puerto de E/S. Si todo el código pudiera hacerlas, no habría nada que un kernel pudiera garantizar — ni siquiera seguir corriendo.

La respuesta podría haber sido "que el kernel revise el código antes de ejecutarlo". No es posible: revisar código arbitrario para saber qué va a hacer es el problema de la parada. Así que la respuesta es del hardware: **el procesador tiene un estado que dice cuánto puede el código de ahora**, y las instrucciones prohibidas no fallan por buena voluntad — fallan porque el silicio las rechaza.

## Cómo funciona

El mismo mecanismo con tres nombres y, en dos de los tres, **numerado al revés**. Está en [[Falsos-amigos#3]] y vale repetir la parte que confunde:

| Arquitectura | Cómo se llama | El más privilegiado | El menos |
|---|---|---|---|
| x86_64 | **Anillo** (*ring*) | **0** — el número **baja** al subir el privilegio | 3 |
| aarch64 | **Nivel de excepción** (*EL*) | **EL3** ([[Firmware|firmware]]) — el número **sube** | EL0 |
| RISC-V | **Modo** | M (máquina) | U (usuario) |

En x86_64 los anillos 1 y 2 existen y casi nadie los usa. En aarch64 un kernel normal vive en EL1, el hipervisor en EL2 y el firmware seguro en EL3. Y quién manda en x86 no es un registro aparte: **son los dos bits de abajo de `CS`**, el selector de segmento. El segmento en 64 bits ya casi no direcciona nada, pero sigue llevando el privilegio ([[Falsos-amigos#5]]).

Qué se pierde al bajar:

| Se pierde | Se conserva |
|---|---|
| Enmascarar interrupciones | **Escribirle a los registros de un [[Aparato|aparato]] [[PCIe]]** — están en memoria ([[MMIO]]) y escribirlos es una instrucción común |
| Cargar tablas de páginas propias | Leer y escribir la memoria que te dejaron alcanzable |
| Los MSR y los puertos de E/S de x86 | Aritmética, saltos, todo el cómputo |
| Instrucciones de mantenimiento de caché y [[TLB]] | |

Fijate en la fila de arriba a la derecha: **la parte central de un [[Driver|driver]] anda sin privilegio**. Es la observación que hace que D27 sea barato.

Y la transición no es una llamada: **bajar de privilegio es "volver de una excepción que nunca ocurrió"**. Se le arma al hardware el marco que espera y se ejecuta `iretq` (x86_64) o `eret` (aarch64). Subir de vuelta requiere un trap: eso es [[Syscall]].

## Cómo lo hace Linux

Linux usa exactamente dos niveles de los cuatro: anillo 0 para el kernel, anillo 3 para todo lo demás. Los anillos 1 y 2 quedaron sin uso — la portabilidad manda, y ARM y RISC-V no tienen equivalente.

Para verlo:

```bash
cat /proc/cpuinfo | grep -o ' smep\| smap\| umip' | sort -u   # las defensas del anillo 0
dmesg | grep -iE 'smep|smap'
perf stat -e 'cycles:u,cycles:k' ./mi-programa   # ciclos en usuario contra ciclos en kernel
```

`cycles:u` contra `cycles:k` es la forma más directa de *ver* la frontera: son los mismos ciclos del mismo programa, contados según de qué lado del privilegio ocurrieron.

Dos siglas que aparecen en `/proc/cpuinfo` y son la misma idea llevada más lejos:

- **SMEP** — el anillo 0 no puede **ejecutar** páginas marcadas como de usuario.
- **SMAP** — ni siquiera puede **leerlas** sin desactivarlo explícitamente (`stac`/`clac`).

Existen porque el ataque clásico es engañar al kernel para que salte a código del atacante que ya está mapeado en su propio proceso.

## Cómo lo hace Kornelia

| | |
|---|---|
| **Decisiones** | **D27** (el agente declara el privilegio), **D29** (en el núcleo del protocolo manda el kernel), D12 (identity map, enmendado por D27) |
| **Principios** | **P6 — el hardware hace cumplir lo que el agente declaró, no una política del kernel**; P5; P2 |
| **El verbo** | `exec {mode: "supervised" \| "raw"}` |
| **Dónde vive** | `kernel-core/src/protocol.rs:1480#exec needs mode: supervised or raw`, `kernel-x86_64/src/gdt.rs:166#gdt.0[3] = 0x00AF_FA00_0000_FFFF`, `kernel-aarch64/src/exec.rs:151#exec_supervised:` |

**`mode` es obligatorio y no tiene valor por omisión.** Eso no es rigor: es P6 al pie de la letra. Si faltara y el kernel eligiera, estaría eligiendo el kernel — que es exactamente lo que D27 le devuelve al agente. El kernel ofrece los dos y **no opina**.

- `supervised` — anillo 3 en x86_64, EL0 en aarch64.
- `raw` — el privilegio del kernel: anillo 0 / EL1.

Lo que esto compra: **`cli` ahora vuelve como fault estructurado.** Era lo único que el agente podía hacer y de lo que el kernel no podía volver — todo lo demás ya se recuperaba (un [[Fault|fault]] vuelve como dato por P5, un bucle infinito se desvía con el punto de retorno de `exec`). Con `supervised`, enmascarar es privilegiado y el intento vuelve como `protection` con los registros adentro.

Tres cosas que valen la pena mirar:

1. **En el núcleo del protocolo solo se admite `supervised`** (D29): `kernel-core/src/protocol.rs:1504#the protocol core only runs supervised`. Ahí manda el kernel, y para que eso sea verdad y no una intención, el agente **no puede *poder*** enmascarar. Si quiere el privilegio entero, reclama un núcleo — ahí la prioridad la decide él, incluido no ser molestado. Y el kernel **lo publica** en vez de dejar que se descubra chocándose: `describe exec` trae `this_core` (`kernel-core/src/protocol.rs:746#w.text("this_core")`).
2. **El privilegio y el permiso de la memoria tienen que coincidir, y la misma página no puede ser las dos cosas.** Una página marcada como alcanzable por el agente **deja de ser ejecutable con privilegio** — SMEP en x86_64, el modelo de permisos en aarch64, que ni se puede apagar. Así que el pedido se rechaza antes en vez de prometer algo que el hardware va a negar un microsegundo después, con un fault que no se parece a la causa: `kernel-core/src/protocol.rs:1526#exec supervised needs memory claimed with user`.
3. **En aarch64 el agente arranca con las interrupciones abiertas y sin poder cerrarlas.** El `SPSR` con el que se hace `eret` se toma del `DAIF` de ahora, y como `exec` se llama con los timbres abiertos (D29), quedan abiertos: `kernel-aarch64/src/exec.rs:159#sin poder cerrarlas`. En x86_64 la bajada es la misma idea con otros bits: los descriptores de anillo 3 son los mismos que los de anillo 0 con el DPL corrido, y **lo que decide qué alcanza el agente son las tablas de páginas, no el segmento**.

Y hay una consecuencia sobre D12 que conviene tener presente: cargar tablas de páginas propias —la puerta que D12 dejaba abierta— **es una instrucción privilegiada**, así que solo vale corriendo `raw`. En `supervised` el intento vuelve como fault.

> [!warning] La garantía es más angosta de lo que parece
> D27 cubre `exec` y **no** cubre `irq.install`. Un [[Handler|handler]] corre siempre privilegiado porque el hardware no entrega interrupciones sin privilegio. La división es intencional: el **cómputo** es donde el agente genera mucho código, rápido y con errores; los **handlers** son piezas chicas y cuidadas escritas una vez.

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| La máquina queda muda y no vuelve. | Código `raw` que ejecutó `cli` / `msr daifset` y no volvió. Sin un segundo escalón (NMI) no hay forma de recuperarlo. Ver [[39-Plazos-y-cortes]]. |
| `exec` devuelve `protection` en algo que debería andar. | Se declaró `supervised` y el código usa una instrucción privilegiada — cargar tablas, tocar un MSR, un puerto de E/S. |
| El pedido se rechaza antes de correr. | El privilegio declarado y el permiso de la memoria no coinciden: `supervised` necesita memoria reclamada `user`, `raw` necesita memoria que **no** lo esté. |
| Andaba y al marcar los permisos dejó de andar. | Los bits de permiso **no se pueden poner antes** de que el agente corra sin privilegio: mientras corra con privilegio, marcarlos rompe `exec`. Van juntos o no van. |
| `exec raw` contesta un error de núcleo. | Se pidió `raw` en el núcleo del protocolo, donde solo se admite `supervised` (D29). Hay que reclamar un núcleo. |

## Práctica

- [[P10-Correr-cli-sin-privilegio]] — *(construir)* `./scripts/client.py --supervised`: subir código que ejecuta `cli` y ver el fault volver en vez de que la máquina se muera. Después el mismo código con `mode: raw` en un núcleo reclamado, y ver la diferencia.
- [[P01-Preguntarle-a-Linux-que-maquina-es]] — *(mirar)* `perf stat -e cycles:u,cycles:k` para ver la frontera contada en ciclos.

## Recordar #flashcards/conceptos

En x86 el privilegio más alto es el anillo…::0. Y en aarch64 EL1 para un kernel (EL3 es el firmware): los números van al revés. Es una fuente inagotable de errores al leer código de las dos.

¿Qué se pierde y qué se conserva al correr sin privilegio?::Se pierden enmascarar interrupciones, cargar tablas de páginas, los MSR y los puertos de E/S. Se conserva la parte central de un driver: escribirle a los registros de un aparato PCIe, que están en memoria y son una instrucción común.

¿Por qué `mode` es obligatorio en `exec` y no tiene valor por omisión?::Porque un valor por omisión sería el kernel eligiendo, y D27 existe para devolverle esa elección al agente (P6). El kernel ofrece los dos y no opina.

¿Qué gana Kornelia con `supervised`?::Que `cli` vuelva como fault estructurado. Era lo único que el agente podía hacer y de lo que el kernel no podía volver; todo lo demás ya se recuperaba.

¿Por qué en el núcleo del protocolo solo se admite `supervised`?::Porque ahí manda el kernel (D29), y para que eso sea verdad el agente no puede *poder* enmascarar las interrupciones — que es privilegiado. Si quiere `raw`, reclama un núcleo propio.

¿Por qué una página no puede ser alcanzable por el agente y ejecutable por el kernel a la vez?::Porque el hardware lo prohíbe: SMEP en x86_64, y en aarch64 está metido en el modelo de permisos y no se puede apagar. O es del agente, o el kernel la ejecuta.

¿Cómo se baja de privilegio, mecánicamente?::Volviendo de una excepción que nunca ocurrió: se le arma al hardware el marco que espera y se ejecuta `iretq` (x86_64) o `eret` (aarch64).

## Ver también

- [[Syscall]] · [[Fault]] · [[Handler]] · [[Registro]]
- [[Falsos-amigos#3]] — anillo: tres cosas distintas.
- [[25-Como-se-baja-de-privilegio]] · [[39-Plazos-y-cortes]] · [[Memoria-virtual]]
