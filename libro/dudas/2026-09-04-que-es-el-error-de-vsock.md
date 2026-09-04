---
tipo: duda
estado: cerrada
abierta: 2026-09-04
cerrada: 2026-09-04
sobre: "[[Maquina-virtual]]"
---

# ¿Y la última línea? `Failed to query local AF_VSOCK CID`

## Qué pasó

Al final del arranque de la VM, después de todo lo de cloud-init:

```
[ 1234.610029] systemd-ssh-generator[5398]: Failed to query local AF_VSOCK CID: Cannot assign requested address
```

Dos cosas la hacían sospechosa: dice `Failed`, y aparece **1054 segundos después** de que el arranque terminó.

## Qué era

Inofensivo, y con dos explicaciones distintas.

**El error.** `systemd-ssh-generator` intenta ofrecer SSH por [[Maquina-virtual|vsock]], que es un canal directo con el anfitrión sin pasar por la red. Su propio manual lo dice: *"If invoked in a VM with AF_VSOCK support, a socket-activated SSH per-connection service is bound to AF_VSOCK port 22"*. Esta VM no lleva el aparato que provee vsock, así que no hay dirección que consultar. El generador se calla y sigue.

**El timestamp.** Los generadores de systemd no corren solo al arrancar: **se re-ejecutan en cada `daemon-reload`**. Algo disparó uno a los ~20 minutos — un `apt`, un timer, o un `systemctl`. Eso explica el salto, y no era que el arranque hubiera tardado 20 minutos.

## En qué quedó

La respuesta a la pregunta era una línea. Lo que valía era **el concepto que la pregunta destapó**, y que no estaba en el libro: hay una forma de que un huésped hable con su anfitrión **sin red**.

Quedó escrito en [[Maquina-virtual]], sección *El canal que no es una red*: qué es un CID, por qué no hay ruteo, y que lo provee un aparato paravirtualizado que no existe en el mundo físico.

Y ahí apareció la conexión que hace que valga la pena: **vsock es la forma exacta del problema que plantea D5.** *El [[UART]] es el cordón umbilical, no el transporte; el agente escribe el transporte rápido.* Un cable serie a 115.200 baudios alcanza para hablar, no para mover un volcado de memoria. vsock es cómo se ve ese transporte rápido cuando la máquina es virtual — y **Kornelia no lo tiene**: el transporte rápido que D5 le deja al agente no lo escribió nadie todavía.

También quedó la fila en la tabla de síntomas de [[P00-Armar-la-VM-de-practicas]], y cómo habilitarlo con `-device vhost-vsock-pci,guest-cid=3` para quien quiera verlo andar.

## Lo que esta duda enseñó sobre el libro

**Un mensaje que dice `Failed` no siempre es un problema, y la única forma de saberlo es leer qué estaba intentando hacer** — en este caso, el manual del propio programa que se queja (`man systemd-ssh-generator`), que lo explicaba en la primera línea.

Y la segunda, sobre los logs: **el timestamp de una línea dice cuándo se imprimió, no cuándo empezó lo que la causó.** Un generador que corre a los 20 minutos no significa que el arranque durara 20 minutos. Es la misma familia que la lección de la duda anterior sobre los relojes mezclados.
