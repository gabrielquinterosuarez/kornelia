---
tipo: glosario
estado: vivo
---

# Índice de síntomas

**Del síntoma a la causa**, que es la dirección en la que uno depura y la contraria a la que enseñan los libros.

Casi todo lo de acá pasó de verdad —en este proyecto o en cualquier kernel— y el punto central es este: **el síntoma no se parece a la causa**. Un `mem.write` de 4 KiB que cuelga la máquina no dice "el buffer del cable es más chico que el pedido". Nadie lo deduce; se busca en una lista como esta.

Cuando encuentres uno nuevo, agregalo. Esta nota crece con la experiencia, no con la lectura.

---

## Nada sale por ningún lado

### La máquina no imprime una sola letra

| Causa | Cómo se confirma |
|---|---|
| El cable no está donde el kernel cree. | La dirección del UART está horneada para el primer byte, pero cambia según la máquina y el firmware. Kornelia arranca con una horneada y se muda a la que dice la tabla SPCR de ACPI (P4). Ver [[15-Enumerar-sin-adivinar-ACPI]]. |
| QEMU se come los bytes. | `-serial mon:stdio` usa `0x01` como escape y por ahí viaja CBOR. **Tiene que ser `-serial stdio`** (D26). |
| El kernel se murió antes del primer `write_byte`. | No hay forma de saberlo desde afuera: hay que poner la letra más temprano posible y ver si sale. |
| Bucle de faults. | El handler de la excepción provoca la misma excepción. No alcanza a avisar nunca. Ver abajo. |

### La máquina imprimía y se quedó muda de golpe

| Causa | Cómo se confirma |
|---|---|
| Bucle de faults en el handler. | Si el handler de excepciones toca algo que vuelve a fallar, no sale nada. Caso real: **un núcleo arrancado por PSCI viene con los registros SIMD atrapados** (`CPACR_EL1` en cero), el compilador usa registros anchos para copiar structs, la primera copia es una excepción, y el handler la repite al copiar la suya. Silencio total. Se arregla en el trampolín, antes de saltar a Rust. |
| Un acceso que el bus rechazó. | En aarch64, leer un registro de aparato con el **ancho equivocado** provoca un abort externo que dejaba la máquina muda. En x86 la misma lectura devuelve **ceros en silencio**. El mismo pedido: en una arquitectura miente, en la otra mata. Ver [[45-Un-registro-no-es-RAM]] y [[33-Recuperar-un-acceso-que-el-bus-rechaza]]. |
| La pila se destruyó. | Si la excepción se atiende en la misma pila que se rompió, no se puede atender. Por eso hay una pila aparte: IST en x86_64, `SP_EL1` en aarch64. Ver [[31-La-pila-que-sobrevive]]. |
| El código enmascaró las interrupciones y no volvió. | `cli` / `msr daifset`. Sin un segundo escalón (NMI) no hay forma de recuperarlo. Ver [[39-Plazos-y-cortes]]. |

### Se cuelga esperando algo que ya no viene

| Causa | Cómo se confirma |
|---|---|
| Un pedido llegó incompleto y el kernel espera el resto. | **Caso real, y el síntoma no se parece:** el buffer circular donde el handler del serie dejaba los bytes tenía 4 KiB, y el protocolo acepta pedidos de 64 KiB. Un `mem.write` de 4 KiB colgaba la máquina — el CBOR quedaba trunco y el kernel esperaba para siempre el resto de un mensaje que ya se había perdido. Y el contador de bytes perdidos **nadie lo podía ver**. Ahora se publica en `describe {what:["cable"]}` y el portón exige que sea cero. |
| Se espera con los timbres cerrados. | Si el kernel espera a otro núcleo con las interrupciones deshabilitadas, no atiende el cable ni corre handlers en todo ese rato. Estuvo así desde que `exec` acepta `core` y **no lo encontró nadie mirando**: apareció cuando una prueba nueva obligó a recorrer ese camino. |

> [!tip] Regla que sale de los dos casos de arriba
> **Un contador de errores que nadie puede leer no existe.** Si el kernel descarta algo, lo tiene que poder decir. Y **un camino que ninguna prueba recorre no está andando: está sin probar.**

---

## Anda, pero anda mal

### La prueba pasa y sospecho que no prueba nada

| Causa | Cómo se confirma |
|---|---|
| "Bloqueado" y "nunca pasó nada" se ven **idénticos** desde afuera. | Antes de creerle a un bloqueo, comprobá que **la cosa bloqueada ocurre**: corré lo mismo sin el bloqueo y mirá. **Caso real:** el aparato `edu` recorta la dirección de DMA a 28 bits si no se le dice otra cosa, y en aarch64 la RAM arranca en 1 GiB — ningún destino podía llegar nunca. El IOMMU parecía estar bloqueando perfectamente. En x86 no se veía porque la RAM arranca en cero. |
| El sistema se está probando a sí mismo. | Si la prueba solo comprueba lo que el kernel *dice* de sí mismo, no prueba nada. La versión buena: que **el código del agente** informe en qué núcleo está; que ejecute `cli` y el fault vuelva; que un aparato de verdad intente escribir y la memoria quede intacta. Ver [[58-Una-prueba-que-no-puede-pasar-por-accidente]]. |

### El IOMMU deja pasar todo

| Causa | Cómo se confirma |
|---|---|
| Se le escribió un registro de estado como si fuera una lista de botones. | **`GCMD` de VT-d no es una lista de botones: es el estado entero.** El silicio compara lo que se le escribe contra lo que había. Pedirle "tomate la tabla raíz" sin arrastrar el estado **apaga la traducción**, y el síntoma es que todo pasa: se ve como si anduviera. |

### El aparato lee ceros donde escribimos datos

| Causa | Cómo se confirma |
|---|---|
| Una alineación mayor que la página. | `#[repr(align(8192))]` queda alineado adentro de la imagen, pero UEFI la carga a 4 KiB y ahí se pierde. Lo caro no es la tabla desalineada: el compilador, **dando por cierto que los bits de abajo son cero, simplifica las máscaras** con las que se arma la dirección. El síntoma fue un SMMU leyendo ceros una página más abajo. Con más de 4 KiB: pedir de más y alinear a mano en runtime. Ver [[24-Alineacion-la-promesa-que-el-cargador-no-cumple]]. |
| Escrituras que no salieron de la caché o del buffer. | Falta una barrera, o la memoria no está marcada no-cacheable. En x86 casi nunca se nota; en ARM sí. |

### Un registro de aparato devuelve ceros

| Causa | Cómo se confirma |
|---|---|
| Se leyó con el ancho equivocado. | Muchos registros solo aceptan accesos de su ancho exacto y **descartan los más angostos sin avisar**. Por eso `mem.read`/`mem.write` toman `width`. |
| El rango no está mapeado, o está mapeado como cacheable. | En aarch64 los BARs de PCIe caen en 512 GiB y el mapa del firmware llega a 257: el aparato era literalmente inalcanzable. Ver [[18-Lo-que-la-maquina-no-dice]]. |
| La ventana de configuración de PCIe no está en el mapa. | UEFI no la informa en aarch64; la tabla MCFG de ACPI sí. Publicar una dirección que uno mismo hace inalcanzable es lo que prohíbe P1. |

### La interrupción no llega nunca

| Causa | Cómo se confirma |
|---|---|
| Llega como pulso y el controlador la trata como nivel. | El frame que convierte una escritura en interrupción (MSI) **no sostiene la línea**: la sube y la baja. Por omisión el GIC trata las de aparato como sensibles a nivel, así que el pulso se perdía. Hay que configurarla **por flanco** en `GICD_ICFGR`. Se encontró **separando las dos mitades**: hacerla sonar a mano con lo que el kernel publica, sin aparato. |
| Está pendiente pero no se puede reconocer. | En el GICv2 de QEMU, una interrupción del Grupo 1 queda pendiente y `GICC_IAR` devuelve **1022**: "es del Grupo 1, reconocela por `GICC_AIAR`" — y `GICC_AIAR` lee cero, porque ese registro existe solo con extensiones de seguridad que este GIC no tiene. El GIC manda a una puerta que no está construida. |
| El grupo no está habilitado. | El error tonto que conviene no repetir: mover interrupciones al Grupo 1 **sin habilitar el Grupo 1**. Con eso nada se entrega, y parece que el problema es otro. |
| El IOMMU bloqueó la escritura del MSI. | Un MSI **es** un DMA: una escritura del aparato a memoria. Si no está declarada, no llega. |

---

## Bugs que se mueven

### Falla una vez cada tanto y no reproduce

| Causa | Cómo se confirma |
|---|---|
| Estado compartido que alguien toca sin candado. | **La lección del caso real no es sobre candados: es sobre cómo se audita.** "Se auditó lo único que puede causarlo" es una afirmación sobre lo que uno se acordó de mirar. La auditoría revisó los tests que tocan las tablas de reclamos —esos sí tomaban el candado— y nadie miró los que leen la descripción de la máquina, que pisan **otros** estáticos. Apareció recorriendo la **lista de estáticos del crate**, no la lista de sospechosos. |
| La suite entera esconde la carrera. | **Para reproducirlo hubo que correr menos, no más.** Con toda la suite no salía ni en 150 corridas; con los tres tests que comparten esos estáticos y 16 hilos, dos de cada cuatrocientas. |

### Desaparece cuando lo instrumento

| Causa | Cómo se confirma |
|---|---|
| La instrumentación cambia los tiempos. | Escribir una letra por el cable **desde un handler del reloj** movió el timing lo suficiente para que el `exec` dejara de volver. Para algo que depende de tiempos: dejar el dato en un estático y publicarlo por `describe`. |
| La instrumentación rompe el protocolo. | Las letras salen **después** del marcador, así que el cliente se las come como CBOR y el error se ve en otro lado. |

### Anda en x86 y no en ARM (o al revés)

| Causa | Cómo se confirma |
|---|---|
| Falta una barrera de memoria. | x86 tiene modelo de memoria fuerte y **esconde barreras faltantes que ARM expone**. Es la razón de fondo por la que este proyecto compila las dos desde el primer commit (D22/D23), y no es solo portabilidad. Ver [[42-Ordenamiento-de-memoria]]. |
| La convención de llamada no es la que creías. | **La ABI de C de este kernel en x86_64 no es la de Linux: es la de Windows**, porque el target es `x86_64-unknown-uefi`. Ahí `extern "C"` pasa argumentos por **RCX, RDX, R8, R9** —no RDI/RSI— y exige 32 bytes de pila vacía antes de la llamada. El síntoma: una llamada que entra a la función correcta y **ve punteros nulos**; los cuatro argumentos estaban ahí, en otros cuatro registros. Ver [[27-La-ABI-la-pone-el-target-no-el-silicio]]. |
| Un límite que sobra es tan inválido como uno que falta. | El tamaño de entrada de la etapa 2 del SMMU **no puede ser menor** que el de salida. Pedir 39 bits donde la máquina tiene 44 no se rechaza: se reinterpreta, y las direcciones se recortan en silencio. Al silicio hay que pedirle configuraciones que **pueda** hacer, no las que le sobren. |

### Anda solo, se rompe con otro cambio

| Causa | Cómo se confirma |
|---|---|
| Un offset que el ensamblador tiene escrito a mano. | Los offsets del bloque por núcleo (`percpu.rs`) los usa ensamblador hecho a mano; agregar un campo los corre. Hay `assert!` de tiempo de compilación, y existen por esto. |
| Un orden que dos lugares tienen que compartir. | El orden de `REGISTERS` es el orden en que el ensamblador deja los valores. Cambiar uno sin el otro hace que el kernel **informe un registro con el nombre de otro**. Ya pasó, en aarch64. |

---

## Del lado de Linux

Los mensajes que vas a ver en `dmesg` y qué significan de verdad.

| Mensaje | Qué pasó | Se sigue viviendo |
|---|---|---|
| `BUG: unable to handle kernel paging request at ...` | El kernel usó un puntero inválido. El equivalente de un segfault, pero del kernel. | A veces: mata la tarea. |
| `Oops: 0000 [#1] SMP` | El kernel se dio cuenta de que algo está mal y no sigue con **esa** tarea. | Sí, la máquina suele seguir — inestable. |
| `Kernel panic - not syncing` | No hay forma de seguir. Se detiene todo. | No. |
| `soft lockup - CPU#N stuck for 22s` | Una tarea se quedó girando en el kernel sin ceder, con las interrupciones **habilitadas**. | Sí, se detecta y se avisa. |
| `hard LOCKUP` | Lo mismo pero con las interrupciones **deshabilitadas**: solo lo detecta el NMI. Es exactamente el problema del [[39-Plazos-y-cortes|segundo escalón]]. | No. |
| `INFO: task X blocked for more than 120 seconds` | Una tarea espera algo que no llega (casi siempre I/O). | Sí. |
| `DMAR: DRHD: handling fault status reg`, `SMMU: ... C_BAD_STE` | El IOMMU **bloqueó** un DMA y lo anotó. Cuando probás que el bloqueo funciona, esto es lo que querés ver. | Sí. |
| `Call Trace:` seguido de nombres | El camino de funciones hasta el problema. Se lee **de abajo hacia arriba**. | — |

Práctica para verlos sin sufrirlos: [[P02-Provocar-y-leer-un-oops-en-una-VM]].

---

## Cómo se depura un núcleo que se quedó mudo

No hay debugger. Se marca el camino con letras por el cable y se lee la traza:

```rust
p.uart_write_byte(b'A');
```

Así apareció lo de `CPACR_EL1`: la traza `1ST234KJ2Da2` y **ninguna `b`** dijo que los dos núcleos hacían su parte y que el reclamado moría entre terminar el trabajo y guardar la respuesta. Las letras se sacan antes de commitear.

Cuando el problema depende de tiempos, la letra **cambia el fenómeno** (ver arriba): ahí el dato va a un estático y se publica por `describe`.

Y la técnica que resolvió más de un caso de esta página: **separar las dos mitades**. Si el aparato escribe y la interrupción no llega, hacé sonar la interrupción a mano sin aparato. Una de las dos mitades anda, y ya sabés cuál.

---

## Recordar #flashcards/sintomas

Un `mem.write` de 4 KiB cuelga la máquina. ¿Primera sospecha?::Que el pedido llegó incompleto y el kernel espera el resto: el buffer donde se acumulan los bytes es más chico que el pedido más grande que el protocolo acepta. El síntoma no se parece a la causa.

Antes de creerle a un IOMMU que bloquea, ¿qué hay que comprobar?::Que la cosa bloqueada **ocurre** sin el bloqueo. "Bloqueado" y "nunca pasó nada" se ven idénticos desde afuera.

Un bug de concurrencia no reproduce en 150 corridas de la suite. ¿Qué se prueba?::Correr **menos**: solo los tests que comparten el mismo estado, con muchos hilos. La suite entera esconde la carrera porque cambia los tiempos.

Una llamada entra a la función correcta y ve punteros nulos. ¿Qué mirar?::La convención de llamada del **target**, no de la arquitectura. En `x86_64-unknown-uefi` los argumentos van por RCX/RDX/R8/R9, no RDI/RSI.

¿Por qué instrumentar con letras por el cable puede ser contraproducente?::Porque cambia los tiempos y puede hacer desaparecer (o aparecer) el bug, y porque las letras salen después del marcador del protocolo y el cliente las lee como CBOR.
