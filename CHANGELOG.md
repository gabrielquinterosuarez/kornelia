# Historial

Formato: lo que cambió y **por qué importa**, no la lista de commits. El detalle
de cada decisión vive en [`docs/DISENO.md`](docs/DISENO.md).

## 0.1.0 — sin publicar

La primera versión que se puede mostrar. Es un **experimento**, no un sistema
operativo de propósito general, y conviene leer la sección *Qué NO hay* del
README antes que ésta.

### El kernel

- Arranca por **UEFI en x86_64 y aarch64**, le toma la máquina al firmware y
  habla **CBOR** por el cordón umbilical (D6). Las dos arquitecturas en verde
  desde el primer commit, y la frontera la verifica CI (D22/D23).
- **Los once verbos**, enteros en las dos: `describe`, `mem.claim`, `mem.read`,
  `mem.write`, `core.claim`, `exec`, `irq.install`, `irq.install_raw`,
  `dma.allow`, `release`, `listen`.
- **Los faults vuelven como datos** (P5): causa, dirección y registros. Ni
  siquiera destruyendo el puntero de pila se pierde la máquina.
- **El agente declara con qué privilegio corre** (D27) y **cuánto puede tardar**.
  El kernel ofrece los dos modos y no elige.
- **El IOMMU arranca encendido y vacío** (D8): sin declarar nada, ningún aparato
  llega a la memoria. VT-d y SMMUv3.
- La máquina se describe por **ACPI y por device tree**, y cuál usar no lo elige
  el kernel: es cuál dejó el firmware.

### Lo que el agente escribió encima

No es el kernel (D4): usa sólo los once verbos.

- Driver de **NVMe** que lee y escribe, así que lo grabado sobrevive al reinicio.
- Driver de **red** (Intel 82540EM) que manda y recibe paquetes.
- **El transporte de D5**: el kernel contesta el protocolo por red, por un buzón
  que el agente le entregó con `listen`.

### El blob (D19/D20)

- `blob/` **se compila** a binario plano para las dos arquitecturas.
- Adentro tiene un **cargador NVMe completo**: trae del disco un programa que no
  está en la partición y lo corre.
- Se puede **cancelar desde el cable** en los primeros dos segundos, que es lo
  que hace que un blob roto no deje la máquina inútil en cada arranque.

### Sobre la estabilidad

**Nada de esto es estable todavía.** La superficie de once verbos es lo más
firme que hay —está pensada para no crecer— pero el formato de las respuestas,
el acuerdo del buzón y el formato del payload en disco pueden cambiar sin aviso
mientras la versión empiece con cero.
