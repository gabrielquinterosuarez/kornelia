---
tipo: practica
clase: construir
contra: linux
estado: pendiente
riesgo: ninguno
conceptos: []
---

# P00 · Armar la VM de prácticas

> [!success] Qué vas a ver si funciona
> Un Linux completo arrancando en tu terminal —sin ventana, por el cable serie— donde
> podés escribir `sudo rmmod` cualquier cosa y colgar el kernel sin consecuencias. Y un
> **snapshot** al que volver en dos segundos después de haberlo roto.

Esta es la máquina donde se hacen las prácticas de clase **romper**. La regla del libro es
que nada que pueda dejar una máquina inservible corre en tu Debian, y hay prácticas que
consisten exactamente en dejar una máquina inservible.

Que arranque **por el serie y sin ventana** no es solo comodidad: es la misma vista que vas
a tener de [[10-Un-kernel-cuyo-usuario-no-es-humano|Kornelia]], donde el cable serie es todo
lo que hay. Acostumbrarse ahora ayuda después.

## Lo que hace falta

```bash
sudo apt install -y qemu-system-x86 qemu-utils cloud-image-utils
```

`cloud-image-utils` trae `cloud-localds`, que arma el disquito de configuración con el que
la imagen de Debian se auto-configura en el primer arranque (usuario, clave, paquetes). Sin
eso habría que hacer una instalación entera a mano.

## Bajar la imagen

Las imágenes *cloud* de Debian son un disco ya instalado, sin entorno gráfico, de unos 350
MB. Son la forma más rápida de tener un Linux desechable.

```bash
mkdir -p ~/vm-practicas && cd ~/vm-practicas

curl -LO https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2
```

> Si esa URL cambia, el índice está en `https://cloud.debian.org/images/cloud/`. `trixie` es
> Debian 13; si querés otra versión, cambiá el nombre.

**El original no se toca.** Se crea un disco nuevo que lo usa como base y solo guarda las
diferencias, así podés tirar el disco de trabajo y volver a empezar sin bajar nada:

```bash
qemu-img create -f qcow2 -F qcow2 \
  -b debian-13-genericcloud-amd64.qcow2 practicas.qcow2 20G
```

Eso es *copy-on-write*: el archivo nuevo pesa unos kilobytes y crece solo con lo que
cambies. La misma idea que usa un `fork()` para no copiar la memoria de un proceso.

## La configuración del primer arranque

```bash
cat > seed.yaml <<'EOF'
#cloud-config
hostname: practicas
users:
  - name: gabriel
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    lock_passwd: false
    # Clave: "practicas". Es una VM desechable sin red entrante; no importa.
    plain_text_passwd: practicas
ssh_pwauth: true
package_update: true
packages:
  - build-essential
  - linux-headers-amd64
  - pciutils
  - binutils
  - bpftrace
  - trace-cmd
  - acpica-tools
  - kmod
EOF

cloud-localds seed.img seed.yaml
```

`linux-headers-amd64` y `build-essential` son lo que hace falta para **compilar un módulo de
kernel**, que es la práctica de [[49-Escribir-un-driver]].

## Arrancarla

```bash
qemu-system-x86_64 \
  -enable-kvm -cpu host -m 2048 -smp 2 \
  -drive file=practicas.qcow2,if=virtio \
  -drive file=seed.img,if=virtio,format=raw \
  -device intel-iommu \
  -netdev user,id=n0,hostfwd=tcp::2222-:22 -device virtio-net,netdev=n0 \
  -nographic
```

| Pedazo | Para qué |
|---|---|
| `-enable-kvm -cpu host` | Que el silicio corra el código en vez de emularlo. Sin esto va diez veces más lento, y no verías las capacidades reales de tu procesador. |
| `-smp 2` | Dos núcleos: hace falta para todo lo de la **Parte IX**. |
| `-device intel-iommu` | Un IOMMU emulado, para las prácticas de [[47-IOMMU-VT-d-y-SMMUv3]]. Igual que hacen los `scripts/run-*.sh` de Kornelia. |
| `hostfwd=tcp::2222-:22` | `ssh -p 2222 gabriel@localhost` desde otra terminal, cómodo para copiar archivos. |
| `-nographic` | Sin ventana: la consola sale por esta terminal. **Se sale con `Ctrl-A` y después `X`.** |

El primer arranque tarda un par de minutos (cloud-init instala los paquetes). Después
arranca en unos segundos.

> [!warning] Acá `Ctrl-A X` sí sirve, en Kornelia no
> Esta VM usa el multiplexor de QEMU, así que `Ctrl-A X` la cierra. Los scripts de Kornelia
> usan `-serial stdio` **crudo** a propósito (D26), porque el multiplexor se come el byte
> `0x01` y por ahí viaja CBOR: de Kornelia se sale con `Ctrl-C`. Dos máquinas, dos formas de
> salir; es fácil confundirse.

Para que el IOMMU se use de verdad, hay que pedírselo al kernel de la VM:

```bash
sudo sed -i 's/GRUB_CMDLINE_LINUX_DEFAULT="/&intel_iommu=on iommu=pt /' /etc/default/grub
sudo update-grub && sudo reboot
# despues del reinicio:
ls /sys/kernel/iommu_groups/     # tiene que haber algo
dmesg | grep -i dmar
```

## Lo que hace que esto sirva: snapshots

Antes de romper nada:

```bash
# con la VM apagada
qemu-img snapshot -c limpia practicas.qcow2
qemu-img snapshot -l practicas.qcow2       # listar
```

Y después de haberla dejado inservible:

```bash
qemu-img snapshot -a limpia practicas.qcow2   # volver
```

Y para una prueba de la que **no** querés que quede nada, agregá `-snapshot` a la línea de
QEMU: todo lo que escriba la VM se descarta al apagarla.

> [!tip] Esto es lo que un kernel no puede hacer
> Poder volver a un estado anterior con un comando es un lujo de las máquinas virtuales.
> Adentro de un kernel **el rollback real es imposible, no caro** (D7/D11): cuando el código
> del agente escribió en un registro de un aparato, no hay snapshot que lo deshaga. Por eso
> Kornelia devuelve el fault con lo que pasó en vez de prometer que va a arreglarlo. Tenerlo
> claro acá, donde sí se puede volver, hace más entendible por qué allá no.

## Un script para no repetir todo esto

```bash
cat > ~/vm-practicas/arrancar.sh <<'EOF'
#!/usr/bin/env bash
# La VM desechable del libro. Con -limpia vuelve al snapshot antes de arrancar.
set -euo pipefail
cd "$(dirname "$0")"
[[ "${1:-}" == "-limpia" ]] && qemu-img snapshot -a limpia practicas.qcow2
exec qemu-system-x86_64 \
  -enable-kvm -cpu host -m 2048 -smp 2 \
  -drive file=practicas.qcow2,if=virtio \
  -drive file=seed.img,if=virtio,format=raw \
  -device intel-iommu \
  -netdev user,id=n0,hostfwd=tcp::2222-:22 -device virtio-net,netdev=n0 \
  -nographic
EOF
chmod +x ~/vm-practicas/arrancar.sh
```

## Qué mirar cuando no sale

| Síntoma | Causa probable |
|---|---|
| `Could not access KVM kernel module` | Falta el módulo o el usuario no está en el grupo `kvm`. `sudo usermod -aG kvm $USER` y volver a entrar. En una máquina que ya es virtual, KVM anidado puede no estar. Sacá `-enable-kvm`: va lento pero anda. |
| Arranca y no aparece la consola | Falta `-nographic`, o la imagen no es *genericcloud* (las `generic` esperan pantalla). |
| No pide usuario nunca | `seed.img` no se adjuntó o el YAML tiene un error de indentación. cloud-init es muy quisquilloso. Mirá `sudo cloud-init status --long` adentro. |
| No hay `/sys/kernel/iommu_groups/` | Falta `intel_iommu=on` en la línea de comandos del kernel, o falta `-device intel-iommu` en QEMU. |
| Se queda en `Booting from Hard Disk...` | El snapshot al que volviste estaba corrupto, o el disco base se movió. `qemu-img info practicas.qcow2` dice a qué base apunta. |

## Anotaciones

*(Tuyas.)*
