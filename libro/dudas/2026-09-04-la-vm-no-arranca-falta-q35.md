---
tipo: duda
estado: cerrada
abierta: 2026-09-04
cerrada: 2026-09-04
sobre: "[[P00-Armar-la-VM-de-practicas]]"
---

# ¿La máquina arrancó? QEMU imprimió algo sobre `dynamic sysbus device type`

## Qué decía el libro

```bash
qemu-system-x86_64 \
  -enable-kvm -cpu host -m 2048 -smp 2 \
  ...
  -device intel-iommu \
  -nographic
```

## Qué pasó

```
qemu-system-x86_64: -device intel-iommu: Parameter 'driver' expects a dynamic
sysbus device type for the machine
```

Y volvió el prompt. **No arrancó nada**: [[QEMU]] se negó a empezar. Eso no se ve como un error de arranque —no hay pantalla negra ni kernel colgado— se ve como si el comando no hubiera hecho nada.

## Qué era

Faltaba **`-machine q35`**. Por omisión `qemu-system-x86_64` emula la máquina `pc`, que es el chipset i440fx de 1996, y ahí el [[IOMMU]] de Intel **no existe**: no es que no funcione, es que el [[Aparato|aparato]] no se puede ni instanciar.

La práctica estaba mal escrita: copié el `-device intel-iommu` de `scripts/run-x86_64.sh` **sin copiar el `-machine q35` que está tres líneas más arriba en ese mismo script** (`scripts/run-x86_64.sh:104#-machine q35`). El script del kernel siempre lo tuvo.

Verificado con los archivos del autor y `-snapshot` para no escribirle al disco: con q35 arranca hasta el login. Y de paso el arranque confirmó otra cosa del libro — `tsc: Detected 1991.992 MHz processor`, los mismos 1,992 GHz que había medido [[P02-Medir-el-reloj-y-las-latencias]].

Arreglado en [[P00-Armar-la-VM-de-practicas]]: la bandera está en la línea de arranque y en el script, tiene su fila en la tabla que explica cada pedazo, y el mensaje de error exacto está en la tabla de *qué mirar cuando no sale*. Se agregó también la variante con `intremap=on` y `kernel-irqchip=split`, que es la que va a hacer falta para remapeo de [[Interrupcion|interrupciones]].

## Lo que esta duda enseñó sobre el libro

**Copiar una bandera de un comando que funciona no es copiar el comando.** El resto de las prácticas tienen líneas de QEMU armadas de la misma manera; conviene correr cada una una vez antes de darla por buena, que es exactamente lo que el proyecto ya sabe de sus propias pruebas: *que compile no prueba nada*.

Y una segunda, sobre cómo se ve el fracaso: **un comando que no arranca se ve igual que uno que no hizo nada.** Por eso toda práctica declara *qué vas a ver si funciona* — sin esa sección, no había con qué comparar.
