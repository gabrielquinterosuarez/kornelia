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

**29 decisiones (D1–D29) están cerradas en `docs/DISENO.md`, cada una con su
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
6. **El código va en inglés; el español es solo para humanos.** Nombres de
   archivos, tipos, campos, funciones, constantes, variables, etiquetas de
   ensamblador y nombres de test: inglés. Comentarios y documentación: español.
   Los textos que salen por el UART también en español, porque son para leer en
   una terminal — pero en ASCII puro (regla 4).

   **Lo verifica `./scripts/check-language.py`, dentro del portón.** Esta regla
   estuvo escrita acá y se rompió igual, dos veces: el proyecto ya sabe que una
   regla que no se comprueba es una intención (D23). El chequeo es una lista de
   palabras y por eso no es completo — cuando se cuela una que no está, se
   agrega a la lista y deja de poder volver.

## Estado actual

Arranca por UEFI en x86_64 y aarch64, le toma la máquina al firmware y **habla
CBOR** por el cordón umbilical. Corre sobre pila y tablas de páginas propias, y
captura los faults en vez de reiniciarse. **El núcleo que atiende duerme entre
pedidos**: el cable serie tiene timbre (interrupción), así que ya no gira
preguntando.

**Los once verbos andan, y enteros en las dos arquitecturas.** El único que sigue
siendo solo de x86_64 es `irq.install_raw`, porque en aarch64 no hay un camino
más crudo que el que ya se usa — y la máquina lo dice en vez de callarlo.

El agente sube código máquina, lo corre, y
si falla **el fault vuelve como respuesta en vez de matar la máquina** (P5) —
ni siquiera destruyendo el puntero de pila, porque las excepciones entran en una
pila aparte (IST en x86_64, `SP_EL1` en aarch64).

**D27 está entero:** el agente declara con qué privilegio corre y `exec supervised`
entra a anillo 3 / EL0. Ahora `cli` —lo único de lo que el kernel no podía
volver— vuelve como fault estructurado. La pila sale del final del reclamo del
agente y se vuelve por una ventanilla (`int 0x80` / `svc #0`) cuyos bytes
publica `describe`, así el agente no los tiene horneados (P4).

`describe` sirve mapa de memoria, tablas, reclamos, núcleos, controlador de
interrupciones y PCIe, leídos de ACPI — más el acuerdo de `exec`: qué modos hay
y con qué bytes se vuelve de `supervised`.

`core.claim` arranca los otros núcleos: PSCI en aarch64, INIT/SIPI más un
trampolín de 16→32→64 bits en x86_64. **Y ahora reciben trabajo:** `exec {core}`
deja el pedido en un buzón por núcleo, el núcleo **duerme** hasta que lo
despierta un IPI/SGI, corre y contesta. Lo comprueba el código del agente
diciendo en qué núcleo está.

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

El portón es `./scripts/check.sh`: frontera + idioma + 89 tests + compila las
dos + las bootea en QEMU y les habla el protocolo con `scripts/client.py`.
Corrélo antes de commitear; CI corre exactamente ese script.

## Lo que sigue

**No queda ningún verbo sin hacer, ni nada que ande en una arquitectura y no en la
otra.** Lo que queda es pagar deudas. Las preguntas abiertas están en
`docs/DISENO.md` §8; las deudas, en §7 — abiertas la 2, 3, 6, 10, 11, 13 y 15.

1. **Hacer cumplir la regla de D29**, que recién ahora se puede: en el núcleo del protocolo el
   agente debería correr siempre `supervised`, y si quiere `raw` que reclame un núcleo. Antes
   exigirlo dejaba `raw` sin ningún lugar donde correr; ahora `exec` elige núcleo, así que no.
   Es un cambio chico en el verbo y grande en las pruebas — casi todas corren `raw` en el núcleo
   que atiende.
2. **`exec` en otro núcleo es sincrónico** (deuda 13): el del protocolo espera con un tope, así
   que un trabajo largo se informa igual que un núcleo perdido. Falta la forma asincrónica.

## Cosas que ya costaron caras

Bugs que aparecieron una vez, no se ven venir, y **no se parecen a su causa**.
Están acá para no volver a pagarlos:

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

**Cómo se depura un núcleo que se quedó mudo.** No hay debugger: se marca el
camino con letras por el cable (`p.uart_write_byte(b'A')`) y se lee la traza.
Así apareció lo de CPACR — `1ST234KJ2Da2` y ninguna `b` dijo que los dos núcleos
hacían su parte y el reclamado moría entre terminar el trabajo y guardar la
respuesta. Las letras se sacan antes de commitear.

**Las máquinas de prueba llevan aparatos a propósito.** `scripts/run-*.sh`
arrancan QEMU con IOMMU (`-device intel-iommu`, `-machine virt,iommu=smmuv3`) y
con `-device edu`, que es un motor de DMA que se maneja con cuatro escrituras.
Sin ese aparato, `dma.allow` no se podría probar contra nada real.

## Cómo correrlo

```bash
./scripts/run-x86_64.sh      # Ctrl-C para salir (NO Ctrl-A X: ver D26)
./scripts/run-aarch64.sh
./scripts/client.py --what memory   # hablarle el protocolo
./scripts/client.py --supervised    # D27: correr sin privilegio y ver el fault
./scripts/client.py --smp 4 --on-core   # mandar el codigo a otro nucleo
./scripts/check.sh                  # el porton entero
```
