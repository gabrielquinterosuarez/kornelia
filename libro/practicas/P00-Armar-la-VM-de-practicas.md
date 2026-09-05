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
> Un Linux completo arrancando en tu terminal —sin ventana, por el cable serie— donde podés escribir `sudo rmmod` cualquier cosa y colgar el kernel sin consecuencias. Y un **snapshot** al que volver en dos segundos después de haberlo roto.

Esta es la máquina donde se hacen las prácticas de clase **romper**. La regla del libro es que nada que pueda dejar una máquina inservible corre en tu Debian, y hay prácticas que consisten exactamente en dejar una máquina inservible.

Que arranque **por el serie y sin ventana** no es solo comodidad: es la misma vista que vas a tener de [[10-Un-kernel-cuyo-usuario-no-es-humano|Kornelia]], donde el cable serie es todo lo que hay. Acostumbrarse ahora ayuda después.

## Lo que hace falta

```bash
sudo apt install -y qemu-system-x86 qemu-utils cloud-image-utils
```

`cloud-image-utils` trae `cloud-localds`, que arma el disquito de configuración con el que la imagen de Debian se auto-configura en el primer arranque (usuario, clave, paquetes). Sin eso habría que hacer una instalación entera a mano.

## Bajar la imagen

Las imágenes *cloud* de Debian son un disco ya instalado, sin entorno gráfico. La de Debian 13 pesa **339 MB** para bajar y declara **3 GiB** de tamaño virtual. Son la forma más rápida de tener un Linux desechable.

```bash
mkdir -p ~/vm-practicas && cd ~/vm-practicas

curl -LO https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2
```

> Si esa URL cambia, el índice está en `https://cloud.debian.org/images/cloud/`. `trixie` es Debian 13; si querés otra versión, cambiá el nombre.

**El original no se toca.** Se crea un disco nuevo que lo usa como base y solo guarda las diferencias, así podés tirar el disco de trabajo y volver a empezar sin bajar nada:

```bash
qemu-img create -f qcow2 -F qcow2 \
  -b debian-13-genericcloud-amd64.qcow2 practicas.qcow2 20G
```

Eso es *copy-on-write*: el archivo nuevo pesa unos kilobytes y crece solo con lo que cambies. La misma idea que usa un `fork()` para no copiar la memoria de un proceso.

### ¿Ese `20G` me come 20 GB de disco?

**No.** Es el tamaño **virtual**: lo que la VM va a *creer* que mide su disco. El archivo arranca en 196 KiB y crece solo con lo que se escriba. Medido:

| | Ocupado en disco |
|---|---|
| El overlay recién creado, "de 20 GiB" | **196 KiB** |
| Después de escribirle 200 MiB adentro | 201 MB |
| Después de "borrar" esos 200 MiB | **201 MB — no se reduce** |

Esa tercera fila es la que importa y la que sorprende: **qcow2 crece y no se achica solo**. Borrar un archivo adentro de la VM libera espacio para la VM, no para tu disco. Para que sí lo libere hay que pedirle al disco que propague el descarte:

```bash
# en la linea de QEMU
-drive file=practicas.qcow2,if=virtio,discard=unmap
# y adentro de la VM, cada tanto
sudo fstrim -av
```

O, más simple para una máquina desechable: volver al snapshot, que descarta todo.

### El mínimo viable

**3 GiB**, porque es el tamaño virtual de la imagen de Debian que hace de base — el archivo que se baja pesa 339 MB, pero adentro declara 3 GiB. Se puede leer del encabezado qcow2 sin bajarla:

```bash
curl -sL -r 24-31 https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2 \
  | python3 -c "import sys,struct; print(struct.unpack('>Q', sys.stdin.buffer.read())[0] / 2**30, 'GiB')"
```

Y como el tamaño no cuesta nada hasta que se usa, **el mínimo no es el número que conviene poner**. Lo que va a ocupar de verdad son unos 2 a 3 GB: el sistema base más `build-essential` y `linux-headers`, que son lo que pesa. Con 3 GiB justos te queda sin lugar al instalar los paquetes. `8G` alcanza holgado; `20G` es un techo cómodo que sale igual de gratis.

> [!danger] Y si lo pongo **más chico** que la base, qemu-img no se queja
> Un overlay de 1 GiB sobre una base de 3 GiB se crea sin un solo aviso. Y queda roto: el disco que ve la VM está truncado, así que la tabla de particiones apunta a sectores que no existen. Leer más allá del corte da error:
>
> ```
> $ qemu-io -c "read -P 0x42 2G 4k" o1.qcow2
> read failed: Input/output error
> ```
>
> Es el mismo patrón que este proyecto ya pagó con el SMMU: **un límite que sobra puede ser tan inválido como uno que falta**, y el silicio —o acá la herramienta— lo acepta y lo reinterpreta en silencio en vez de rechazarlo. Ver [[Indice-de-sintomas]].
>
> Si no querés pensar el número, **omitilo**: `qemu-img create -f qcow2 -F qcow2 -b base.qcow2 practicas.qcow2` hereda el tamaño de la base, que es siempre correcto por construcción.

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

`linux-headers-amd64` y `build-essential` son lo que hace falta para **compilar un [[Modulo-de-kernel|módulo de kernel]]**, que es la práctica de [[49-Escribir-un-driver]].

## Arrancarla

```bash
qemu-system-x86_64 \
  -machine q35 -enable-kvm -cpu host -m 2048 -smp 2 \
  -drive file=practicas.qcow2,if=virtio \
  -drive file=seed.img,if=virtio,format=raw \
  -device intel-iommu \
  -netdev user,id=n0,hostfwd=tcp::2222-:22 -device virtio-net,netdev=n0 \
  -nographic
```

| Pedazo | Para qué |
|---|---|
| `-machine q35` | **Obligatorio acá.** Es la [[Maquina-virtual|máquina virtual]] que [[QEMU]] emula: por omisión usa `pc` (el chipset i440fx de 1996), y ahí el IOMMU de Intel **no existe**. Sin esto QEMU se niega a arrancar. Es lo mismo que hace `scripts/run-x86_64.sh` (`scripts/run-x86_64.sh:78#-machine q35`). |
| `-enable-kvm -cpu host` | Que el silicio corra el código en vez de emularlo. Sin esto va diez veces más lento, y no verías las capacidades reales de tu procesador. |
| `-smp 2` | Dos núcleos: hace falta para todo lo de la **Parte IX**. |
| `-device intel-iommu` | Un IOMMU emulado, para las prácticas de [[47-IOMMU-VT-d-y-SMMUv3]]. Igual que hacen los `scripts/run-*.sh` de Kornelia. |
| `hostfwd=tcp::2222-:22` | `ssh -p 2222 gabriel@localhost` desde otra terminal, cómodo para copiar archivos. |
| `-nographic` | Sin ventana: la consola sale por esta terminal. **Se sale con `Ctrl-A` y después `X`.** |

### Cómo sabés que el primer arranque terminó

El primer arranque tarda **unos tres minutos** —cloud-init instala los paquetes— y en el medio escupe muchísimo texto, incluidas las claves SSH de la máquina. Los siguientes arrancan en segundos.

La línea que dice que salió bien es esta, y conviene buscarla en vez de interpretar el resto:

```
Cloud-init v. 25.1.4 finished at Fri, 04 Sep 2026 15:43:30 +0000.
Datasource DataSourceNoCloud [seed=/dev/vdb].  Up 179.81 seconds
```

| Pedazo | Qué confirma |
|---|---|
| `DataSourceNoCloud [seed=/dev/vdb]` | Encontró tu `seed.img` y lo leyó: tu usuario, tu clave y los paquetes se aplicaron. Si dijera `DataSourceNone`, el disquito de configuración no llegó. |
| `finished` | Terminó, sin quedarse a mitad de camino. |
| `Up 179.81 seconds` | Cuánto tardó. La segunda vez son segundos. |

Las claves SSH se generan en el primer arranque de cualquier máquina, y se imprimen en la consola **a propósito**: es para poder verificar la huella antes de conectarse por primera vez. Son las públicas; no hay nada sensible ahí.

Después de eso, **Enter** y aparece el login: `gabriel` / `practicas`.

> [!tip] Y mirá el log, que enseña algo
> Las líneas salen **fuera de orden**: el `finished ... Up 179.81 seconds` aparece *antes* de los `Generating public/private rsa key pair`, que obviamente ocurrieron antes. No es un bug: son tres escritores independientes contra un único [[UART]], sin nadie coordinando.
>
> | Lo que ves | Por dónde salió |
> |---|---|
> | `-----BEGIN SSH HOST KEY KEYS-----` | cloud-init escribiendo **directo a la consola** |
> | `<14>Sep  4 15:43:30 cloud-init:` | el mismo mensaje por **syslog** (el `<14>` es la prioridad) |
> | `[  179.878410] cloud-init[608]:` | el mismo texto inyectado al **buffer del kernel** (`/dev/kmsg`) |
>
> **Un cable serie no tiene canales: tiene bytes.** Es el mismo problema que este proyecto pagó del otro lado — las letras de depuración de Kornelia salían después del marcador del protocolo y el cliente se las comía como CBOR. Ver [[Indice-de-sintomas]].
>
> Y los timestamps son de dos relojes distintos: `[  179.8]` es uptime del kernel, `Sep  4 15:43:30` es hora de pared. **Cuando un log se ve desordenado, la primera pregunta es quién puso ese timestamp.**

### Comprobar que quedó usable

Adentro de la VM, cuatro cosas:

```bash
cloud-init status --long        # tiene que decir status: done
id                              # gabriel, y en el grupo sudo
gcc --version                   # los paquetes se instalaron
ls /lib/modules/$(uname -r)/build   # y los headers, que es lo que pide un modulo
```

Y desde tu Debian, en otra terminal, que el puerto reenviado ande:

```bash
ssh -p 2222 gabriel@localhost
```


> [!warning] Acá `Ctrl-A X` sí sirve, en Kornelia no
> Esta VM usa el multiplexor de QEMU, así que `Ctrl-A X` la cierra. Los scripts de Kornelia usan `-serial stdio` **crudo** a propósito (D26), porque el multiplexor se come el byte `0x01` y por ahí viaja CBOR: de Kornelia se sale con `Ctrl-C`. Dos máquinas, dos formas de salir; es fácil confundirse.

> [!note] Si más adelante hace falta el IOMMU completo
> Lo de arriba alcanza para traducir [[DMA]], que es lo que se mira en las prácticas del [[IOMMU]]. Para **remapeo de [[Interrupcion|interrupciones]]** —lo que hace falta para pasarle un [[Aparato|aparato]] real a la VM— son dos cambios más:
>
> ```bash
> -machine q35,kernel-irqchip=split \
> -device intel-iommu,intremap=on
> ```
>
> Se comprueba en el arranque: `dmesg | grep DMAR-IR` tiene que decir `Queued invalidation will be enabled to support x2apic and Intr-remapping`.

Para que el IOMMU se use de verdad, hay que pedírselo al kernel de la VM:

```bash
sudo sed -i 's/GRUB_CMDLINE_LINUX_DEFAULT="/&intel_iommu=on iommu=pt /' /etc/default/grub
sudo update-grub && sudo reboot
# despues del reinicio:
ls /sys/kernel/iommu_groups/     # tiene que haber algo
dmesg | grep -i dmar
```

> [!note] Si algún día querés vsock
> Es un canal punto a punto con el anfitrión que **no usa la red**: sin IP, sin puertos reenviados, sin NAT. Se agrega con un aparato más:
>
> ```bash
> -device vhost-vsock-pci,guest-cid=3
> ```
>
> Necesita `/dev/vhost-vsock` en el anfitrión (en Debian 13 ya está, y es del grupo `kvm`). Con eso, `ssh vsock/3` entra desde el host y el mensaje del generador desaparece. No hace falta para nada de este libro; está acá porque el concepto sí importa — ver [[Maquina-virtual]].

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

Y para una prueba de la que **no** querés que quede nada, agregá `-snapshot` a la línea de QEMU: todo lo que escriba la VM se descarta al apagarla.

> [!tip] Esto es lo que un kernel no puede hacer
> Poder volver a un estado anterior con un comando es un lujo de las máquinas virtuales. Adentro de un kernel **el rollback real es imposible, no caro** (D7/D11): cuando el código del agente escribió en un registro de un aparato, no hay snapshot que lo deshaga. Por eso Kornelia devuelve el [[Fault|fault]] con lo que pasó en vez de prometer que va a arreglarlo. Tenerlo claro acá, donde sí se puede volver, hace más entendible por qué allá no.

## Un script para no repetir todo esto

```bash
cat > ~/vm-practicas/arrancar.sh <<'EOF'
#!/usr/bin/env bash
# La VM desechable del libro. Con -limpia vuelve al snapshot antes de arrancar.
set -euo pipefail
cd "$(dirname "$0")"
[[ "${1:-}" == "-limpia" ]] && qemu-img snapshot -a limpia practicas.qcow2
exec qemu-system-x86_64 \
  -machine q35 -enable-kvm -cpu host -m 2048 -smp 2 \
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
| `-device intel-iommu: Parameter 'driver' expects a dynamic sysbus device type for the machine` | Falta `-machine q35`. El IOMMU de Intel solo existe en esa máquina; en la de omisión el aparato no se puede ni instanciar. QEMU no arranca: no es que la VM falle después. |
| Dice `DataSourceNone` en vez de `DataSourceNoCloud` | El `seed.img` no se adjuntó, o le falta `format=raw` en el `-drive`. Sin eso no hay usuario ni clave y no podés entrar. |
| Terminó pero `gcc` no existe | cloud-init no pudo instalar los paquetes: casi siempre no había red. Mirá `cloud-init status --long` y `/var/log/cloud-init-output.log`. |
| `systemd-ssh-generator: Failed to query local AF_VSOCK CID: Cannot assign requested address` | **Inofensivo, ignoralo.** systemd intenta ofrecer SSH por [[Maquina-virtual#El canal que no es una red\|vsock]], que es un canal directo con el anfitrión sin pasar por la red. Esta VM no tiene ese aparato, así que no hay dirección de vsock que consultar. Para entrar usás el `hostfwd` del 2222. Aparece tarde en el log porque los generadores de systemd **se re-ejecutan en cada `daemon-reload`**, no solo al arrancar. |
| `Could not access KVM kernel module` | Falta el módulo o el usuario no está en el grupo `kvm`. `sudo usermod -aG kvm $USER` y volver a entrar. En una máquina que ya es virtual, KVM anidado puede no estar. Sacá `-enable-kvm`: va lento pero anda. |
| Arranca y no aparece la consola | Falta `-nographic`, o la imagen no es *genericcloud* (las `generic` esperan pantalla). |
| No pide usuario nunca | `seed.img` no se adjuntó o el YAML tiene un error de indentación. cloud-init es muy quisquilloso. Mirá `sudo cloud-init status --long` adentro. |
| No hay `/sys/kernel/iommu_groups/` | Falta `intel_iommu=on` en la línea de comandos del kernel, o falta `-device intel-iommu` en QEMU. |
| Se queda en `Booting from Hard Disk...` | El snapshot al que volviste estaba corrupto, o el disco base se movió. `qemu-img info practicas.qcow2` dice a qué base apunta. |

## Anotaciones

*(Tuyas.)*
