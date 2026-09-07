# Kernel agente-céntrico — contexto del proyecto

> Este archivo se carga solo al abrir Claude Code en esta carpeta.
> **Leé `docs/DISENO.md` antes de proponer cualquier cambio de arquitectura**, y
> `docs/MAPA.md` para saber dónde vive cada cosa sin tener que leer todo.

## Qué es

Un kernel experimental, mínimo, que **supone un agente de IA como usuario** y quita
todas las capas posibles entre ese agente y el hardware. El agente usa el cómputo con
una lógica que no tiene por qué ser la humana, sin las abstracciones ni los
guardarraíles heredados de POSIX.

No es un sistema operativo de propósito general. Es un experimento sobre qué queda de
un kernel cuando el operador deja de ser una persona.

**El autor no es programador de sistemas.** Explicá los términos técnicos cuando
aparezcan (qué es un BAR, qué es DMA, qué es un blob) sin que te lo pidan, y en pocas
líneas. Escribí en español. Los comentarios del código van en español.

## Los seis principios

| # | Principio |
|---|---|
| P1 | El kernel nunca es la razón por la que no se puede usar un dispositivo. |
| P2 | Cada capa que se saca no se reemplaza: se deja vacía para que el agente la llene si quiere. |
| P3 | El agente no es un participante en tiempo de ejecución. Es un **compilador**: escribe código que corre sin él. |
| P4 | La máquina se describe a sí misma. El agente no asume nada sobre ella. |
| P5 | Los faults son datos, no muerte. |
| P6 | El hardware hace cumplir lo que **el agente declaró**, no una política del kernel. |

## Decisiones ya tomadas

**30 decisiones (D1–D30) están cerradas en `docs/DISENO.md`, cada una con su
justificación. No las reabras sin motivo nuevo.** Las más importantes:

- **D1** El agente es externo (cliente), no residente — pero la puerta a residente queda abierta.
- **D4** El agente escribe sus propios drivers. El kernel no tiene ninguno salvo el UART.
- **D5** El UART es el cordón umbilical, no el transporte. El agente escribe el transporte rápido.
- **D6** Protocolo binario CBOR, nunca JSON (se transportan código máquina y volcados).
- **D7/D11** Los faults devuelven registros + causa + log. El kernel no deshace nada: el rollback real es imposible, no caro.
- **D8** IOMMU encendido por defecto. No es guardarraíl: hace cumplir lo que el agente declaró con `dma.allow`, y sin él un DMA mal apuntado es corrupción silenciosa.
- **D12** Identity map de toda la RAM. MMIO no-cacheable.
- **D13** Un solo agente. Se multiplica solo reclamando varios núcleos.
- **D15** El kernel es agnóstico sobre quién está del otro lado. Sin identidad ni autenticación.
- **D20** Separación kernel / distribución: `kernel.efi` (cero drivers) y `blob.bin` (reemplazable).
- **D22/D23** x86_64 **y** aarch64 en verde desde el primer commit; la frontera la verifica CI.
- **D24** La frontera son **dos ejes**: arquitectura (`asm!`) y entorno de arranque (UEFI). El código UEFI va en `boot-uefi/`, compartido; los tipos normalizados en `kernel-core/`.
- **D25** Al firmware se le pide todo (mapa de memoria, ACPI/DT, blob) **antes** de `ExitBootServices`, que se llama una sola vez. Después no hay segunda oportunidad, y solo el firmware sabe leer FAT32.
- **D29** En el núcleo del kernel **manda el kernel**: una interrupción ahí tiene prioridad sobre el código del agente. En un núcleo `dedicated` la prioridad la decide el agente. Implica que en el núcleo del protocolo el agente corre `supervised` — si quiere `raw`, que reclame uno propio.
- **D28** `listen` es el verbo **once**: el agente arma un buzón en memoria y se lo entrega como segundo canal. Se agregó en vez de esconderlo en un acuerdo implícito — un número redondo no es un principio.
- **D27** El agente **declara** si su código corre `supervised` (anillo bajo, no puede colgar la máquina) o `raw` (privilegio completo). El kernel ofrece los dos y no elige (P6): `mode` es obligatorio en `exec`, porque un valor por omisión sería el kernel eligiendo. **Solo cubre `exec`:** un handler de `irq.install` corre siempre privilegiado porque el hardware no entrega interrupciones sin privilegio.
- **D30** El cable **no se puede cerrar**. La autenticación vive en el transporte que escribe el agente o en dónde está enchufado el cable, no en el kernel: cerrarlo cambiaría seguridad por la posibilidad de perder la máquina para siempre.
- **D26** El serie va **crudo**: `-serial stdio`, nunca `mon:stdio`. El multiplexor se come el `0x01` como escape y por ahí viaja CBOR. Se sale de QEMU con `Ctrl-C`.

## Superficie del kernel

Esto es el kernel entero. **Son once** (D28: el once se agregó porque D17
prometía algo que los diez no podían pedir).

`describe` · `mem.claim` · `mem.read` · `mem.write` · `core.claim` · `exec` ·
`irq.install` · `irq.install_raw` · `dma.allow` · `release` · `listen`

## Reglas de código

1. **Ni `kernel-core/` ni `boot-uefi/` pueden contener código específico de
   arquitectura.** Nada de `#[cfg(target_arch)]`, `core::arch` ni `asm!`. Todo pasa
   por el trait `Platform`. `./scripts/check-boundary.sh` lo verifica y CI debe fallar
   si se rompe. Además `kernel-core/` no puede nombrar a `boot-uefi` (D24: el núcleo
   no sabe cómo arrancó).
2. **Las dos arquitecturas arrancan siempre.** Un cambio que rompe una de las dos no
   se mergea. No es solo portabilidad: x86 tiene modelo de memoria fuerte y esconde
   barreras faltantes que ARM expone.
3. **El protocolo no lleva nombres de registros horneados.** x86_64 tiene RAX, ARM64
   tiene X0–X30, RISC-V tiene x0–x31. La máquina informa qué tiene (P4).
4. **Salida del UART en ASCII puro.** Manda bytes, no texto: los acentos salen rotos.
5. Sin dependencias externas salvo que haya una razón fuerte. El kernel es `no_std`.
6. **El código y lo que el kernel *dice* van en inglés. El español es para
   explicarle a un humano, no para hablarle a nadie.** Nombres de archivos,
   tipos, campos, funciones, constantes, variables, etiquetas de ensamblador y
   nombres de test: inglés. **Y también los textos que el kernel escribe por el
   cable y los errores que devuelve el protocolo** — una cosa es un comentario y
   otra lo que el kernel dice. El operador que este proyecto supone es un agente
   (P3, D1), y el protocolo ya estaba entero en inglés: el banner en español era
   la excepción, no la regla. Comentarios y documentación: español.

   **Lo verifica `./scripts/check-language.py`, dentro del portón**, y verifica
   las dos mitades: los identificadores y los literales de los crates del kernel
   (el arnés de `tests.rs` queda afuera: no es el kernel). Esta regla estuvo
   escrita acá y se rompió igual, dos veces: el proyecto ya sabe que una regla
   que no se comprueba es una intención (D23). El chequeo es una lista de
   palabras y por eso no es completo — cuando se cuela una que no está, se
   agrega a la lista y deja de poder volver.

## Estado actual

Arranca por UEFI en x86_64 y aarch64, le toma la máquina al firmware y **habla
CBOR** por el cordón umbilical. Corre sobre pila y tablas de páginas propias, y
captura los faults en vez de reiniciarse. **El núcleo que atiende duerme entre
pedidos**: el cable serie tiene timbre (interrupción), así que ya no gira
preguntando. Y el cable **está donde la máquina dice**: se arranca con una
dirección horneada porque hay que poder hablar antes de leer nada, pero apenas
la tabla SPCR dice dónde está la consola, el kernel se muda ahí (P4).

**Los once verbos andan, y enteros en las dos arquitecturas.** El único que sigue
siendo solo de x86_64 es `irq.install_raw`, porque en aarch64 no hay un camino
más crudo que el que ya se usa — y la máquina lo dice en vez de callarlo.

El agente sube código máquina, lo corre **con los registros que él pone** —los
nombra como los nombra esta máquina, y `describe` publica cuáles se pueden
poner (D3, P4)— y si falla **el fault vuelve como respuesta en vez de matar la
máquina** (P5): ni siquiera destruyendo el puntero de pila, porque las
excepciones entran en una pila aparte (IST en x86_64, `SP_EL1` en aarch64).

**D27 está entero:** el agente declara con qué privilegio corre y `exec supervised`
entra a anillo 3 / EL0. Ahora `cli` —lo único de lo que el kernel no podía
volver— vuelve como fault estructurado. La pila sale del final del reclamo del
agente y se vuelve por una ventanilla (`int 0x80` / `svc #0`) cuyos bytes
publica `describe`, así el agente no los tiene horneados (P4).

**Y el agente declara cuánto puede tardar su código:** `exec {deadline_ms}` lo
corta si no vuelve, y vuelve como `cancelled`. Eso es lo que salva al núcleo que
atiende el protocolo — ahí las interrupciones entran (por D29 el agente corre
sin privilegio), pero el que tendría que mirar el reloj es el que se colgó. El
plazo lo declara el agente, igual que `mode`: un valor por omisión sería el
kernel opinando sobre cuánto puede tardar su código.

**Y D29 también se hace cumplir:** en el núcleo que atiende el protocolo `exec`
solo admite `supervised`. Ahí manda el kernel, y para que eso sea verdad el
agente no puede *poder* enmascarar las interrupciones. Si quiere el privilegio
entero reclama un núcleo, donde la prioridad la decide él. El kernel lo publica
(`describe exec` trae `this_core`) en vez de dejar que se descubra chocándose.

`describe` sirve mapa de memoria, tablas, reclamos, núcleos, controlador de
interrupciones y PCIe — más el acuerdo de `exec`: qué modos hay, con qué bytes
se vuelve de `supervised`, y por qué registros pasan los argumentos.

**Y eso lo averigua por los dos dialectos en que una máquina se describe:** ACPI
donde hay ACPI, y **device tree** donde no —las placas ARM y RISC-V embebidas—,
que es un árbol de nodos en big-endian y no se parece en nada. Cuál usar no lo
elige el kernel: es cuál dejó el firmware. Se comprueba booteando la misma
máquina con `acpi=off`, donde el portón exige que ande hasta el IOMMU contra un
aparato de verdad.

`core.claim` arranca los otros núcleos: PSCI en aarch64, INIT/SIPI más un
trampolín de 16→32→64 bits en x86_64. **Y ahora reciben trabajo:** `exec {core}`
deja el pedido en un buzón por núcleo, el núcleo **duerme** hasta que lo
despierta un IPI/SGI, corre y contesta. Lo comprueba el código del agente
diciendo en qué núcleo está.

**Y se puede no esperar:** con `wait: false` el kernel contesta enseguida y el
resultado queda en `describe {what:["cores"]}` hasta que se mande otro trabajo
—así el agente manda algo largo, se desconecta y vuelve a buscarlo (D14). No
hizo falta un verbo nuevo: un núcleo corre un trabajo por vez, así que su handle
ya identifica el trabajo, y el resultado es estado de la máquina.

**`dma.allow` cierra D8 en las dos:** VT-d en x86_64, SMMUv3 en aarch64. El IOMMU
arranca **encendido y vacío**, así que sin declarar nada ningún aparato llega a
la memoria. Comprobado en las dos con un aparato de verdad que hace DMA:
bloqueado sin declarar —y el silicio lo anota—, permitido al declararlo, y
bloqueado otra vez al soltar el reclamo. Los dos IOMMU hacen lo mismo y no se
parecen: al de Intel se le habla por registros, al de ARM por **colas en
memoria**, y en vez de una tabla por bus tiene una tabla de streams indexada por
el número que el bus le pone al aparato.

**Y el agente ya alcanza PCIe en las dos.** El mapa de UEFI no informa la ventana
de configuración en aarch64; la MCFG de ACPI sí, y se suma al mapa donde el mapa
se arma. Antes el kernel publicaba una dirección que él mismo hacía inalcanzable.

**Y la memoria que el kernel necesita para sí figura como suya.** En x86_64 el
trampolín con el que se arrancan los otros núcleos pasa por una página baja y
fija, que el firmware informa como libre: si el agente la reclamaba, el kernel se
quedaba sin poder arrancar núcleos. Se arregla **en el mapa** —esa página sale
como `kernel` y el firmware la parte en dos pedazos libres a los lados— y no con
un chequeo al reclamar, porque un chequeo haría que `describe memory` diga
"libre" sobre algo que `mem.claim` rechaza: dos respuestas distintas a la misma
pregunta. Así el agente lo ve antes en vez de descubrirlo chocándose (P4).

**Y alcanza los registros de un aparato aunque estén fuera del mapa.** Si el
rango que se reclama cae más arriba de lo que las tablas cubren, el kernel **lo
mapea** y reintenta, en vez de contestar `unmapped`. No es comodidad: en aarch64
los BARs de PCIe caen en 512 GiB y el mapa que da el firmware llega a 257, así
que el controlador NVMe era **inalcanzable** — o sea, el kernel era la razón por
la que no se podía usar un aparato, que es exactamente lo que prohíbe P1. Se
mapea como dispositivo porque no se sabe qué hay, y se sigue entregando como
`unreported`: alcanzarlo no es enterarse (P4).

**Y también alcanza los registros de un aparato.** Un rango que cae en un hueco
del mapa —donde quedan los BARs que el firmware no listó— se entrega con la clase
`unreported`, que no es `mmio`: el agente se lleva el rango **y** la advertencia
de que la máquina nunca dijo qué hay ahí (P4). Y `mem.read`/`mem.write` toman
`width`, porque un registro de dispositivo no es RAM: muchos solo aceptan
accesos de su ancho exacto y descartan los más angostos sin avisar.

**Y un acceso que la máquina rechaza ya no mata al kernel** (P5). `mem.read` y
`mem.write` corren en el camino del protocolo, donde no había punto de
recuperación: en aarch64, leer con el ancho equivocado dejaba la máquina muda.
Ahora los dos verbos van con el mismo punto que usa `exec`, armado alrededor de
**una sola instrucción**, y el rechazo vuelve como `access-refused` con la
dirección que cortó y los números crudos de la máquina.

**Y la máquina tiene reloj** (deuda 17): el contador que ya viene andando —`TSC`
en x86_64, `CNTPCT_EL0` en aarch64—, leído con una instrucción y sin driver. Lo
que cambia entre las dos es quién dice a qué ritmo sube: ARM lo informa en un
registro, x86 puede no decirlo, y ahí **se mide** contra el contador de
frecuencia fija que informa ACPI. Si nadie lo dice, el kernel dice que no lo
sabe en vez de calcular un tiempo falso.

**Y hay persistencia a través del reinicio** (D18): el firmware trae `blob.bin`
de la partición —solo él sabe leer FAT32, así que se carga dentro de la ventana
de D25— y el kernel lo corre antes de escuchar el cable, con la misma red que
`exec`: un blob roto vuelve como fault y el arranque sigue. Antes de saltar
**avisa y espera**, y cualquier byte lo cancela; sin eso, un blob malo dejaría la
máquina inútil en cada arranque y habría que sacarle el disco. La ventana dura
**dos segundos de verdad**, no un número de vueltas, porque ahora hay reloj.

**Y el blob corre sin privilegio** (`supervised`), que era el último lugar donde
el kernel se salteaba su propia regla. Corría `raw` justo en el núcleo que
atiende el protocolo, así que podía enmascarar las interrupciones y dejar el
cordón sordo **para siempre**: no se recuperaba ni reiniciando, porque en cada
arranque volvía a correr. Ahora el silicio no lo deja, así que el timbre entra
siempre. No hizo falta un plazo —que sería el kernel opinando sobre cuánto puede
tardar el código del agente (D27)— ni un núcleo aparte, que no existe en una
máquina de un solo núcleo. **Lo que lo hizo posible es que un cargador no
necesita privilegio:** pide memoria, declara DMA y toca registros por el
protocolo, que es como se escribió el driver de NVMe entero.

Para eso hay **dos puertas** y no una. La de siempre (`int 0x80` / `svc #0`)
dice "terminé"; la nueva (`int 0x81` / `svc #1`) dice "atendeme esto y devolveme
el control", y vuelve al código en la instrucción siguiente. Van dos puertas en
vez de un registro que las distinga porque el código que vuelve ya usa el primer
registro para su resultado, y porque dos puertas con dos significados se leen.
`describe exec` publica las dos en bytes (D3, P4).

**Y el blob le habla al kernel.** Corre antes de que exista el protocolo, así que
lo único que tenía era la máquina cruda: alcanzaba para un cargador (D19), no para
algo que quisiera reclamar memoria. Ahora recibe en el segundo registro de
argumento la dirección de **una función** que atiende un pedido y deja la
respuesta en un buffer suyo. No hizo falta un verbo nuevo ni una ventanilla como
la de `supervised`: el blob corre privilegiado y en el mismo espacio de
direcciones, así que llamar al kernel es una instrucción. Y adentro es el mismo
`dispatch` de los once verbos, con un origen más — D17 ya decía que el kernel
contesta por donde le llegó el pedido; esto agrega una tercera puerta, no un
mecanismo. Se comprueba mirando los reclamos después del arranque: el que pidió
el blob está, y con el blob cancelado no está.

**Y lo que el agente graba sobrevive al reinicio.** El driver de NVMe —que corre
del lado del agente y usa **sólo los once verbos**— lee *y escribe* el disco, así
que un programa se puede dejar grabado y aparece solo en el próximo arranque. Se
comprueba con **dos arranques**: uno graba un programa distinto del que había,
otro —máquina nueva, sin escribir nada— lo encuentra y lo corre. En un solo
arranque no se podría distinguir de haberlo dejado en memoria.

**Y la placa de red manda y recibe paquetes** (`--net`), con los once verbos y
del lado del agente, igual que el NVMe. Se comprueba con un ARP de ida y vuelta
contra el otro extremo del cable: la respuesta trae una MAC que no teníamos y
viene dirigida a la nuestra —que la leímos de la placa—, así que no se puede
fabricar desde este lado.

**Una placa de red no se puede manejar por clase, y eso cambia el diseño.** Al
NVMe se lo encuentra por lo que *hace*: `01.08.02` quiere decir "cualquier NVMe"
y un solo driver los maneja a todos. `02.00.00` sólo quiere decir "ethernet", y
abajo de esa clase cada modelo tiene registros que no se parecen en nada. O sea
que hay que **elegir modelo**, que es exactamente lo que D20 anticipaba al decir
que el blob trae "drivers para una lista conocida": la lista existe porque no
hay forma de no tenerla. Por eso las dos máquinas de prueba llevan ahora la
**misma** placa forzada —Intel 82540EM (`8086:100e`), la placa real más simple
que hay— en vez de la que QEMU pone por omisión, que es distinta en cada una
(`e1000e` en q35, `virtio-net` en virt). Con una sola placa hay **un** driver y
anda en las dos, que es la regla que no se rompe (D22).

**Y el kernel contesta el protocolo por la red** (`--transport`), que es lo que
el proyecto venía prometiendo desde el primer commit y no tenía. Encima del
driver corre un **bucle de código máquina del agente** en un núcleo que reclamó
(D29: ahí manda él), que mueve bytes entre la placa y el buzón que le entregó
con `listen`. El kernel contesta por el buzón sin enterarse de que del otro lado
hay una red (D4). La prueba no admite interpretación: **el pedido no sale por el
cable** — sale de un socket del host, cruza la red, y la respuesta vuelve por el
mismo camino. El cable sólo arma todo y pregunta después, que es justo lo que
D17 exige que siga andando.

Gira en vez de esperar una interrupción porque esta placa no tiene MSI, y el
camino viejo pide interpretar AML. Un núcleo que gira es exactamente para lo
que sirve un núcleo propio.

**Y ahí aparece un hecho del diseño que no estaba dicho: el buzón es un flujo de
bytes, no una cola de mensajes.** El kernel sabe dónde termina un pedido porque
escanea el CBOR a medida que entra (`cbor::scan`); del lado del agente hacer lo
mismo sería un decodificador de CBOR en código máquina. Así que el transporte
**no interpreta nada** — es un caño: manda los bytes que haya cuando los haya, y
los mensajes los arman las dos puntas. Un pedido o una respuesta pueden venir
partidos en varios datagramas, y el cliente los junta hasta que el CBOR cierra.

Los datagramas de vuelta van de **tamaño fijo, con el largo adelante**. No es
capricho: así la cabecera de IP entera —incluido su checksum, que es lo único
que este transporte tendría que calcular— es constante, y la arma el cliente una
vez sola. El bucle sólo copia bytes.

**Y hay un ensamblador chiquito** (`Asm` en `client.py`), porque `emit_writes`
alcanzaba mientras el código del agente fuera "escribí esto acá". Esto tiene
bucles, condiciones y dos copias de largo variable. Está verificado instrucción
por instrucción contra `clang`: en aarch64 los bytes son idénticos, y en x86_64
significan lo mismo con codificaciones más largas — los saltos van de tamaño
fijo **a propósito**, para que las etiquetas no se muevan entre las dos pasadas.

**Ojo: del blob sigue estando el mecanismo, no el contenido.** No hay un
`blob.bin` en el repo — el único blob que existe es el de prueba que genera
`client.py`. Los dos drivers que nombran D19 y D20 **ya existen** (NVMe y red,
los dos con los once verbos), pero viven en Python del lado del cliente. Lo que
falta es mudarlos adentro del blob: la lógica ya está probada de punta a punta y
lo único que cambia es quién consigue los recursos.

El portón es `./scripts/check.sh`: frontera + idioma + **las citas del libro** +
los tests + compila las
dos + las bootea en QEMU y les habla el protocolo con `scripts/client.py`, **y
bootea aarch64 una vez más sin ACPI** para que el device tree no sea una
intención.
Corrélo antes de commitear; CI corre exactamente ese script.

## Lo que sigue

**No queda ningún verbo sin hacer, ni nada que ande en una arquitectura y no en la
otra, ni ninguna deuda abierta** (§7 de `docs/DISENO.md` está entera en resuelto).
Las preguntas abiertas siguen en §8. Lo que sigue son cosas que ninguna deuda
cubría todavía:

1. **Mudar los dos drivers al blob.** NVMe y red andan y están probados de punta a
   punta, pero viven en Python del lado del cliente. D19 y D20 los quieren
   compilados adentro de `blob.bin`, que es lo que haría que una máquina arranque
   con red **sin que haya nadie del otro lado del cable**. La lógica no cambia:
   cambia quién consigue los recursos. Es el paso que vuelve útil todo lo demás.
2. **El buzón entrega bytes, no mensajes, y por ahora eso lo paga el agente.** El
   transporte no puede saber dónde termina una respuesta sin decodificar CBOR, así
   que es un caño y quien arma los mensajes son las puntas. Anda, y para el cliente
   es invisible. Si algún día molesta, la salida no es que el agente decodifique:
   es que el acuerdo de D28 publique un largo — y eso es cambiarle la superficie al
   kernel, así que no se hace sin un motivo que hoy no está.
3. **Un aparato PCIe ya puede interrumpir al agente** (`irq.install {msi:true}`): los aparatos
   de hoy no tienen cable, escriben un dato en una dirección. Lo que **no** está es el camino
   viejo (INTx), y no es olvido: saber qué cable le toca a un aparato pide interpretar AML, un
   lenguaje entero adentro de ACPI. MSI lo hace innecesario.
2. **El segundo escalón para cortar un núcleo no se puede hacer en esta máquina, y ahora se
   sabe por qué.** En x86_64 está: el NMI corta hasta al que hizo `cli`. En ARM sería el FIQ
   —`msr daifset, #2` no lo tapa— y para eso hace falta dejar el timbre del corte en el
   **Grupo 0** del GIC (que es el único que se puede entregar como FIQ) y mover **todo lo
   demás** al Grupo 1, porque hoy todas nuestras interrupciones son del Grupo 0 y prender
   `FIQEn` las mandaría a todas por ahí — y entonces no habría dos escalones sino uno.

   El diagnóstico, medido y cerrado. **El Grupo 1 no se puede reconocer en este GIC:**

   - Los grupos existen y se habilitan: `GICD_CTLR` y `GICC_CTLR` aceptan `EnableGrp1`, y
     `FIQEn` también — se les escribe y leen de vuelta lo escrito (`0x3` y `0xb`).
   - Con el Grupo 1 habilitado, una interrupción puesta ahí **queda pendiente** (`GICD_ISPENDR`
     la muestra) y el GIC la considera de prioridad suficiente. No es un problema de
     habilitación ni de prioridad ni del `PMR`.
   - Pero `GICC_IAR` devuelve **1022**, y ese número no es basura: significa "la pendiente es
     del Grupo 1 y vos lees desde el mundo seguro; si la querés, reconocela por `GICC_AIAR`".
   - Y **`GICC_AIAR` lee cero**, porque los registros del otro grupo existen sólo con
     extensiones de seguridad, y este GIC no las tiene (`GICD_TYPER` bit 10 en cero).

   O sea: el GIC nos manda a una puerta que en esta máquina no está construida. Sin poder
   reconocer una interrupción no hay handler posible, así que las normales no pueden vivir en
   el Grupo 1, así que el corte no puede quedarse solo en el Grupo 0. **La limitación es de la
   máquina, no del kernel** — y por eso el kernel **lo publica** (`describe exec` trae
   `cancel`) en vez de prometer un corte que no llega.

   **Dónde sí se podría:** en **GICv3**, donde el Grupo 0 se entrega como FIQ a EL1 y hay
   `ICC_IAR0_EL1`/`ICC_IAR1_EL1` de verdad, sin depender de extensiones de seguridad. Pero eso
   es un driver nuevo entero (redistribuidores por núcleo, la interfaz por registros de
   sistema) **y se lleva puesto el MSI**: con GICv3, QEMU no da el frame GICv2m que usa
   `irq.install {msi:true}` sino el ITS, que es otro driver grande. Es un proyecto, no un
   pendiente.

   Y el intento anterior falló por otra razón, más tonta y que conviene no repetir: movía
   interrupciones al Grupo 1 **sin habilitar el Grupo 1** (`GICC_CTLR` se quedaba en `1`). Con
   eso nada se entrega, con `FIQEn` prendido o apagado — que es justo lo que se había observado
   y llevó a descartar el FIQ como sospechoso cuando el FIQ nunca había estado en juego.


## Cosas que ya costaron caras

Bugs que aparecieron una vez, no se ven venir, y **no se parecen a su causa**.
Están acá para no volver a pagarlos:

- **El anillo del cable era más chico que el pedido más grande, y perdía en
  silencio.** El protocolo dice aceptar pedidos de 64 KiB; el buzón donde el
  handler del serie deja los bytes tenía 4 KiB. El razonamiento escrito era que
  los bytes llegan de a poco y el bucle los saca enseguida — cierto hasta que el
  kernel empezó a hacer cosas lentas (programar el IOMMU espera a que se vacíe
  una cola de comandos) con bytes llegando mientras tanto. **El síntoma no se
  parece a la causa:** un `mem.write` de 4 KiB colgaba la máquina, porque el
  pedido quedaba incompleto y el kernel esperaba para siempre el resto de un
  CBOR que ya no venía. Y había un contador de bytes perdidos que **nadie podía
  ver**: ahora se publica en `describe {what:["cable"]}`, y el portón exige que
  sea cero.
- **Un test unitario en verde no prueba que la función se llame.**
  `channel::forget` existía, estaba bien escrita y **tenía su test pasando** — y
  no la llamaba nadie. `release` revocaba con cuidado las otras dos cosas que un
  reclamo puede repartir (el permiso sin privilegio y lo que se le dejó tocar a
  un aparato), con comentarios explicando que dejarlas sería "un agujero
  silencioso", y se olvidaba del buzón: el kernel seguía leyendo **y
  escribiendo** memoria devuelta, que el próximo `mem.claim` le daba a otro. La
  regla general que queda: **cada vez que un verbo le entrega algo al kernel,
  `release` tiene que poder devolverlo**, y eso se comprueba desde afuera — el
  test que lo agarró pregunta `describe` después de soltar, no mira el código.
- **Un buffer de transmisión es uno solo, y el paquete anterior sigue ahí.** El
  transporte arma sus respuestas sobre una cabecera fija que se escribe una vez
  al principio. Pero mandar un paquete —el ARP que se manda antes de soltar el
  bucle, para que el otro extremo aprenda nuestra dirección— **arma ese paquete
  en el mismo buffer**, así que le pisaba la cabecera entera. El síntoma no se
  parece a la causa: el bucle contaba respuestas mandadas, la placa las mandaba
  de verdad, el IOMMU las dejaba pasar, y del otro lado no llegaba nada — porque
  lo que salía tenía la cabecera de un ARP. Lo separó tener **contadores en
  memoria**: el bucle no puede imprimir nada, y hacerlo por el cable desde
  adentro cambiaría los tiempos (eso ya está más abajo en esta lista).
- **Restar dos índices que dan la vuelta no es restar.** Los índices del buzón
  son contadores de 32 bits y el kernel los resta con `wrapping_sub`. Leerlos en
  registros de 64 bits y restarlos ahí da, justo después de la vuelta, un número
  enorme en vez de una diferencia chica. Pasaría **una vez cada 4 GiB** de
  tráfico: la clase de bug que no aparece en ninguna prueba y sí en producción.
  Y en x86_64 no se arregla enmascarando con una constante, porque el inmediato
  de 32 bits se extiende **con signo** y `0xFFFFFFFF` se vuelve todo unos; se
  arregla escribiendo la mitad de abajo del registro, que pone la de arriba en
  cero.
- **La ABI de C de este kernel en x86_64 no es la de Linux: es la de Windows.**
  El target es `x86_64-unknown-uefi`, y ahí `extern "C"` pasa los argumentos por
  **RCX, RDX, R8, R9** —no RDI/RSI— y además exige que quien llama reserve 32
  bytes de pila vacía antes de la llamada. Saber la arquitectura no alcanza para
  saber la convención: la pone el *target*, no el silicio. El síntoma fue una
  llamada del blob que entraba a la función correcta y veía punteros nulos: los
  cuatro argumentos estaban ahí, en otros cuatro registros. Por eso `ARGUMENTS`
  vive en cada arquitectura y se publica (P4) en vez de deducirse.
- **Un núcleo arrancado por PSCI viene con los registros SIMD atrapados**
  (`CPACR_EL1` en cero, que es su valor de reset). El de arranque no lo sufre
  porque UEFI se los habilitó. Y el compilador usa registros anchos para copiar
  structs, así que la primera copia es una excepción — que el handler de faults
  vuelve a provocar al copiar la suya. El núcleo entra en un bucle de faults
  **sin alcanzar a avisar por el cordón**: silencio total. Se arregla en el
  trampolín, antes de saltar a Rust.
- **`GCMD` del IOMMU no es una lista de botones: es el estado entero.** El
  silicio compara lo que se le escribe contra lo que había. Pedirle "tomate la
  tabla raíz" sin arrastrar el estado **apaga la traducción de paso**, y el
  síntoma es que todo pasa — o sea que se ve como si anduviera.
- **Una prueba que pasa porque no pasa nada.** El IOMMU "bloqueando" y el DMA no
  ocurriendo se ven idénticos desde afuera. Antes de creerle a un bloqueo hay
  que comprobar que la cosa bloqueada ocurre: se bootea sin IOMMU y se mira.
  **Pasó de verdad:** el aparato `edu` recorta la dirección de DMA a 28 bits si
  no se le dice otra cosa, y en aarch64 la RAM arranca en 1 GiB — así que ningún
  destino podía llegar nunca. En x86_64 no se veía porque la RAM arranca en cero.
  Se encontró booteando sin IOMMU, que es exactamente lo que dice este párrafo.
- **Pedir una alineación mayor que la página es una promesa que el cargador no
  cumple — y el compilador le cree.** Un `#[repr(align(8192))]` queda alineado
  adentro de la imagen, pero UEFI la carga en una dirección alineada a 4 KiB y
  ahí se pierde. Lo caro no es la tabla desalineada: es que el compilador, dando
  por cierto que los bits de abajo son cero, **simplifica las máscaras** con las
  que se arma esa dirección. El síntoma fue un SMMU leyendo ceros una página más
  abajo de donde habíamos escrito. Con más de 4 KiB, se pide de más y se alinea
  a mano en runtime.
- **Al silicio hay que pedirle configuraciones que pueda hacer, no las que le
  sobren.** El tamaño de entrada de la etapa 2 del SMMU no puede ser menor que
  el de salida. Pedir 39 bits donde la máquina tiene 44 no se rechaza: se
  reinterpreta, y las direcciones que pide el aparato se recortan en silencio.
  Un límite que sobra puede ser tan inválido como uno que falta.
- **Una interrupcion que llega como pulso se pierde si el GIC la trata como
  nivel.** El frame que convierte una escritura en interrupción (MSI) no sostiene
  la línea: la sube y la baja. Por omisión el GIC trata las de aparato como
  sensibles a nivel, así que el pulso se perdía — el aparato escribía, el IOMMU
  dejaba pasar la escritura, y la interrupción no llegaba nunca. Hay que
  configurarla **por flanco** en `GICD_ICFGR`. Se encontró separando las dos
  mitades: hacerla sonar a mano con lo que el kernel publica, sin aparato.
- **El ancho de un acceso a MMIO no es un detalle, y las dos máquinas no fallan
  igual.** Un registro que solo acepta lecturas de 4 bytes, leído de a uno,
  devuelve ceros en x86_64 —silencioso, se ve como si el aparato no estuviera— y
  en aarch64 lo rechaza el bus con un abort externo que **dejaba la máquina
  muda**. El mismo pedido: en una arquitectura miente, en la otra mata. Se
  arregló con el punto de recuperación de `exec` usado afuera de `exec`
  (`guarded.rs`), pero la moraleja queda: **el kernel también toca memoria que
  puede fallar**, y ahí P5 no se cumple solo por existir el mecanismo de `exec`.
- **"Se auditó lo único que puede causarlo" es una afirmación sobre lo que uno
  se acordó de mirar.** Un test fallaba una vez cada tanto y no reproducía. La
  auditoría revisó los tests que tocan las tablas de reclamos y de núcleos —y
  esos sí tomaban el candado—; nadie miró los que leen la descripción de la
  máquina, que pisan **otros** estáticos. Apareció recorriendo la lista de
  estáticos del crate en vez de la lista de sospechosos.
  **Y para reproducirlo hubo que correr menos, no más:** con la suite entera no
  salía ni en 150 corridas; con solo los tres tests que comparten esos estáticos
  y dieciséis hilos, dos de cada cuatrocientas.
- **El núcleo que atiende el protocolo no tiene ranura en la tabla de núcleos**,
  porque no se reclama: su índice es `cores::MAX`, justo el primero que se sale
  de cualquier arreglo dimensionado por esa constante. Un arreglo así descartaba
  la marca de "a este lo cortaron" **por índice y sin decir nada**: el corte
  ocurría, y la respuesta salía con el fault viejo del arranque porque nadie
  había anotado qué había pasado.
- **Instrumentar por el cable dentro de un handler cambia el fenómeno.** Escribir
  una letra desde el handler del reloj movió el timing lo suficiente para que el
  `exec` dejara de volver. Y las letras salen después del marcador, así que el
  cliente las come como CBOR. Para algo que depende de tiempos, conviene dejar el
  dato en un estático y publicarlo por `describe`.
- **Un camino que ninguna prueba recorre no está andando: está sin probar.**
  Mientras esperaba a un núcleo reclamado, el núcleo del protocolo esperaba
  **con los timbres cerrados**, así que un handler del agente no corría y el
  cordón no se atendía en todo ese rato. Estaba desde que `exec` acepta `core`.
  No lo encontró nadie mirando: apareció cuando D29 obligó a que las pruebas
  usaran ese camino, porque recién ahí hubo una que disparaba una interrupción
  desde otro núcleo.

**Cómo se depura un núcleo que se quedó mudo.** No hay debugger: se marca el
camino con letras por el cable (`p.uart_write_byte(b'A')`) y se lee la traza.
Así apareció lo de CPACR — `1ST234KJ2Da2` y ninguna `b` dijo que los dos núcleos
hacían su parte y el reclamado moría entre terminar el trabajo y guardar la
respuesta. Las letras se sacan antes de commitear.

**Las máquinas de prueba llevan aparatos a propósito.** `scripts/run-*.sh`
arrancan QEMU con IOMMU (`-device intel-iommu`, `-machine virt,iommu=smmuv3`) y
con `-device edu`, que es un motor de DMA que se maneja con cuatro escrituras.
Sin ese aparato, `dma.allow` no se podría probar contra nada real.

## El libro (`libro/`)

Un **vault de Obsidian** con un libro sobre kernels en general que usa este kernel como
caso de estudio en cada capítulo. Es material de estudio del autor, no documentación del
proyecto: `docs/` sigue siendo la verdad sobre el diseño, y el libro **cita** a `docs/` y al
código en vez de reemplazarlos.

Vive en el repo por una sola razón: **las citas al código se pueden verificar**. El libro
cita así, con ruta, línea y ancla:

    `kernel-core/src/platform.rs:69#pub trait Platform`

El número es informativo; **el ancla es lo que manda**. Lo comprueba
`./libro/scripts/check-citas.py`, y con `--fix` reescribe los números que se corrieron. Es
D23 aplicado al libro: sin el chequeo, en dos meses el libro miente y no hay forma de saber
dónde. **Si tocás el kernel y una cita queda vieja, el arreglo es una línea.**

- La puerta del vault es `libro/00-Empezar-aca.md`; el método, `libro/El-metodo.md`.
- Las imágenes de `libro/historia/imagenes/` son de Wikimedia Commons y **cada una tiene su
  licencia anotada en `CREDITOS.md`**. Si se agrega una, se agrega ahí: varias son CC BY-SA
  y obligan a acreditar autor y licencia en el pie de foto.
- El libro va en **español, con acentos**: no le aplica la regla 4 (el ASCII puro es para lo
  que sale por el UART) ni la 6 (el inglés es para el código y para lo que el kernel *dice*).
  Sí aplica a los identificadores que el libro cite, que son del código.
- **Los nombres de archivo del vault van sin acentos ni eñes**, para no romper scripts.
- El nombre `Kornelia` es **provisorio** también para el libro: ver `libro/_El-nombre.md`.

## Cómo correrlo

```bash
./scripts/run-x86_64.sh      # Ctrl-C para salir (NO Ctrl-A X: ver D26)
./scripts/run-aarch64.sh
./scripts/client.py --console       # una terminal para hablarle a mano

# Y si se quiere que la maquina sobreviva a que el cliente se vaya (D14):
SOCKET=/tmp/kornelia.sock ./scripts/run-x86_64.sh &   # el cable sale por un socket
./scripts/client.py --connect --console               # engancharse, irse, y volver
./scripts/client.py --what memory   # hablarle el protocolo
./scripts/client.py --kvm --exec    # que el codigo lo corra el silicio, no la emulacion
./scripts/client.py --supervised    # D27: correr sin privilegio y ver el fault
./scripts/client.py --smp 4 --on-core   # mandar el codigo a otro nucleo
./scripts/client.py --net           # el driver de red: un ARP de ida y vuelta
./scripts/client.py --udp           # un datagrama del host al agente y la vuelta
./scripts/client.py --transport     # D5: el kernel contesta el protocolo por red
./scripts/check.sh                  # el porton entero
```
