---
tipo: concepto
estado: pendiente
dificultad: 3
principios: [P3, P4]
decisiones: [D3, D18, D24, D27]
practicas: [P03-Ver-donde-se-acaban-los-registros]
capitulos: [27-La-ABI-la-pone-el-target-no-el-silicio, 02-Registros-y-RAM-no-son-lo-mismo, 13-Cargar-una-imagen-PE-ELF-y-el-entry-point]
---

# ABI

> *Application Binary Interface*. El acuerdo de **cómo** se hablan dos pedazos de código que se compilaron por separado: por dónde van los argumentos, quién guarda qué, cómo vuelve el resultado.

La API dice **qué** se llama: `int abrir(const char *nombre)`. La ABI dice **cómo**: que `nombre` viaja en `rdi`, que el resultado vuelve en `eax`, que la pila tiene que estar alineada a 16 bytes en el momento del `call`, y que el que llama puede dar por perdido lo que hubiera en `rcx`. La API se escribe en un archivo de cabecera y la lee un humano. La ABI no se escribe en ninguna parte del programa: está **horneada en los bytes** que emitió el compilador.

## Qué problema resuelve

Dos funciones que van a llamarse **casi nunca se compilan juntas**. Tu programa llama a `printf`, que se compiló hace ocho años, con otro compilador, por gente que no sabía que ibas a existir. El kernel salta al código que le subió el agente, que se compiló en otra máquina hace treinta segundos. Un módulo se enlaza contra un kernel que ya estaba corriendo.

En todos esos casos hay un `call` a una dirección y nada más. No hay negociación, no hay descubrimiento, no hay una capa que traduzca. El que llama pone los valores en algún lado y salta; el llamado los busca en algún lado. **Si los dos "algún lado" no son el mismo, no hay error: hay basura.** Nadie comprueba nada, porque comprobarlo costaría tiempo en cada llamada y a esa altura ya no hay tipos, solo [[Registro|registros]] y pila.

Así que hace falta un **acuerdo escrito de antemano y respetado por todos los compiladores** de una plataforma. Eso es la ABI. No es una biblioteca ni un mecanismo: es un documento que el compilador obedece, y el precio de desobedecerlo es un programa que corre y da cualquier cosa.

## Qué incluye una ABI

Es más que "por dónde van los argumentos", y las piezas que se olvidan son las que muerden:

| Pieza | Qué fija | Qué pasa si no coincide |
|---|---|---|
| **Convención de llamada** | Cuántos argumentos van por registro, cuáles, y en qué orden se apilan los que sobran. | Los argumentos llegan cambiados de lugar, o nulos, o basura. |
| **Registros preservados** | Cuáles el llamado tiene que devolver como los encontró (*callee-saved*) y cuáles puede pisar libremente (*caller-saved*). | Una variable cambia sola después de una llamada. |
| **Valor de retorno** | Por qué registro vuelve, y qué se hace con un struct que no entra en uno. | El resultado es basura, o se corrompe memoria del que llamó. |
| **Alineación de la pila** | Cuánto tiene que estar alineado `rsp` en el momento exacto del `call` — 16 bytes en casi todas las ABI modernas. | Cuelga en la primera instrucción SIMD que use un operando de memoria. |
| **Representación de tipos** | Tamaño y alineación de cada tipo, y por lo tanto el **padding** de cada struct. | Los dos lados leen campos distintos del mismo struct. |
| **Name mangling** | Cómo se convierte un nombre del lenguaje en un símbolo del objeto. | No enlaza — que es, de toda la lista, el único caso feliz. |

De todas, **la alineación de la pila es la que más sorprende**, porque parece burocracia. No lo es: existe porque los registros anchos (SSE, AVX, NEON) tienen instrucciones que exigen que la dirección esté alineada, y el compilador las usa sin avisar — para copiar un struct, por ejemplo. Si la pila entró desalineada, la excepción aparece en una instrucción que vos no escribiste.

Y el **name mangling** es la única pieza que se comprueba sola. C no manglea: `abrir` es el símbolo `abrir`, y por eso C es el idioma franco entre lenguajes. C++ y Rust sí, porque necesitan meter los tipos y el módulo en el nombre para que dos funciones distintas con el mismo nombre no choquen. `#[no_mangle]` en Rust y `extern "C"` en C++ apagan el mangling; **apagar el mangling no cambia la convención de llamada**, que es una confusión frecuente y cara: son dos piezas de la misma ABI y se eligen por separado.

## Los números concretos

Tres ABI, dos de ellas para el **mismo silicio**:

| | System V AMD64 | Microsoft x64 | AAPCS64 (aarch64) |
|---|---|---|---|
| Dónde se usa | Linux, BSD, macOS en x86_64 | Windows y [[UEFI]] en x86_64 | Todo aarch64 |
| Argumentos enteros por registro | **6**: `RDI RSI RDX RCX R8 R9` | **4**: `RCX RDX R8 R9` | **8**: `X0`–`X7` |
| Argumentos flotantes | 8: `XMM0`–`XMM7`, aparte de los enteros | Los mismos 4 slots, compartidos con los enteros | 8: `V0`–`V7` |
| Retorno | `RAX` (y `RDX` para 128 bits) | `RAX` | `X0` (y `X1`) |
| Los que sobran | Por la pila, en orden inverso | Por la pila | Por la pila |
| Espacio extra en la pila | La *red zone*: 128 bytes **debajo** de `rsp` que una función hoja usa sin reservar | El *shadow space*: **32 bytes** que el que llama reserva **arriba**, siempre | Nada de eso |
| Alineación de pila | 16 bytes en el `call` | 16 bytes | 16 bytes |
| Preservados por el llamado | `RBX RBP R12`–`R15` | `RBX RBP RDI RSI R12`–`R15` y varios XMM | `X19`–`X28`, `SP`, y la mitad baja de `V8`–`V15` |

Mirá las dos primeras columnas: **el mismo procesador, y ni un solo renglón igual**. `RDI` y `RSI` pasan de ser los dos primeros argumentos a ser registros que el llamado tiene obligación de preservar. Y las dos zonas de pila son opuestas: System V regala 128 bytes debajo del puntero que nadie va a pisar, Microsoft exige 32 bytes arriba que el que llama reserva y no usa —son para que el llamado tenga dónde volcar los cuatro argumentos que le llegaron por registro si necesita sus direcciones.

La *red zone*, de paso, **no existe adentro de un kernel**: se compila con `-mno-red-zone` porque una [[Interrupcion|interrupción]] puede llegar en cualquier momento y apilar su marco justo ahí.

## La idea central: la ABI la pone el *target*, no la arquitectura

Es lo único que hay que llevarse de esta nota, y la tabla de arriba ya lo dice sin decirlo: **saber qué procesador tenés no alcanza para saber por dónde pasan los argumentos.** La convención la fija el *target* —la combinación de arquitectura, sistema y formato de ejecutable para la que compilaste—, y en el mismo x86_64 hay por lo menos dos convenciones vivas y contradictorias.

Suena a trivia hasta que te la comés.

> [!danger] El bug que costó caro en este proyecto
> El kernel de x86_64 se compila para el target **`x86_64-unknown-uefi`**, porque [[UEFI]] carga ejecutables [[ELF-y-PE|PE]] y ese es el target que los produce. Y ese target **no trae solo el formato: trae la ABI de Windows.** Ahí `extern "C"` pasa los argumentos por `RCX, RDX, R8, R9` —no `RDI`/`RSI`— y exige los 32 bytes de *shadow space* antes de la llamada.
>
> El síntoma no se parece a la causa: una llamada del blob entraba **a la función correcta**, con la dirección correcta, y adentro veía **punteros nulos**. Los cuatro argumentos estaban ahí, enteros, en otros cuatro registros. No hubo fault, no hubo símbolo sin resolver, no hubo nada que apuntara a la ABI — porque la ABI no participa en tiempo de ejecución: ya ocurrió, cuando se compilaron los dos lados con documentos distintos.

La moraleja quedó anotada en `CLAUDE.md`, sección *Cosas que ya costaron caras*: **la convención la pone el target, no el silicio.**

## Cómo lo hace Linux

Linux vive con **dos ABI distintas al mismo tiempo**, y la distinción es exactamente el tema de esta nota.

#### La de las funciones

System V AMD64, la primera columna de la tabla. Es la que usa todo lo que se llama con `call`: tu programa, la libc, el código adentro del kernel.

#### La de las [[Syscall|syscalls]]

**No es la misma**, y la diferencia es de un solo registro:

| | Número | Argumentos | Vuelve en |
|---|---|---|---|
| Función (System V) | — | `RDI RSI RDX **RCX** R8 R9` | `RAX` |
| Syscall Linux x86_64 | `RAX` | `RDI RSI RDX **R10** R8 R9` | `RAX` |

El cuarto argumento se corrió a `R10` porque **la instrucción `syscall` pisa `RCX`**: el silicio guarda ahí la dirección de retorno, y las banderas en `R11`. O sea que acá la convención no la eligió nadie por gusto: la impuso el hardware, y la ABI tuvo que acomodarse. Es el mejor ejemplo de por qué una convención de llamada **no se deduce razonando**, se busca en el documento.

#### Mirarlo

```bash
objdump -d /bin/ls | less              # el desensamblado: los argumentos, a la vista
readelf -h /bin/ls                     # cabecera: arquitectura, tipo, y el campo ABI
readelf -hA /bin/ls                    # en ARM, -A trae los atributos de ABI
gcc -S -O2 -o - archivo.c              # el ensamblador antes de ensamblar, con nombres
```

En `objdump -d` la convención se lee sola: antes de cada `call` hay una fila de `mov` a `%rdi`, `%rsi`, `%rdx`… y cuando se acaban los seis, empiezan los `mov` a `(%rsp)`. Ese corte es lo que se cuenta a mano en [[P03-Ver-donde-se-acaban-los-registros]].

#### Y las dos promesas opuestas del kernel

Esto es lo que más confunde de Linux, y en realidad es simple: **el kernel tiene una ABI estable y una inestable, y son distintas a propósito.**

| | Hacia el espacio de usuario | Adentro del kernel |
|---|---|---|
| ¿Es estable? | **Sí, para siempre.** *"We do not break userspace."* | **No, ni entre dos versiones menores.** |
| Qué cubre | Números y firmas de syscalls, estructuras que cruzan la frontera, `/proc`, `/sys` | Firmas de funciones internas, layout de structs como `task_struct`, símbolos exportados |
| Consecuencia | Un binario de 1995 corre hoy sin recompilar | Un [[Modulo-de-kernel|módulo]] se compila **contra el kernel exacto** con el que va a correr |

La segunda fila es la razón de que `insmod` conteste `Invalid module format` cuando el módulo no coincide: el kernel guarda en cada módulo un `vermagic` —versión, opciones de compilación, flags de ABI— y lo compara al cargar. Y lo compara porque **no hay forma de detectar en tiempo de ejecución** que un struct cambió de tamaño: se leería el campo equivocado y seguiría andando hasta corromper algo. Es un `vermagic` en vez de un chequeo real por la misma razón que un `call` no comprueba dónde están los argumentos.

Que la ABI interna **no** sea estable es una decisión, no un descuido: mantenerla congelaría el diseño del kernel alrededor de decisiones viejas. El precio lo pagan los módulos fuera del árbol, y esa es la idea.

## Cómo lo hace Kornelia

| | |
|---|---|
| **Decisiones** | D3 (nada específico de arquitectura horneado en el protocolo), D24 (la frontera son dos ejes: arquitectura **y** entorno de arranque), D18 (el blob), D27 (`supervised` y `raw`) |
| **Principio** | **P4 — la máquina se describe a sí misma.** El agente no deduce la ABI: la pide. |
| **Dónde vive** | `kernel-core/src/platform.rs:129#const ARGUMENTS: &'static [usize];`, `kernel-x86_64/src/exec.rs:365#pub const ARGUMENTS: &[usize] = &[2, 3]`, `kernel-aarch64/src/exec.rs:356#pub const ARGUMENTS: &[usize] = &[0, 1]` |

Acá el problema es más agudo que en un sistema normal, porque el código que corre lo **compiló otra máquina**: el agente es un compilador externo (P3) que emite bytes y los sube. Cuando el kernel salta a esos bytes le tiene que pasar algo —al menos la dirección donde los cargó—, y ahí hay una ABI aunque nadie la haya nombrado.

La respuesta del proyecto es la de siempre: **publicarla en vez de que se deduzca.** `describe {what:["exec"]}` devuelve el campo `arguments` con **los nombres de los registros de esta máquina**, en orden (`kernel-core/src/protocol.rs:776#for i in P::ARGUMENTS`). Son nombres y no índices porque un nombre es lo que el agente puede pedir (D3): el protocolo no dice `RCX` en ninguna parte de su definición, lo dice la máquina cuando se le pregunta.

Y mirá los dos valores de la tabla de arriba: `&[2, 3]` en x86_64 y `&[0, 1]` en aarch64. Son índices sobre la lista `REGISTERS` que cada arquitectura publica, y **en x86_64 no son los dos primeros**. Ese `2, 3` es el bug fosilizado: apunta a `rcx` y `rdx` porque el target es UEFI. Si el kernel hubiera hecho la deducción "estamos en x86_64, entonces RDI/RSI", ese constante diría `&[5, 4]` y todo se rompería en silencio.

#### Dentro del blob se ve la costura

El blob (D18) es código del agente compilado para **bare-metal**, no para UEFI, y tiene que llamar a un kernel compilado para UEFI. O sea: dos ABI distintas tocándose. El proyecto lo resuelve nombrando la convención a mano, y el comentario del código dice justo lo de esta nota:

- La entrada del blob en x86_64 declara `extern "win64"` y no `extern "C"` (`blob/src/main.rs:90#pub extern "win64" fn blob_entry`), porque el kernel le deja la dirección de carga en `rcx`; un `extern "C"` de bare-metal la buscaría en `rdi` y leería basura.
- En aarch64 no hace falta nada de eso: hay **una sola** ABI de C y los argumentos ya van en `X0`–`X7` (`blob/src/main.rs:98#pub extern "C" fn blob_entry`). Todo este párrafo es un problema de x86_64.
- Y al pedirle un verbo al kernel por la ventanilla, los cuatro argumentos se ponen en registros **a mano**, con `asm!`, en vez de dejar que el compilador los acomode (`blob/src/gate.rs:30#pub unsafe fn service`). No es paranoia: es que el compilador del blob acomoda según *su* target, y el que va a leerlos es de otro.

#### Qué se quitó

En un sistema normal la ABI es un documento que hay que ir a buscar afuera —el *System V ABI Supplement*, el *AAPCS64*— y que el programa da por sabido. Acá **no hay documento que buscar: la máquina lo contesta.** Es la misma quita que con los bytes de la ventanilla, los registros iniciales o el tamaño de página: cada cosa que un sistema normal deja implícita se convierte acá en un campo de `describe`, porque un agente que asume algo sobre la máquina rompe P4.

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| La llamada entra a **la función correcta** y adentro los punteros son nulos o basura. | Convención distinta entre los dos lados. Los argumentos están ahí, en otros registros. El caso canónico: `extern "C"` compilado para bare-metal llamando a algo compilado para UEFI. |
| Solo el **quinto argumento en adelante** llega mal. | Los primeros entran por registro y coinciden; los que sobran van por pila, y ahí discrepan el orden, el padding o el *shadow space*. |
| Cuelga en una instrucción SIMD que vos no escribiste (`movaps`, `ld1`). | La pila desalineada. El compilador usó un registro ancho para copiar un struct y esa instrucción exige alineación de 16 bytes. |
| Una variable local cambia sola **después** de una llamada. | El llamado pisó un registro *callee-saved* sin restaurarlo. Típico de ensamblador escrito a mano. |
| Los dos lados leen campos distintos del mismo struct. | Padding: distinta alineación de tipos entre los dos compiladores, o un `#pragma pack` de un solo lado. |
| Se corrompe memoria al llamar a algo del [[Firmware|firmware]] desde UEFI. | Faltan los 32 bytes de *shadow space* que la ABI de Windows exige reservar antes del `call`. |
| No enlaza: símbolo no encontrado, con el nombre lleno de letras raras. | *Name mangling*. Falta un `extern "C"` o un `#[no_mangle]`. Es el único síntoma de ABI que se detecta antes de correr. |
| `insmod: Invalid module format`. | ABI interna del kernel: el módulo se compiló contra otra versión. `modinfo` muestra el `vermagic` esperado. Ver [[Modulo-de-kernel]]. |

La fila de arriba es la que hay que memorizar, porque **el síntoma no se parece a la causa**. Un puntero nulo hace pensar en quién lo pasó, no en dónde lo puso. Y no hay error, no hay fault, no hay símbolo faltante: los dos lados están perfectamente bien, cada uno según su documento. Ver [[Indice-de-sintomas]].

## Práctica

- [[P03-Ver-donde-se-acaban-los-registros]] — *(construir)* la parte 2 es exactamente esta nota: en el desensamblado de una función de doce argumentos se ve el corte donde System V se queda sin registros a los **seis** y empieza a leer de la pila. Y el recuadro compara ese seis contra el cuatro de Kornelia, en el mismo silicio.
- Contra Kornelia: `./scripts/client.py --what exec` y mirar el campo `arguments`. Comparalo entre `./scripts/run-x86_64.sh` y `./scripts/run-aarch64.sh` — la misma pregunta, dos respuestas, y ninguna horneada en el cliente.

## Recordar #flashcards/conceptos

¿Cuál es la diferencia entre API y ABI?::La API dice **qué** se llama —nombres, tipos, firmas— y la lee un humano en un archivo de cabecera. La ABI dice **cómo** —por qué registro va cada argumento, quién preserva qué, cómo vuelve el valor— y está horneada en los bytes que emitió el compilador.

¿Qué cosas fija una ABI, además de por dónde van los argumentos?::Qué registros preserva el llamado y cuáles puede pisar, por dónde vuelve el valor, cuánto tiene que estar alineada la pila en el `call`, el tamaño y padding de cada tipo, y el name mangling.

¿Quién decide la convención de llamada: la arquitectura o el target?::El **target**. En el mismo x86_64, System V pasa 6 argumentos por RDI RSI RDX RCX R8 R9, y Microsoft x64 —que es lo que usa UEFI— pasa 4 por RCX RDX R8 R9 más 32 bytes de shadow space.

¿Cuántos argumentos van por registro en System V x86_64, Microsoft x64 y AAPCS64?::Seis (RDI RSI RDX RCX R8 R9), cuatro (RCX RDX R8 R9) y ocho (X0–X7).

¿Qué es el shadow space y qué es la red zone?::Dos zonas de pila opuestas. El shadow space son 32 bytes que en Microsoft x64 el que llama reserva **arriba** de la pila, siempre, para que el llamado pueda volcar ahí sus cuatro argumentos de registro. La red zone son 128 bytes **debajo** de rsp que System V le regala a una función hoja para usar sin reservar — y que en un kernel se desactiva, porque una interrupción apilaría su marco justo ahí.

¿Por qué la ABI de las syscalls de Linux usa R10 y no RCX para el cuarto argumento?::Porque la instrucción `syscall` pisa RCX con la dirección de retorno y R11 con las banderas. El hardware impuso la convención y la ABI tuvo que correr el argumento.

Una llamada entra a la función correcta y adentro ve punteros nulos. ¿Qué pasó?::Los dos lados se compilaron con ABI distintas. Los argumentos están ahí, enteros, en otros registros. Es el bug que costó caro en Kornelia: el target es `x86_64-unknown-uefi`, así que `extern "C"` es la convención de Windows.

¿Por qué `ARGUMENTS` se publica en `describe exec` en vez de deducirse?::Porque no se puede deducir de la arquitectura: la pone el target (P4, D3). En x86_64 vale `&[2, 3]` —rcx y rdx— y no los dos primeros registros, justamente porque el kernel se compila para UEFI.

¿Es estable la ABI del kernel de Linux?::Depende de cuál. Hacia el espacio de usuario sí, para siempre ("no rompemos el espacio de usuario"): un binario viejo corre hoy. La interna **no**, ni entre versiones menores, y por eso un módulo se compila contra el kernel exacto y lleva un `vermagic` que se compara al cargar.

¿Por qué `#[no_mangle]` no alcanza para que dos lados se llamen bien?::Porque el mangling y la convención de llamada son piezas distintas de la ABI. Apagar el mangling hace que el símbolo se encuentre; no cambia por dónde viajan los argumentos.

## Ver también

- [[Registro]] — dónde viven los argumentos, y por qué son pocos.
- [[Syscall]] — la otra ABI de Linux, la que usa R10.
- [[ELF-y-PE]] — el formato del ejecutable, que en UEFI viene con la ABI pegada.
- [[Modulo-de-kernel]] — el caso donde la ABI inestable se hace visible todos los días.
- [[UEFI]] · [[Indice-de-sintomas]]
- [[27-La-ABI-la-pone-el-target-no-el-silicio]] — el capítulo que cuenta el bug entero.
