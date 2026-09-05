---
tipo: concepto
estado: pendiente
dificultad: 1
principios: [P4]
decisiones: [D3]
practicas: [P02-Medir-el-reloj-y-las-latencias]
capitulos: [01-El-reloj-y-el-transistor, 02-Registros-y-RAM-no-son-lo-mismo]
---

# Registro

> Un puñado de celdas adentro del procesador. La única memoria que no cuesta ciclos alcanzar, y la única que las instrucciones pueden nombra directamente.

Ojo con la palabra: **"registro" también se dice de los registros de un [[Aparato|aparato]]**, que son una cosa completamente distinta —una dirección de memoria que no es memoria— y viven en [[MMIO]]. Esta nota es sobre los del procesador.

## Qué problema resuelve

Una instrucción tiene que decir sobre qué opera, y ese "sobre qué" tiene que caber en la instrucción misma, que son unos pocos bytes. No hay lugar para una dirección de 64 bits en cada operando; y aunque hubiera, ir a la [[Jerarquia-de-memoria|RAM]] cuesta unos 250 ciclos y una operación aritmética cuesta uno.

Así que el procesador tiene un juego chiquito de celdas con **nombre propio**, cableadas al lado de la unidad aritmética. Nombrar una cuesta 3 o 4 bits en la instrucción, y usarla cuesta cero ciclos.

## Cómo funciona

| | x86_64 | aarch64 |
|---|---|---|
| De propósito general | 16: `rax rbx rcx rdx rsi rdi rbp rsp r8`–`r15` | 31: `x0`–`x30` (o `w0`–`w30` para usar 32 bits) |
| Puntero de instrucción | `rip` | `pc` |
| Puntero de pila | `rsp` (es uno de los 16) | `sp` (aparte de los 31) |
| Banderas | `rflags` | `nzcv` (parte de `pstate`) |
| Nombres heredados | Sí: `rax` era `ax` en el 8086, de 16 bits | No: los renombraron en el salto a 64 bits |

Los nombres de x86 arrastran cuarenta años: `rax` es el mismo registro que `eax` (32 bits), que `ax` (16), que `al` (8 bits bajos). Escribir en `eax` **pone en cero la mitad de arriba** de `rax`, y escribir en `al` no. Es una de las asimetrías que hacen que el ensamblador de x86 se lea raro.

Y hay registros que no son de propósito general y mandan más que ellos: los **de control**. `cr3` en x86_64 apunta a la [[Tabla-de-paginas|tabla de páginas]] activa; `ttbr0_el1` hace lo mismo en aarch64. Cambiar uno de esos cambia el mundo entero que ve el código.

## Por qué importan tanto en un kernel

**El estado visible de un procesador es un puñado de números.** Los registros más unos de control, y nada más. De ahí salen dos cosas centrales:

- **Un cambio de contexto es copiar registros.** Guardar los de un programa y poner los de otro *es* cambiar de programa. No hay más magia.
- **Un [[Fault|fault]] se puede contar entero.** Cuando el código se rompe, "qué estaba pasando" cabe en una respuesta: los registros, más la causa.

## Cómo lo hace Linux

Los registros del momento de un crash aparecen en `dmesg` cuando hay un `oops`, con sus nombres horneados:

```
RIP: 0010:mi_funcion+0x1a/0x40
RAX: 0000000000000000 RBX: ffff8881040a8000 RCX: 0000000000000000
```

Y `ptrace` / `gdb` los leen de un proceso vivo. Están **nombrados en la interfaz**: la estructura `user_regs_struct` de Linux tiene un campo `rax`, lo cual significa que la interfaz de Linux sabe que está en x86.

## Cómo lo hace Kornelia

Acá aparece una de las decisiones más chicas y más ilustrativas del proyecto (D3, P4): **el protocolo no lleva nombres de registros horneados.**

x86_64 tiene RAX; ARM64 tiene X0–X30; RISC-V tiene x0–x31. Si el protocolo dijera "poné esto en RAX", el protocolo sería de x86 disfrazado de genérico. Entonces la máquina **informa qué registros tiene**, y el agente los nombra como los nombra esa máquina:

- `describe exec` publica cuáles se pueden poner, y por cuáles pasan los argumentos: `kernel-x86_64/src/exec.rs:293#pub const ARGUMENTS`.
- El orden de esa lista es el orden en que el ensamblador deja los valores. **Cambiar uno sin el otro hace que el kernel informe un registro con el nombre de otro** — ya pasó, en aarch64. Ver [[Indice-de-sintomas]].

Y la razón por la que `ARGUMENTS` **se publica en vez de deducirse** es un bug que costó caro: la convención de llamada no la pone la arquitectura, la pone el *target*. Ver [[27-La-ABI-la-pone-el-target-no-el-silicio]].

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| El kernel informa un valor con el nombre del registro equivocado. | El orden de `REGISTERS` y el orden en que el ensamblador los apila se desincronizaron. |
| Una función ve punteros nulos aunque el que llama pasó argumentos correctos. | Los argumentos están en otros registros: la ABI del target no es la que se supuso. |
| Un núcleo nuevo muere en la primera copia de un struct. | No son los registros generales sino los **SIMD**: `CPACR_EL1` en cero los deja atrapados, y el compilador usa registros anchos para copiar. |

## Práctica

- [[P02-Medir-el-reloj-y-las-latencias]] — leer un registro de contador con una instrucción.
- [[P04-Ver-los-registros-de-una-maquina-congelada]] — `info registers` en el monitor de [[QEMU]], contra Kornelia.

## Recordar #flashcards/conceptos

¿Por qué existen los registros en vez de operar directamente sobre la RAM?::Porque nombrarlos cuesta 3 o 4 bits dentro de la instrucción y usarlos cuesta cero ciclos, contra ~250 ciclos de ir a la RAM. Y porque una dirección de 64 bits no cabe en cada operando.

¿Qué es un cambio de contexto, en términos de registros?::Copiar los registros del programa que sale y poner los del que entra. El estado visible de un procesador es un puñado de números, así que eso alcanza.

¿Por qué el protocolo de Kornelia no nombra `rax`?::Porque sería x86 disfrazado de genérico (D3, P4). La máquina informa qué registros tiene y el agente los nombra como los nombra esa máquina.

¿Qué hace `cr3` / `ttbr0_el1`?::Apunta a la [[Tabla-de-paginas|tabla de páginas]] activa. Cambiarlo cambia el mundo entero de direcciones que ve el código.

## Ver también

- [[MMIO]] — el otro sentido de la palabra.
- [[Jerarquia-de-memoria]]
- [[31-La-pila-que-sobrevive]]
