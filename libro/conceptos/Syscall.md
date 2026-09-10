---
tipo: concepto
estado: pendiente
dificultad: 2
principios: [P4, P3]
decisiones: [D27, D3, D18]
practicas: [P11-Contar-las-syscalls-de-un-programa]
capitulos: [26-La-llamada-al-sistema, 25-Como-se-baja-de-privilegio, 09-Que-cuesta-una-abstraccion]
---

# Syscall

> La puerta de vuelta. Desde código sin privilegio no se puede *llamar* al kernel: hay que **provocar una excepción a propósito** para que el silicio suba el privilegio por vos.

Es un **trap**, no un fault: una instrucción que *pide* ser interrumpida, y de la que se vuelve a la **siguiente** instrucción. Ver [[Falsos-amigos#2]].

## Qué problema resuelve

[[Modo-privilegiado|Bajar de privilegio]] es fácil: `iretq` o `eret` y listo. El problema es volver.

Un `call` no sirve: llamar a una dirección no cambia el privilegio, así que el código saltaría al kernel **con el privilegio del que llamó** — y entonces el privilegio no significaría nada. Un `ret` tampoco: se entró por una excepción, no por un `call`, y no hay dirección de retorno esperando en la pila.

La única forma de subir de privilegio es que **el hardware lo haga**, y el hardware solo lo hace al tomar una excepción. Así que la llamada al sistema es una excepción pedida a propósito: el código sin privilegio ejecuta una instrucción cuyo único efecto es "entrá al kernel por la puerta que él dejó abierta".

Lo importante es que **el que llama no elige a dónde salta**. El destino lo puso el kernel antes, en una tabla o en un registro de sistema. Eso es lo que hace que sea una puerta y no un agujero.

## Cómo funciona

Tres generaciones, y las tres siguen existiendo:

| | Instrucción | Cómo sabe a dónde ir | Costo |
|---|---|---|---|
| Vieja (x86, 32 bits) | `int 0x80` | Entrada 0x80 de la IDT, con `DPL=3` para que anillo 3 pueda invocarla | Caro: es una interrupción completa |
| Hoy (x86_64) | `syscall` / `sysret` | Registros de sistema (MSR): `LSTAR` tiene el destino, `STAR` los selectores, `SFMASK` qué banderas limpiar | Barato: no toca la IDT ni la memoria |
| aarch64 | `svc #imm` / `eret` | La tabla de vectores, en el slot de "excepción sincrónica desde el nivel de abajo" | Barato |

Y la **ABI de la llamada**: qué registro lleva el número y cuáles los argumentos. No hay un estándar; lo fija cada sistema, y hasta cambia entre las generaciones de la misma arquitectura.

| | Número | Argumentos | Vuelve en |
|---|---|---|---|
| Linux x86_64 (`syscall`) | `rax` | `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9` | `rax` |
| Linux x86 32 bits (`int 0x80`) | `eax` | `ebx`, `ecx`, `edx`, `esi`, `edi`, `ebp` | `eax` |
| Linux aarch64 (`svc #0`) | `x8` | `x0`–`x5` | `x0` |

> [!tip] Por qué `r10` y no `rcx`
> Porque la instrucción `syscall` **usa `rcx`**: guarda ahí la dirección de retorno, y `r11` se lleva las banderas. El silicio se los pisa, así que la ABI tuvo que correr el cuarto argumento a `r10`. Es un caso perfecto de una convención que **la impone el hardware**, y de por qué [[Registro|una convención de llamada no se deduce]].

## Cómo lo hace Linux

Casi nadie escribe una syscall a mano: la libc envuelve cada una. Pero se pueden ver todas las que hace un programa:

```bash
strace -f ls /tmp                     # una línea por llamada, con argumentos y resultado
strace -c ls /tmp                     # el resumen: cuántas de cada una y cuánto tardaron
ausyscall --dump | head               # número → nombre, en esta máquina
grep __NR_write /usr/include/asm/unistd_64.h
```

`strace -c` es la más útil de las cuatro: te dice **dónde se va el tiempo** de un programa que parece lento sin razón.

Tres cosas de Linux que vale conocer:

- **El vDSO.** Algunas llamadas son tan frecuentes y tan inofensivas que ni siquiera valen el trap: `gettimeofday`, `clock_gettime`. Linux mapea un pedacito de código en cada proceso (`ldd /bin/ls` lo muestra como `linux-vdso.so.1`) que lee el reloj **sin entrar al kernel**. Una syscall que no es una syscall.
- **seccomp.** Un filtro que decide qué números de syscall puede hacer un proceso. Es lo que usan los contenedores y los navegadores para achicar la superficie.
- **Los números no se reordenan nunca.** La tabla es ABI: cambiar un número rompería todos los binarios existentes. Por eso hay huecos y nombres raros como `openat2`.

## Cómo lo hace Kornelia

| | |
|---|---|
| **Decisiones** | D27 (`supervised` necesita una puerta de vuelta), D3 (nada específico de arquitectura en el protocolo), D18 (el blob no necesita puerta) |
| **Principios** | **P4 — la máquina se describe a sí misma**; P3 |
| **Dónde vive** | `kernel-x86_64/src/exec.rs:57#pub const WINDOW_VECTOR: usize = 0x80`, `kernel-aarch64/src/exec.rs:45#pub const RETURN_BYTES`, `kernel-core/src/protocol.rs:736#w.bytes(P::EXEC_RETURN)` |

Acá no hay tabla de syscalls, porque no hay servicios que pedir desde `exec`: los once verbos llegan por el cable, no por un trap. Lo que hay es **una sola puerta con un solo significado**: *terminé*.

El proyecto la llama **la ventanilla**. Desde anillo 3 un `ret` no vuelve —se entró por un `iretq` y no por un `call`— así que el código `supervised` termina invocando el trap, y el kernel aterriza en el mismo lugar donde termina un `exec` que volvió solo.

- x86_64: `int 0x80`, o sea los bytes `CD 80` (`kernel-x86_64/src/exec.rs:60#pub const RETURN_BYTES`). Es la **única** entrada de la IDT con `DPL=3` — las demás son de anillo 0, así que el agente no puede invocar ninguna otra: `kernel-x86_64/src/idt.rs:369#las demas son de anillo 0`, `kernel-x86_64/src/idt.rs:377#idt.0[crate::exec::WINDOW_VECTOR]`.
- aarch64: `svc #0`, o sea `01 00 00 D4`.

Y acá está lo que hace a esta nota parte de este libro:

> [!important] Los bytes se publican, no se hornean
> `describe exec` devuelve el campo `return` con **los bytes exactos** que abren la ventanilla. No el nombre de la instrucción —`int` en x86_64, `svc` en aarch64— sino los bytes, para que el agente los pegue al final de lo que emite **sin saber sobre qué silicio corre** (D3, P4). El kernel también publica de dónde sale la pila (`claim-end`: el final del mismo reclamo) y por qué registros pasan los argumentos.

Un detalle de x86_64 que es puro sistema: la compuerta es de **interrupción**, no de trap, así que se entra con las [[Interrupcion|interrupciones]] cerradas. Y el CPU cambia solo a la pila de anillo 0 que está en el TSS (`kernel-x86_64/src/gdt.rs:154#tss.rsp[0]`). Sin eso, la ventanilla de vuelta aterrizaría **sobre la pila del agente** — que es justo la que puede estar rota.

Y un contraste que aclara todo el concepto: **el blob usa dos ventanillas distintas, y la diferencia entre ellas es qué pasa después.** El blob de D18 corre `supervised` desde que D29 cerró ese agujero —era el único código que corría con privilegio completo en el núcleo del protocolo—, así que cruza una frontera de verdad y necesita una puerta. Pero necesita dos:

| Puerta | x86_64 | aarch64 | Qué significa |
|---|---|---|---|
| **De salida** | `int 0x80` | `svc #0` | "Terminé." No se vuelve. |
| **De servicio** | `int 0x81` | `svc #1` | "Atendeme esto y devolveme el control." Se vuelve **en la instrucción siguiente**. |

Son dos puertas y no un registro que las distinga por dos razones: el código que vuelve ya usa el primer registro para su resultado, y dos puertas con dos significados **se leen**. Los bytes de las dos los publica `describe {what:["exec"]}`, así que el agente no los tiene horneados (D3, P4).

Al entrar, el blob recibe en el primer registro de argumento **su propia dirección, y nada más** (`blob/src/main.rs:90#pub extern "win64" fn blob_entry`). Con eso alcanza: todo lo demás lo pide por la puerta de servicio, y adentro es el mismo `dispatch` de los once verbos con un origen más.

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| El `exec supervised` no vuelve nunca. | El código no emitió los bytes de la ventanilla al final, o emitió los de la otra arquitectura. Por eso se publican en vez de hornearse. |
| `#GP` / `protection` al invocar la ventanilla. | La entrada de la tabla no tiene `DPL=3`: desde anillo 3 no se la puede invocar. |
| La ventanilla entra y la máquina se muere ahí. | La pila. Si el CPU no cambia a una pila de anillo 0 conocida, el marco se apila sobre la del agente. |
| Una función ve punteros nulos aunque el que llama pasó argumentos correctos. | La ABI del **target**, no de la arquitectura. En `x86_64-unknown-uefi`, `extern "C"` es la convención de Windows: RCX, RDX, R8, R9. Ver [[27-La-ABI-la-pone-el-target-no-el-silicio]]. |
| La llamada aparece en `strace` con un número y ningún nombre. | La tabla de `ausyscall` de tu máquina es más vieja que el kernel, o es una syscall de otra arquitectura. |

## Práctica

- [[P11-Contar-las-syscalls-de-un-programa]] — *(mirar)* `strace -c ls`, `strace -c curl`, y encontrar la llamada donde se va el tiempo. Después `ldd /bin/ls` para ver el vDSO.
- [[P10-Correr-cli-sin-privilegio]] — *(construir)* pedirle a `describe exec` los bytes de `return`, pegarlos al final del código y ver el `exec supervised` volver.

## Recordar #flashcards/conceptos

¿Por qué no se puede volver al kernel con un `call`?::Porque un `call` no cambia el privilegio: saltaría al kernel con el privilegio del que llamó. La única forma de subir es que lo haga el hardware, y solo lo hace al tomar una excepción.

¿Qué registro lleva el número de syscall en Linux x86_64 y en aarch64?::`rax` en x86_64 (argumentos en rdi, rsi, rdx, r10, r8, r9) y `x8` en aarch64 (argumentos en x0–x5). En los dos vuelve en el primero: rax / x0.

¿Por qué el cuarto argumento de una syscall en x86_64 va en `r10` y no en `rcx`?::Porque la instrucción `syscall` pisa `rcx` con la dirección de retorno y `r11` con las banderas. La ABI tuvo que correr el argumento: es el hardware imponiendo una convención.

¿Qué es el vDSO?::Un pedacito de código que Linux mapea en cada proceso para resolver llamadas muy frecuentes e inofensivas —leer el reloj— **sin entrar al kernel**. Una syscall que no es una syscall.

¿Qué es "la ventanilla" en Kornelia?::La única puerta de vuelta del código `supervised`: `int 0x80` en x86_64, `svc #0` en aarch64. Significa una sola cosa, "terminé", y es la única entrada de la IDT con DPL=3.

¿Por qué `describe` publica los **bytes** de la ventanilla y no el nombre de la instrucción?::Para que el agente los pegue al final de lo que emite sin saber sobre qué silicio corre (D3, P4). Un nombre de instrucción lo obligaría a saber la arquitectura.

¿Por qué el blob de Kornelia no necesita ventanilla?::Porque corre con privilegio completo y en el mismo espacio de direcciones: llamar al kernel es una instrucción común. La ventanilla existe porque hay una frontera de privilegio, no porque haya que hablarle al kernel.

## Ver también

- [[Modo-privilegiado]] · [[Fault]] · [[Registro]]
- [[Falsos-amigos#2]] — trap contra fault: del trap se vuelve a la **siguiente** instrucción.
- [[Syscall]] · [[27-La-ABI-la-pone-el-target-no-el-silicio]] · [[51-El-blob-y-la-ventana-de-rescate]]
