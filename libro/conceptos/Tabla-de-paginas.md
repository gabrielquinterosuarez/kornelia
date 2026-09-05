---
tipo: concepto
estado: pendiente
dificultad: 4
principios: [P4, P6]
decisiones: [D12, D27]
practicas: [P03-Ver-las-tablas-de-paginas, P06-Desarmar-una-entrada-de-tabla]
capitulos: [20-Tablas-de-paginas-de-verdad, 22-Identity-map-la-mentira-mas-simple, 21-TLB-invalidacion-y-barreras]
---

# Tabla de páginas

> La estructura de datos, en memoria común, que **el software escribe y el hardware lee** para traducir. Un árbol de tablas de 512 entradas, cada una del tamaño de una página.

No es un registro ni un pedazo de silicio: son bytes en RAM, y se pueden mirar con un volcado de memoria. Lo único que sabe el hardware es dónde empieza la raíz.

## Qué problema resuelve

La [[MMU]] tiene que traducir cada acceso, y la traducción tiene que estar guardada en algún lado. La forma obvia —un arreglo indexado por número de [[Pagina|página]]— no cierra por tamaño.

Hacé la cuenta: 48 bits de dirección virtual, páginas de 4 KiB, o sea 2³⁶ páginas. A 8 bytes por entrada son **512 GiB de tabla, por espacio de direcciones**. Para traducir memoria hace falta más memoria que la que hay.

Y el arreglo estaría casi entero vacío: un programa usa unos pocos rangos separados por huecos enormes. Lo que hace falta es una estructura **dispersa**, donde un hueco no cueste nada.

## Cómo funciona

La solución es un **árbol de radix**: se parte el índice en pedazos y cada pedazo elige una entrada de un nivel. Una rama que no existe se marca con un bit y ahí termina — el hueco cuesta una entrada, no 512 GiB.

El recorrido nivel por nivel está en [[MMU]] y no se repite acá. Lo que importa acá es la **forma** del árbol y el **contenido** de una entrada.

Los números salen todos de una sola decisión: **una tabla tiene que entrar en una página**. 512 entradas × 8 bytes = 4096 bytes exactos. De ahí que cada nivel consuma 9 bits de la dirección (2⁹ = 512), y de ahí que con 12 bits de desplazamiento y cuatro niveles se llegue a 12 + 9×4 = **48 bits**. No es casualidad: es que el asignador de tablas y el asignador de páginas puedan ser el mismo.

### El formato de una entrada en x86_64

Una entrada es un `u64`. La dirección ocupa el medio, y arriba y abajo van banderas — se pueden meter ahí justamente porque la dirección está alineada y sus bits de abajo son cero.

| Bit(s) | Nombre | Qué dice | Quién lo escribe |
|---|---|---|---|
| 0 | **P** (Present) | Si está en 0 **todo lo demás queda libre para el software**: el kernel guarda ahí dónde está la página en el disco. | Software |
| 1 | **R/W** | 0 = solo lectura. | Software |
| 2 | **U/S** (User) | 1 = alcanzable desde el nivel sin privilegio. | Software |
| 3 | **PWT** | Write-through. | Software |
| 4 | **PCD** | [[Cache]] disable. Con PWT arriba, "no cacheable". | Software |
| 5 | **A** (Accessed) | Alguien la tocó. | **Hardware** |
| 6 | **D** (Dirty) | Alguien la **escribió**. Solo en las hojas. | **Hardware** |
| 7 | **PS** | "Esta entrada *es* la página": corta el recorrido acá. | Software |
| 8 | **G** (Global) | No se tira del [[TLB]] al cambiar de espacio. | Software |
| 12–51 | Dirección física | El marco, o la tabla del nivel siguiente. | Software |
| 63 | **NX** | No ejecutable. | Software |

**A y D son la única parte que el hardware escribe, y nunca las apaga.** Esa asimetría es todo el mecanismo de reemplazo de páginas de un sistema operativo grande: el kernel las borra, deja pasar un rato, y las que se volvieron a prender son las que se están usando. Las sucias hay que escribirlas a disco antes de reusarlas; las limpias se descartan gratis.

### El equivalente en aarch64

La idea es la misma y **no hay un solo bit en el mismo lugar**.

| x86_64 | aarch64 | La diferencia que importa |
|---|---|---|
| P (bit 0) | Bits [1:0] del descriptor: `0b00` inválido, `0b01` **bloque**, `0b11` **tabla** (o página en el último nivel). | En x86 "soy hoja" es un bit aparte (PS). En ARM está en el **tipo** del descriptor, y qué significa `0b11` depende del nivel. |
| U/S y R/W, dos bits sueltos | **AP[2:1]**, bits [7:6]: `00` EL1 RW · `01` EL1+EL0 RW · `10` EL1 RO · `11` EL1+EL0 RO. | Un solo campo dice privilegio **y** escritura, así que no se pueden combinar libremente. |
| NX, un bit | **PXN** (53) y **UXN** (54): no ejecutable con privilegio, y no ejecutable sin él. | Dos bits, uno por nivel. |
| PWT/PCD, la cacheabilidad en la entrada | **AttrIndx**, bits [4:2]: un **índice de 3 bits** a `MAIR_EL1`, un registro con ocho ranuras. | El descriptor no dice *qué* es la memoria: dice *cuál de las ocho definiciones* usar. |
| — | **SH**, bits [9:8]: shareability. | x86 no tiene equivalente: la coherencia es implícita. |
| A (bit 5) | **AF**, bit 10, y al revés: si está en **cero**, el primer acceso **da [[Fault|fault]]**. | El software lo prende en el [[Handler|handler]] y así se entera. Hardware que lo prenda solo es opcional (ARMv8.1). |
| D (bit 6) | **DBM**, bit 51, y también opcional. | Sin él, "sucio" se emula: se mapea de solo lectura, el primer intento de escritura da fault, y ahí se anota. |

Que dos arquitecturas resuelvan lo mismo tan distinto es el argumento entero de D22/D23: un kernel escrito contra una sola cree que el formato *es* el concepto.

## Cómo lo hace Linux

Linux le puso nombre a cada nivel y escribió una función por nivel, así que el recorrido en C se lee como el dibujo: `pgd_offset`, `p4d_offset`, `pud_offset`, `pmd_offset`, `pte_offset_map`. Los tipos son `pgd_t`, `pud_t`, `pmd_t`, `pte_t` — envoltorios de un entero, para que el compilador no deje mezclar niveles.

Los bits están con nombre en `arch/x86/include/asm/pgtable_types.h` (`_PAGE_BIT_PRESENT`, `_PAGE_BIT_DIRTY`, `_PAGE_BIT_ACCESSED`) y se tocan con constructores: `pte_mkwrite`, `pte_mkdirty`, `pte_young`. Nadie escribe el `u64` a mano.

Para mirarlo desde afuera:

```bash
grep PageTables /proc/meminfo        # cuánta RAM se está yendo en tablas
sudo cat /sys/kernel/debug/kernel_page_tables   # el volcado, con CONFIG_PTDUMP_DEBUGFS
sudo hexdump -C /proc/self/pagemap   # 8 bytes por página: bit 63 presente, 0-54 el marco
echo 4 | sudo tee /proc/self/clear_refs   # borra el soft-dirty y volvés a mirar
```

`/proc/self/pagemap` es lo más cerca que se llega de la entrada real sin ser el kernel: no te da los bits de permiso, pero sí el número de marco físico, y con eso se puede comprobar a mano que dos procesos comparten una biblioteca.

## Cómo lo hace Kornelia

| | |
|---|---|
| **Decisiones** | D12 (identity map con páginas de 1 GiB), D27 (el bit de usuario, bloque por bloque) |
| **Dónde vive** | `kernel-x86_64/src/paging.rs:5#Cuatro niveles de tablas` y `kernel-aarch64/src/paging.rs:54#const IS_TABLE: u64 = 0b11;`; el plan común en `kernel-core/src/paging.rs:30#pub const GIB: u64 = 1 << 30;` |

Las tablas son **arreglos estáticos dentro de la imagen del kernel**. No es prolijidad: un estático cae en memoria que [[UEFI]] cargó como `LoaderData` y el mapa informa como `Kernel`, y esa clase no se entrega nunca. Si vivieran en memoria libre, `mem.claim` se las podría dar al agente — y eso no falla donde se escribe, falla en la próxima traducción, en cualquier parte.

Cuatro cosas concretas:

1. **El árbol es enano.** Una raíz, hasta ocho tablas de segundo nivel (4 TiB de alcance) y hasta cuatro de bloques. Con entradas de 1 GiB (`kernel-x86_64/src/paging.rs:57#const HUGE: u64 = 1 << 7;`), mapear toda la RAM son unas pocas entradas.
2. **Se parte solo donde hace falta.** Un gigabyte se baja a bloques de 2 MiB únicamente si tiene kernel o memoria libre adentro (`kernel-core/src/paging.rs:206#pub fn needs_split`), que en la práctica son uno o dos. Lo demás queda en una sola entrada.
3. **El permiso efectivo es el AND de todos los niveles.** Por eso la raíz y el nivel de arriba llevan el bit de usuario **prendido** aunque casi nada de abajo sea del agente: si estuviera apagado ahí, lo que digan los bloques no importaría (`kernel-x86_64/src/paging.rs:262#const USER: u64 = 1 << 2;`, `kernel-aarch64/src/paging.rs:289#const AP_USER: u64 = 0b01 << 6;`). Quien decide es cada bloque, uno por uno, cuando el agente reclama memoria pidiéndolo (D27, P6).
4. **En aarch64 hay que configurar `MAIR_EL1` antes que nada.** El descriptor solo lleva el índice: si el registro no dice qué significa la ranura 1, "memoria de [[Aparato|dispositivo]]" no quiere decir nada (`kernel-aarch64/src/paging.rs:70#const MAIR: u64 = 0x0000_0000_0000_04FF;`).

**Qué se quitó.** No hay una tabla por espacio, porque hay un solo [[Espacio-de-direcciones|espacio]] (D12, D13). **Los bits A y D no los mira nadie**: existen, el hardware los prende, y el kernel no los lee nunca porque no hay reemplazo de páginas ni disco a dónde escribir (ver [[Memoria-virtual]]). Y no hay asignador de tablas: son estáticas con techo, y si el mapa no entra el kernel **avisa** en vez de mapear a medias.

## Cómo se ve roto

| Síntoma | Causa |
|---|---|
| La máquina se reinicia en la instrucción siguiente a cargar la raíz. | Las tablas nuevas no mapean el código que está corriendo. La próxima instrucción se busca en una dirección que no existe. |
| Marcaste el bloque como de usuario y sigue sin alcanzarse. | Algún nivel de arriba tiene el bit apagado. El permiso efectivo es el **AND** de la cadena entera: un nivel que dice que no gana sobre cuatro que dicen que sí. |
| Marcaste una página para el agente y el **kernel** dejó de poder ejecutar la suya. | Es lo esperado, no un bug: SMEP en x86_64, el modelo de permisos en aarch64. Una página es del agente o la ejecuta el kernel, nunca las dos. Por eso los bits se prenden después de que el agente corra sin privilegio, no antes. |
| Cambiaste una entrada y el procesador sigue traduciendo como antes. | El [[TLB]]. La tabla es memoria común: el hardware no se entera de que la escribiste. Ver [[TLB]]. |
| En aarch64 anda todo hasta que arranca el segundo núcleo, y ahí se corrompe. | Falta el atributo de *shareable* en el descriptor. En x86 no existe el problema porque la coherencia es implícita; ARM lo hace explícito. |
| Todo da fault en el primer acceso, aunque el mapeo esté bien. | `AF` en cero. En ARM el *access flag* no es informativo: si no lo prendés, el acceso no ocurre. |
| Kornelia dice `more chunks contain kernel than can be split`. | Hay más gigabytes con kernel o memoria libre adentro que tablas de bloques estáticas. Se avisa en vez de mapear a medias. |
| Kornelia dice `CR3 did not end up pointing at our tables`. | La raíz **se relee** del registro. Un `install` que no hiciera nada se vería igual que uno que anduvo, y el síntoma llegaría mucho después y en otro lado. |

## Práctica

- [[P03-Ver-las-tablas-de-paginas]] — *(mirar/construir)* recorrer una traducción a mano en Linux con `/proc/self/pagemap`, y volcar el identity map de Kornelia desde el monitor de [[QEMU]].
- [[P06-Desarmar-una-entrada-de-tabla]] — *(construir)* tomar un `u64` de una tabla real y separarle a mano la dirección de las banderas, en las dos arquitecturas.

## Recordar #flashcards/conceptos

¿Por qué la tabla de páginas es un árbol y no un arreglo?::Porque el arreglo sería de 2³⁶ entradas —512 GiB por espacio— y estaría casi entero vacío. El árbol es disperso: un hueco cuesta una entrada marcada como no presente.

¿De dónde salen los 512 entradas por tabla y los 9 bits por nivel?::De que una tabla tiene que entrar en una página: 512 × 8 bytes = 4096 exactos. Así el asignador de tablas y el de páginas son el mismo.

¿Qué bits de una entrada escribe el hardware?::Solo *accessed* y *dirty*, y nunca los apaga. El kernel los borra a mano y mira cuáles se volvieron a prender: ese es todo el mecanismo de reemplazo de páginas.

¿Dónde están los bits de cacheabilidad en aarch64?::En ningún lado. El descriptor lleva un índice de 3 bits a `MAIR_EL1`, un registro con ocho ranuras que definen qué significa cada índice. Sin configurar ese registro, el índice no quiere decir nada.

¿Qué significa que el permiso efectivo sea el AND de los niveles?::Que si un nivel de arriba dice que no, no importa lo que digan los de abajo. Por eso Kornelia deja el bit de usuario prendido en la raíz y decide bloque por bloque.

Si una entrada tiene el bit de presente en cero, ¿los otros bits importan?::Para el hardware no: quedan libres para el software. Linux guarda ahí en qué parte del swap está la página.

## Ver también

- [[MMU]] · [[Pagina]] · [[TLB]] · [[Memoria-virtual]] · [[Espacio-de-direcciones]] · [[Modo-privilegiado]]
- [[Tabla-de-paginas]] · [[Memoria-virtual]]
- [[IOMMU]] — otro árbol, otro formato, para las direcciones que pide un aparato.
