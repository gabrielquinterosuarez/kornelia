---
tipo: duda
estado: cerrada
abierta: 2026-09-04
cerrada: 2026-09-04
sobre: "[[P00-Armar-la-VM-de-practicas]]"
---

# ¿`qemu-img create ... 20G` ocupa 20 GB de mi disco? ¿Y cuál es el mínimo viable?

## Qué decía el libro

> ```bash
> qemu-img create -f qcow2 -F qcow2 \
>   -b debian-13-genericcloud-amd64.qcow2 practicas.qcow2 20G
> ```
>
> Eso es *copy-on-write*: el archivo nuevo pesa unos kilobytes y crece solo con lo que cambies.

## Qué no cerraba

La frase "pesa unos kilobytes" estaba, pero **el número `20G` en el comando la contradecía visualmente** y ganaba el número. Y aunque se entienda que no ocupa 20 GB, quedaba sin contestar lo segundo, que es lo práctico: si no ocupa 20 GB, ¿cuánto ocupa? ¿y qué número hay que poner?

## En qué quedó

Se midió en la máquina, no se explicó de memoria. Tres hechos, y uno era una trampa:

1. **No ocupa 20 GB.** El overlay recién creado ocupa **196 KiB**. `20G` es el tamaño *virtual*: lo que la VM va a creer que mide su disco.
2. **Crece y no se achica.** Escribirle 200 MiB lo lleva a 201 MB, y borrarlos adentro **no lo baja**. Hace falta `discard=unmap` más `fstrim`, o volver al snapshot.
3. **El mínimo es 3 GiB**, el tamaño virtual de la imagen base — que se puede leer del encabezado qcow2 sin bajar el archivo. Pero como el tamaño no cuesta nada hasta que se usa, el mínimo no es el número que conviene poner: lo que se ocupa de verdad son 2 a 3 GB, y con 3 GiB justos te quedás sin lugar al instalar los paquetes.

**La trampa:** un overlay **más chico** que su base se crea sin un solo aviso y queda roto — leer más allá del corte da `Input/output error`. Es el mismo patrón que el proyecto ya pagó con el SMMU: *un límite que sobra puede ser tan inválido como uno que falta*. Ahí salió una recomendación que no estaba: **omitir el tamaño** hereda el de la base, que es correcto por construcción.

Quedó escrito en [[P00-Armar-la-VM-de-practicas]], en dos secciones nuevas: *¿Ese `20G` me come 20 GB de disco?* y *El mínimo viable*.

## Lo que esta duda enseñó sobre el libro

Que **un número en un bloque de código pesa más que la prosa que lo rodea**. La explicación correcta estaba escrita al lado y no alcanzó. Conviene revisar el resto de las prácticas buscando lo mismo: comandos con constantes que el lector va a leer como una recomendación sin justificación.
