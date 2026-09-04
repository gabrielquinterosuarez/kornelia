---
tipo: duda
estado: cerrada
abierta: 2026-09-04
cerrada: 2026-09-04
sobre: "[[Aparato]]"
---

# ¿Qué es un aparato?

## Qué decía el libro

Nada. **La palabra aparecía 496 veces en el vault y 37 en la documentación del proyecto, y no estaba definida en ninguna parte.** No estaba en el [[Glosario]], no tenía nota de concepto, y ningún capítulo la explicaba antes de usarla.

## Qué no cerraba

Todo. Es el término más usado del libro y el lector no tenía de dónde agarrarse. Y es peor que un olvido cualquiera: es la regla del propio libro incumplida en el peor lugar posible — *"los términos técnicos se explican la primera vez que aparecen, en dos líneas, y se enlazan al concepto. No hay 'como es sabido'"*.

## En qué quedó

Se escribió [[Aparato]], y contestarla obligó a encontrar una definición que sirviera para pensar y no solo para clasificar. La que quedó: **un aparato no es una categoría de cosas, es una categoría de relación con el procesador.** Un disco, un chip de temperatura y un reloj no tienen nada en común salvo *cómo se los alcanza*.

Y de ahí salió algo que no estaba en ningún lado del libro y que ordena varias partes: **un aparato se define por cuatro canales** — ocupa direcciones, puede avisar, puede tocar la memoria solo, y se puede descubrir. Con eso:

- Se explica por qué el [[UART]] es el aparato con el que arranca todo kernel: es el único que no necesita ni DMA ni bus enumerable.
- Se explica que el controlador de interrupciones y el [[IOMMU]] **son aparatos**, aunque no se sientan como tales.
- Los síntomas se ordenan solos: si algo falla con un aparato, es uno de los cuatro canales.
- Y **los once verbos dejan de parecer un número arbitrario**: son los cuatro canales, más memoria, más núcleos, más ejecutar código.

Se agregó además [[Falsos-amigos#13]], porque al escribirlo apareció un falso amigo que no estaba registrado: **"controlador" en español significa dos cosas** — el chip (*controller*) y el código que le habla (*driver*). "El controlador del disco falló" puede querer decir que se quemó el chip o que hay un bug.

## Lo que esta duda enseñó sobre el libro

**Las palabras que más se usan son las que menos se definen**, justamente porque el que escribe ya no las ve. Ninguna auditoría de enlaces la iba a encontrar: el término no estaba "mal enlazado", no existía como concepto, así que no había enlace roto que delatara el hueco.

Conviene el chequeo inverso: **listar las palabras más frecuentes del vault y ver cuáles no tienen entrada.** Es barato y encuentra exactamente esta clase de agujero.
