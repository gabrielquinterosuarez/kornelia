---
tipo: capitulo
parte: 1
estado: pendiente
dificultad: 2
conceptos: [Registro, Jerarquia-de-memoria, Espacio-de-direcciones]
practicas: [P03-Ver-donde-se-acaban-los-registros]
---

# 02 · Registros y RAM no son lo mismo

> [!abstract] Al terminar este capítulo vas a poder
> - Decir en qué se diferencian un registro y una posición de memoria **sin nombrar la velocidad**.
> - Ver, en el desensamblado de tu propia máquina, el momento exacto en que al compilador se le acaban los registros.
> - Explicar por qué un cambio de contexto es barato, y por qué eso no es casualidad.

El capítulo anterior terminó con una frase que conviene desconfiar: *los registros son la memoria más rápida*. Es cierto y es lo de menos. Si la única diferencia fuera la velocidad, un registro sería una [[Cache|caché]] muy chiquita y muy rápida, y no lo es: **es otra cosa**.

La diferencia de fondo es esta:

> Un registro se **nombra**. Una posición de memoria se **direcciona**.

Todo lo que sigue —y buena parte de cómo está hecho un kernel— sale de esa distinción.

---

## Nombrar contra direccionar

Cuando el procesador ejecuta `add rax, rbx`, los operandos **están escritos adentro de la instrucción**. No son números que apuntan a ningún lado: son tres o cuatro bits que dicen "el segundo" y "el cuarto". El nombre está horneado en el momento de compilar, y no se puede calcular.

Cuando ejecuta `mov rax, [rbx]`, en cambio, `rbx` contiene **un número que se calculó mientras el programa corría**. Puede venir de una suma, de otra lectura, de lo que sea.

De ahí salen tres consecuencias que no tienen nada que ver con ser rápido:

**No se pueden indexar.** No existe `registro[i]`. No hay aritmética sobre nombres de registro, porque el nombre no es un valor: es parte del código. Un arreglo tiene que vivir en memoria por definición — para recorrerlo hace falta *calcular* dónde está cada elemento.

**No tienen dirección.** No se puede hacer `&rax`. Y esto no es una limitación de escritura, es lo que *significa* no ser direccionable.

**Son finitos y el número no lo elige nadie.** Cuántos hay lo fija el juego de instrucciones, porque el nombre tiene que caber adentro de la instrucción: con 4 bits de campo, dieciséis. Pasar de 16 a 32 registros no es agregar silicio, es **cambiar el formato de todas las instrucciones**, o sea inventar otra arquitectura. x86 pasó de 8 a 16 al saltar a 64 bits; ARM ya tenía 31.

---

## La segunda consecuencia, vista de verdad

Esto no hay que creerlo: se mira. Dos funciones que hacen la misma cuenta:

```c
uint64_t sin_direccion(uint64_t x) {
    uint64_t y = x * 3;
    return y + 1;
}

uint64_t con_direccion(uint64_t x) {
    uint64_t y = x * 3;
    usar(&y);              // <- lo unico que cambia
    return y + 1;
}
```

Compiladas con `-O2` en la máquina donde se escribió este capítulo:

```
0000000000000000 <sin_direccion>:
   0:	lea    0x1(%rdi,%rdi,2),%rax
   5:	ret
```

**Dos instrucciones, y ni una toca la memoria.** El `lea` hace `x*3+1` de una sola vez y deja el resultado listo para volver.

```
0000000000000010 <con_direccion>:
  10:	sub    $0x18,%rsp            <- reserva lugar en la pila
  14:	lea    (%rdi,%rdi,2),%rax
  18:	lea    0x8(%rsp),%rdi        <- calcula la direccion de y
  1d:	mov    %rax,0x8(%rsp)        <- y BAJA a memoria
  22:	call   usar
  27:	mov    0x8(%rsp),%rax        <- y vuelve a subir
  2c:	add    $0x18,%rsp
  30:	add    $0x1,%rax
  34:	ret
```

La cuenta es idéntica. Lo único que cambió es que alguien **pidió la dirección**, y con eso el valor dejó de poder vivir en un registro: hubo que reservar pila, escribirlo, y volver a leerlo después de la llamada.

> [!important] Esta es la idea del capítulo
> Un registro no es memoria rápida: **es memoria que no tiene dirección**. En cuanto algo necesita una dirección —porque se lo apunta, porque es un arreglo, porque otro lo tiene que ver— deja de poder ser un registro. No por lento: por incompatible.

Y por eso `&` en C es más caro de lo que parece. No cuesta el `&`: cuesta que la variable se muda a memoria para siempre.

---

## Dónde se acaban

Como son finitos, en algún momento no alcanzan. El lugar donde eso se ve más limpio es la **convención de llamada**: el acuerdo de por dónde pasan los argumentos de una función.

Una función de doce argumentos, en Linux x86_64:

```
0000000000000040 <doce_argumentos>:
  40:	mov    %rdx,%rax
  43:	mov    0x30(%rsp),%rdx     ┐
  48:	xor    0x28(%rsp),%rdx     │
  4d:	xor    0x20(%rsp),%rdx     │  los argumentos 7 al 12
  52:	xor    0x18(%rsp),%rdx     │  vienen de la PILA
  57:	xor    0x10(%rsp),%rdx     │
  5c:	xor    0x8(%rsp),%rdx      ┘
  61:	xor    %r9,%rdx            ┐
  64:	xor    %r8,%rdx            │
  67:	xor    %rcx,%rdx           │  los argumentos 1 al 6
  6a:	xor    %rax,%rdx           │  vienen de REGISTROS
  6d:	xor    %rsi,%rdx           │
  73:	xor    %rdi,%rax           ┘
```

Seis y seis, y el corte se ve a simple vista. La convención de Linux en x86_64 pasa los primeros seis por `rdi, rsi, rdx, rcx, r8, r9` y el resto por la pila, **porque no hay más registros que gastar**.

> [!warning] Y ese número no lo pone la arquitectura
> Este kernel corre en la misma arquitectura y su convención es otra: `x86_64-unknown-uefi` usa la de Windows, que pasa **cuatro** argumentos, por `RCX, RDX, R8, R9`, y encima exige 32 bytes de pila vacía antes de la llamada. El mismo silicio, otro acuerdo.
>
> Eso costó caro de verdad: una llamada del blob entraba a la función correcta y **veía punteros nulos** — los cuatro argumentos estaban ahí, en otros cuatro registros. Ver [[27-La-ABI-la-pone-el-target-no-el-silicio]]. Es la razón por la que Kornelia **publica** `ARGUMENTS` en vez de que el agente lo deduzca (`kernel-x86_64/src/exec.rs:365#pub const ARGUMENTS`): la máquina se describe a sí misma (P4).

---

## El derrame

Adentro de una función pasa lo mismo. Cuando hay más valores **vivos al mismo tiempo** que registros disponibles, el compilador tiene que bajar algunos a memoria y volver a subirlos cuando los necesite. Se llama **derrame** (*spill*).

"Vivo" no es "declarado": es que su valor todavía se va a usar. Veinte variables usadas de a una no derraman nada; veinte que se combinan entre sí al final, sí.

En la práctica de este capítulo hay una función con veinte valores vivos. Compilada con `-O2`, en 76 instrucciones **12 tocan la pila**:

```
  cd:	mov    %r10,-0x10(%rsp)
  da:	mov    %rbx,-0x28(%rsp)
  e3:	mov    %rcx,-0x18(%rsp)
  ...
```

Eso es el compilador quedándose sin lugar. Y da una forma concreta de leer una vieja regla de estilo: *"las funciones cortas son más rápidas"* no es estética — es que una función corta suele tener pocos valores vivos y entra entera en registros.

**La pila es, en el fondo, la extensión de los registros.** Cuando se acaban los nombres, se usan direcciones.

---

## Por qué esto le importa a un kernel

Tres cosas centrales del libro son consecuencia directa de que los registros sean pocos.

**Un cambio de contexto es barato porque el estado es chico.** Cambiar de programa es copiar unos treinta números —los registros más unos pocos de control— y poner otros treinta. Si el procesador tuviera mil registros, cambiar de tarea costaría mil escrituras y mil lecturas, y la idea entera de un sistema que multiplexa el procesador sería mucho más cara. El diseño del silicio y el diseño del kernel están atados.

**Un [[Fault|fault]] cabe en un mensaje.** Cuando el código del agente se rompe, "qué estaba pasando" se puede contar entero: los registros, más la causa. Eso es lo que hace posible P5 —los faults son datos— y no es una elección de diseño del kernel: es que el estado visible de un procesador es chico.

**Y buena parte de lo que hace un kernel es mover cosas entre niveles.** Copiar de la memoria del usuario a la del kernel, de la del kernel a un [[Aparato|aparato]], del aparato a la RAM. Tanto que el mecanismo más valioso del libro —[[DMA]]— existe para **no** copiar: que el aparato mueva los datos por su cuenta y el procesador no gaste ni un ciclo trayéndolos.

---

## En Kornelia

Acá la distinción aparece en la superficie del protocolo, y de una forma que en Linux no tiene equivalente.

Cuando el agente manda código con `exec`, **elige con qué valores arrancan los registros**. No hay una convención escondida: la máquina publica cuáles se pueden poner y con qué nombres los llama ella.

| | |
|---|---|
| Qué registros hay y cómo se llaman | `describe {what:["exec"]}` |
| Por cuáles pasan los argumentos | `ARGUMENTS`, publicado, no deducido |
| Qué vuelve cuando el código falla | Los registros, más la causa (P5) |

Y el motivo de que se publiquen es D3: **el protocolo no lleva nombres de registros horneados.** x86_64 tiene `RAX`, ARM64 tiene `X0`–`X30`, RISC-V tiene `x0`–`x31`. Un protocolo que dijera "poné esto en RAX" sería x86 disfrazado de genérico. Ver [[Registro]].

Hay un detalle que muerde y está anotado en el mapa del proyecto: **el orden de esa lista es el orden en que el ensamblador deja los valores.** Cambiar uno sin el otro hace que el kernel informe un registro con el nombre de otro — ya pasó, en aarch64. Un dato correcto con la etiqueta equivocada es peor que un error.

---

## Las prácticas de este capítulo

- [[P03-Ver-donde-se-acaban-los-registros]] — *(construir)* compilar las tres funciones de arriba en tu máquina y encontrar en el desensamblado el momento exacto del derrame.

---

## Recordar #flashcards/parte-01

¿Cuál es la diferencia de fondo entre un registro y una posición de memoria?::Un registro se **nombra** —el nombre está escrito adentro de la instrucción, en 3 o 4 bits— y una posición de memoria se **direcciona** con un número calculado en tiempo de ejecución. La velocidad es una consecuencia, no la diferencia.

¿Por qué no existe un arreglo de registros?::Porque indexar es aritmética sobre una dirección, y un registro no tiene dirección: su nombre es parte del código, no un valor que se pueda calcular.

¿Qué le pasa a una variable de C cuando le tomás la dirección con `&`?::Deja de poder vivir en un registro y baja a memoria. El costo no es el `&`: es que la variable se muda a la pila y cada uso pasa a ser una lectura o escritura.

¿Por qué el juego de instrucciones fija cuántos registros hay?::Porque el nombre del registro tiene que caber adentro de la instrucción. Agregar registros cambia el formato de todas las instrucciones: es otra arquitectura, no más silicio.

¿Qué es un derrame (*spill*)?::Cuando hay más valores vivos al mismo tiempo que registros, el compilador baja algunos a la pila y los vuelve a subir. La pila es, en el fondo, la extensión de los registros.

¿Por qué es barato un cambio de contexto?::Porque el estado visible del procesador son unos treinta números. Con mil registros, multiplexar el procesador entre tareas costaría mucho más — el diseño del silicio y el del kernel están atados.

En x86_64 sobre Linux, ¿cuántos argumentos van por registro?::Seis (`rdi, rsi, rdx, rcx, r8, r9`); del séptimo en adelante, por la pila. Pero ese número lo pone el **target**, no la arquitectura: el mismo silicio bajo [[UEFI]] usa la convención de Windows, con cuatro.

## Qué sigue

[[03-Que-hace-realmente-una-instruccion]] — y si preferís seguir por conceptos en vez de por capítulos, de acá salen [[Registro]], [[Jerarquia-de-memoria]] y [[Espacio-de-direcciones]].
