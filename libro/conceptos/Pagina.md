---
tipo: concepto
estado: pendiente
dificultad: 2
principios: [P1, P4]
decisiones: [D12, D27]
practicas: [P03-Ver-las-tablas-de-paginas, P07-Medir-la-fragmentacion-interna]
capitulos: [24-Alineacion-la-promesa-que-el-cargador-no-cumple, 20-Tablas-de-paginas-de-verdad, 22-Identity-map-la-mentira-mas-simple]
---

# Página

> La **unidad** de la traducción de direcciones: el hardware no traduce direcciones sueltas, traduce bloques de tamaño fijo. Casi siempre 4096 bytes.

Todo lo que se puede decir de la memoria —dónde está, quién la alcanza, si se cachea, si se puede ejecutar— se dice **de a una página**. No hay grano más fino.

## Qué problema resuelve

Si la [[MMU]] tradujera dirección por dirección, la [[Tabla-de-paginas|tabla]] tendría una entrada por byte: más grande que la memoria que describe. Hay que agrupar.

Y una vez que agrupás, el tamaño del grupo es **una perilla con dos extremos malos**:

- **Muy chico**: la tabla tiene muchísimas entradas y el [[TLB]] —que guarda unas pocas decenas de traducciones— cubre una porción ridícula de la memoria.
- **Muy grande**: cada pedido chico desperdicia el resto del bloque, y los permisos se vuelven groseros: no podés decir "estos 100 bytes son de solo lectura" si el grano es 2 MiB.

La página es el punto donde ese compromiso quedó parado. Que sea un compromiso y no una verdad es lo que explica que existan **varios** tamaños a la vez.

## De dónde sale el 4096

De dos cuentas que dan lo mismo, y por eso quedó:

1. **12 bits de desplazamiento.** Los bits de abajo de la dirección pasan sin traducir; 12 bits son 4096 bytes. Y como esos 12 bits siempre son cero en la base de una página, las entradas de la tabla los usan para guardar banderas — la dirección y los permisos entran en el mismo `u64`.
2. **Una tabla entra en una página.** 512 entradas × 8 bytes = 4096 exactos. Así el asignador de tablas y el asignador de páginas son el mismo, y cada nivel del árbol come 9 bits (`kernel-x86_64/src/paging.rs:29#const ENTRIES: usize = 512;`).

El 386 lo fijó en 1985 y no se movió más, porque a esta altura es lo que espera todo el software. Antes hubo otros: la VAX usaba 512 bytes. **Y no es universal hoy:** aarch64 elige el grano en `TCR_EL1` entre 4, 16 y 64 KiB, y macOS en los chips de Apple usa 16 KiB. Con grano de 64 KiB alcanzan tres niveles en vez de cuatro.

## Páginas grandes

Una página de 2 MiB o de 1 GiB no es otro mecanismo: es **el mismo recorrido, cortado un nivel antes**. Una entrada intermedia dice "yo *soy* la página" y ahí termina.

| Tamaño | Bits de desplazamiento | Niveles que se recorren | Una entrada de TLB cubre |
|---|---|---|---|
| 4 KiB | 12 | 4 | 4 KiB |
| 2 MiB | 21 = 12 + 9 | 3 | 512 veces más |
| 1 GiB | 30 = 12 + 9 + 9 | 2 | 262.144 veces más |

Lo que se gana es **presión sobre el TLB**, que es lo que se nota en una base de datos o una máquina virtual: con 4 KiB, 8 GiB de memoria activa no entran ni de casualidad en el TLB y cada acceso paga un recorrido. Lo que se pierde es grano: los atributos son de todo el bloque, y el desperdicio del final también.

## Alineación

Una página **siempre** empieza en un múltiplo de su tamaño, y no por convención: la entrada de la tabla guarda la dirección desplazada, sin los bits de abajo. Una dirección desalineada **no se puede escribir** ahí. Lo mismo hacia arriba: un bloque de 2 MiB tiene que estar alineado a 2 MiB, porque los bits 12 a 20 tampoco están en la entrada.

De ahí sale la regla que los programadores conocen sin saber de dónde viene: `mmap` devuelve direcciones alineadas a página, y los tamaños se redondean hacia arriba.

## Fragmentación interna

El redondeo tiene nombre: **fragmentación interna** es lo que se desperdicia entre lo que pediste y el final de la página. Pedís 100 bytes y ocupás 4096: 97% tirado. Con bloques de 2 MiB el mismo pedido tira el 99,995%.

Es lo contrario de la fragmentación **externa**, que es tener memoria libre de sobra pero en pedazos demasiado chicos para el pedido. La paginación mata la externa y crea la interna: el intercambio es a propósito.

## Cómo lo hace Linux

```bash
getconf PAGE_SIZE                    # 4096 en casi todo
grep -E 'Hugepagesize|AnonHugePages|HugePages_Total' /proc/meminfo
cat /sys/kernel/mm/transparent_hugepage/enabled     # [always] madvise never
ls /sys/kernel/mm/hugepages/         # los tamaños que soporta esta máquina
grep -o -m1 -E 'pse|pdpe1gb' /proc/cpuinfo   # pse = 2 MiB, pdpe1gb = 1 GiB
grep -i huge /proc/self/smaps | sort | uniq -c
```

Hay dos caminos distintos para páginas grandes y se confunden:

- **hugetlbfs**: se reservan a mano (`nr_hugepages`), no se pueden intercambiar, y el programa las pide con `mmap(MAP_HUGETLB)`. Es lo que usan las bases de datos.
- **THP** (*transparent huge pages*): el kernel promueve a 2 MiB por su cuenta, y un hilo de fondo, `khugepaged`, va compactando memoria para poder armarlas. Se pide o se evita por rango con `madvise(MADV_HUGEPAGE)` / `MADV_NOHUGEPAGE`.

El segundo es el que da sorpresas: la promoción cuesta compactar, y compactar frena.

## Cómo lo hace Kornelia

| | |
|---|---|
| **Decisiones** | D12 (identity map con páginas de 1 GiB), D27 (el permiso, y su redondeo) |
| **Dónde vive** | `kernel-core/src/paging.rs:30#pub const GIB: u64 = 1 << 30;` y `kernel-core/src/paging.rs:184#pub const BLOCK: u64 = 2 << 20;` |

**El grano por omisión es 1 GiB, no 4 KiB.** Mapear toda la RAM son unas pocas entradas y la presión sobre el TLB queda en casi nada. Un gigabyte se baja a bloques de 2 MiB solo cuando tiene kernel o memoria libre adentro, que en la práctica son uno o dos (`kernel-core/src/paging.rs:177#El grano fino`).

**Por qué 2 MiB y no 4 KiB para el grano fino:** partir un gigabyte en páginas de 4 KiB serían 512 tablas. Y la contra está escrita y se informa: memoria del agente que caiga en el mismo bloque de 2 MiB que el kernel queda también fuera de su alcance.

**El redondeo se ve en el protocolo.** Un `mem.claim` que pide `user: true` —memoria alcanzable sin privilegio, D27— **redondea el tamaño y la alineación al bloque**, porque el permiso no se puede decir más fino que eso (`kernel-core/src/claims.rs:157#pub fn claim`). Pedís 100 bytes y te devuelve 2 MiB. Eso es fragmentación interna con nombre y apellido, elegida a propósito — y el kernel **informa lo que quedó de verdad** en vez de contestar lo que le pediste (P4).

**No se da por sentado que la máquina tenga páginas de 1 GiB.** Se pregunta, y si faltan el kernel avisa en vez de armar las tablas igual: el bit que dice "soy una página" se interpretaría como parte de una dirección y el salto sería a la nada (`kernel-x86_64/src/paging.rs:245#fn has_1gib_pages`). En aarch64 el grano se elige en `TCR_EL1` y se elige 4 KiB (`kernel-aarch64/src/paging.rs:223#TG0: grano de 4 KiB`), que es lo que hace que un bloque de nivel 1 sean 1 GiB justos.

**Qué se quitó:** no hay páginas grandes *opcionales*, porque no hay chicas — no hay demand paging, no hay promoción, no hay `khugepaged`. El tamaño de página no es una perilla que el agente gire: es una consecuencia de D12.

## Cómo se ve roto

> [!danger] Una alineación mayor que la página es una promesa que el cargador no cumple
> `#[repr(align(8192))]` deja el símbolo alineado **adentro de la imagen**, pero UEFI carga la imagen en una dirección alineada a 4 KiB, y ahí una alineación de 8 KiB se pierde. Lo caro no es que la estructura quede desalineada: es que **el compilador le cree al `align`** y, dando por cierto que los bits de abajo son cero, simplifique las máscaras con las que se arma la dirección. El síntoma fue un SMMU leyendo ceros una página más abajo de donde habíamos escrito, que no se parece en nada a la causa. La salida es pedir el doble de lugar y alinear **a mano en runtime**, para que la dirección sea un dato y no una suposición: `kernel-aarch64/src/smmu.rs:152#struct StreamL1` y `kernel-aarch64/src/smmu.rs:158#static mut STRTAB`. Ver [[24-Alineacion-la-promesa-que-el-cargador-no-cumple]].

| Síntoma | Causa |
|---|---|
| El silicio lee ceros una página más abajo de donde escribiste. | La de arriba: una alineación mayor que 4 KiB que el cargador no cumplió. |
| Pediste 100 bytes alcanzables sin privilegio y te devolvieron 2 MiB. | No es un bug: el permiso no se puede decir más fino que un bloque de la tabla, así que el pedido redondea. El kernel informa el tamaño real. |
| Kornelia dice `the CPU has no 1 GiB pages`. | El CPU no tiene la extensión. Se avisa en vez de armar tablas donde el bit de página grande se leería como dirección. |
| Kornelia dice `that chunk has no fine grain`. | Ese gigabyte quedó en una sola entrada porque no tenía kernel ni memoria libre adentro. No hay bloques que marcar. |
| Kornelia dice `the range is not block-aligned`. | Se quiso marcar el permiso de un rango que no empieza ni termina en un múltiplo de 2 MiB. |
| En Linux: la máquina se traba a ratos, con un hilo `khugepaged` arriba. | Promoción a páginas grandes: compactar memoria para armar un bloque de 2 MiB cuesta, y frena a todos mientras tanto. |
| Un `malloc` de 8 bytes hace que el proceso crezca 4 KiB en `smaps`. | Fragmentación interna. La unidad de la traducción es la página; adentro reparte el asignador de la biblioteca, no el kernel. |

## Práctica

- [[P03-Ver-las-tablas-de-paginas]] — *(mirar/construir)* ver el tamaño de página real y recorrer una traducción a mano.
- [[P07-Medir-la-fragmentacion-interna]] — *(mirar)* pedir de a poco en Linux y mirar cómo crece `Rss` a saltos de 4096 en `/proc/self/smaps`.

## Recordar #flashcards/conceptos

¿Por qué la traducción es de a páginas y no de a bytes?::Porque una entrada por byte haría una tabla más grande que la memoria que describe. Hay que agrupar, y el tamaño del grupo es un compromiso entre el tamaño de la tabla y el desperdicio.

¿De dónde salen los 4096 bytes?::De 12 bits de desplazamiento —que dejan libres los 12 bits de abajo de la entrada para banderas— y de que 512 entradas de 8 bytes son exactamente 4096, o sea que una tabla entra en una página.

¿Una página de 2 MiB es otro mecanismo?::No: es el mismo recorrido cortado un nivel antes. Una entrada intermedia dice "yo soy la página". Se gana TLB y se pierde grano: los permisos y los atributos son de todo el bloque.

¿Qué es la fragmentación interna, y en qué se diferencia de la externa?::Interna es lo que se desperdicia entre lo que pediste y el final de la página. Externa es tener libre de sobra pero en pedazos demasiado chicos. La paginación mata la externa y crea la interna.

¿Por qué una página tiene que estar alineada a su tamaño?::Porque la entrada de la tabla no guarda los bits de abajo de la dirección: los usa para banderas. Una dirección desalineada literalmente no se puede escribir ahí.

En Kornelia, ¿qué pasa si pedís memoria alcanzable sin privilegio?::El tamaño y la alineación se redondean a 2 MiB, que es el grano del permiso, y el kernel te informa lo que quedó de verdad en vez de repetirte lo que pediste (D27, P4).

## Ver también

- [[MMU]] · [[Tabla-de-paginas]] · [[TLB]] · [[Memoria-virtual]] · [[Espacio-de-direcciones]]
- [[24-Alineacion-la-promesa-que-el-cargador-no-cumple]] · [[23-Asignadores-y-por-que-aca-no-hay]]
- [[Falsos-amigos#11]] — página, palabra y byte: los tres tamaños que se confunden.
