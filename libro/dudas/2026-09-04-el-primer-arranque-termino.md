---
tipo: duda
estado: cerrada
abierta: 2026-09-04
cerrada: 2026-09-04
sobre: "[[P00-Armar-la-VM-de-practicas]]"
---

# ¿Esto está bien? El primer arranque escupió cientos de líneas y no sé si terminó

## Qué decía el libro

> El primer arranque tarda un par de minutos (cloud-init instala los paquetes). Después arranca en unos segundos.

## Qué no cerraba

Eso dice **cuánto** tarda y no dice **cómo se ve cuando termina**. El primer arranque escribe cientos de líneas —incluidas las claves SSH de la máquina, con sus dibujos de *randomart*— y ninguna dice "listo" de forma obvia. Peor: el log sale **fuera de orden**, así que parece que algo se rompió.

Es la sección *qué vas a ver si funciona* faltando justo donde más hacía falta. La práctica la tenía para el resultado final —"un Linux completo arrancando en tu terminal"— pero no para el paso intermedio que dura tres minutos y llena la pantalla.

## En qué quedó

Sí estaba bien. La línea que lo dice es una sola:

```
Cloud-init v. 25.1.4 finished at ... Datasource DataSourceNoCloud [seed=/dev/vdb].  Up 179.81 seconds
```

Y se lee en tres pedazos: `DataSourceNoCloud [seed=/dev/vdb]` confirma que encontró el disquito de configuración —si dijera `DataSourceNone`, no habría usuario ni clave—, `finished` que no se quedó a mitad de camino, y `Up 179.81` cuánto tardó.

Quedó escrito en [[P00-Armar-la-VM-de-practicas]] con esa tabla, más cuatro comandos para comprobar que la VM quedó usable (`cloud-init status --long`, `gcc --version`, los headers del kernel, y el `ssh -p 2222`), más dos filas nuevas en la tabla de síntomas.

## Lo que esta duda enseñó — y que terminó siendo material del libro

Investigar **por qué** el log se veía desordenado dio algo mejor que la respuesta a la pregunta. Las líneas salen tres veces por caminos distintos:

| Lo que se ve | Por dónde salió |
|---|---|
| `-----BEGIN SSH HOST KEY KEYS-----` | cloud-init escribiendo directo a la consola |
| `<14>Sep  4 15:43:30 cloud-init:` | el mismo mensaje por syslog |
| `[  179.878410] cloud-init[608]:` | el mismo texto inyectado al buffer del kernel |

Tres escritores independientes contra un único [[UART]], sin nadie coordinando el orden. **Un cable serie no tiene canales: tiene bytes** — que es exactamente el problema que este proyecto ya pagó del otro lado, cuando las letras de depuración de Kornelia salían después del marcador del protocolo y el cliente se las comía como CBOR.

Y los timestamps son de dos relojes: uptime del kernel y hora de pared. **Cuando un log se ve desordenado, la primera pregunta es quién puso ese timestamp.**

Eso último no era una duda del lector: era un concepto que faltaba, y apareció solo porque la práctica se corrió de verdad en vez de darse por buena.
