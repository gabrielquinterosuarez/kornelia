#!/usr/bin/env bash
# Arranca el kernel en QEMU x86_64. El cordón umbilical sale por stdout.
set -euo pipefail
cd "$(dirname "$0")/.."

# rustup se puede haber instalado sin tocar el PATH del shell. El cargo esta,
# pero `cargo` pelado no lo encuentra: se lo agrega aca para que este script
# ande sin preparativos.
if ! command -v cargo >/dev/null && [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi

if ! command -v cargo >/dev/null; then
    echo "FALTA cargo. Instalar con:" >&2
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    exit 1
fi

OVMF_CODE=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/OVMF/OVMF_VARS_4M.fd}

cargo build --release -p kernel-x86_64 --target x86_64-unknown-uefi
rm -rf target/esp-x86_64/EFI/BOOT && mkdir -p target/esp-x86_64/EFI/BOOT
cp target/x86_64-unknown-uefi/release/kernel.efi target/esp-x86_64/EFI/BOOT/BOOTX64.EFI

# BLOB=<archivo> lo copia a la particion como `blob.bin`, que es donde el kernel
# lo busca (D18). Sin esto no hay blob y el arranque lo dice: D20 lo permite
# explicitamente — el blob es borrable e ignorable.
rm -f target/esp-x86_64/blob.bin
if [ -n "${BLOB:-}" ]; then
    cp -f "$BLOB" target/esp-x86_64/blob.bin
fi

# Las variables UEFI tienen que ser escribibles: copia propia.
cp -f "$OVMF_VARS" target/OVMF_VARS-x86_64.fd

# El disco del que el cargador se trae el payload (D19). Se arma aca y no en el
# repo: es estado de la maquina de prueba, no fuente.
# DISK=<ruta> usa otro disco. Sirve para correr dos maquinas a la vez sin que
# se peleen: QEMU bloquea la imagen, y la segunda no arranca.
DISK=${DISK:-target/nvme-x86_64.img}
if [ ! -f "$DISK" ]; then
    dd if=/dev/zero of="$DISK" bs=1M count=16 status=none
fi
# PAYLOAD=<archivo> lo escribe al principio del disco, que es donde el cargador
# lo va a buscar.
if [ -n "${PAYLOAD:-}" ]; then
    dd if="$PAYLOAD" of="$DISK" bs=512 conv=notrunc status=none
fi

# El cordon umbilical, por donde sale y entra todo (D5).
#
# Con SOCKET=<ruta> el cable sale por un socket en vez de por esta terminal, y
# entonces **la maquina sobrevive a que el cliente se vaya**: se puede mandar
# algo largo, cortar la conexion y volver a buscar el resultado, que es
# justamente lo que D14 dice que el agente tiene que poder hacer. Sin esto el
# cliente es dueno de QEMU y al salir se lo lleva puesto.
#
# `server=on,wait=off` = QEMU escucha pero arranca igual sin que nadie este del
# otro lado; si no, la maquina no bootearia hasta que alguien se conecte.
#
# OJO: `-serial stdio` va SIN el prefijo `mon:`, y no es un olvido.
#
# Con `mon:` QEMU multiplexa su monitor sobre la misma terminal, y ese
# multiplexor se come el byte 0x01 (Ctrl-A) como escape junto con el que le
# sigue. Por este puerto viaja CBOR y, mas adelante, codigo maquina (D6): ahi
# 0x01 es un byte tan legitimo como cualquier otro, y perderlo corrompe el
# mensaje en silencio.
#
# El costo es que Ctrl-A X no sale. Se sale con Ctrl-C.

# QMP=<ruta> abre el canal de control de QEMU por un socket. Sirve para una
# sola cosa y vale la pena: **volcar la pantalla desde afuera**. Es la unica
# forma de comprobar que el kernel dibuja de verdad sin creerle al kernel, que
# es como se prueba todo lo demas en este proyecto.
#
# No es el monitor de texto sobre el serie (D26): es un canal aparte, asi que el
# cordon umbilical sigue crudo y nadie se come el 0x01.
QMP_ARGS=()
if [ -n "${QMP:-}" ]; then
    rm -f "$QMP"
    QMP_ARGS=(-qmp "unix:$QMP,server=on,wait=off")
fi

if [ -n "${SOCKET:-}" ]; then
    rm -f "$SOCKET"
    SERIAL=(-chardev "socket,id=cord,path=$SOCKET,server=on,wait=off" -serial chardev:cord)
else
    SERIAL=(-serial stdio)
fi
exec qemu-system-x86_64 \
    -machine q35 \
    `# -cpu max: el modelo por defecto (qemu64) NO tiene paginas de 1 GiB, que` \
    `# D12 necesita. Cualquier x86_64 de silicio las tiene desde ~2008, asi que` \
    `# el default de QEMU es mas austero que el hardware real, no al reves.` \
    -cpu max \
    `# D8: el IOMMU va encendido por defecto. Y el aparato 'edu' es un motor de` \
    `# DMA que se maneja con cuatro escrituras: es lo que permite comprobar que` \
    `# el IOMMU bloquea de verdad, en vez de creerle al kernel.` \
    -device intel-iommu \
    `# dma_mask: sin esto el aparato recorta la direccion de DMA a 28 bits, en` \
    `# silencio. Aca no se notaba —la RAM arranca en cero y el buffer del agente` \
    `# cae abajo de 256 MiB— pero en aarch64, donde la RAM arranca en 1 GiB,` \
    `# ningun destino podia llegar nunca. Se pone en las dos: una prueba que` \
    `# pasa porque las direcciones son chicas pasa por casualidad.` \
    -device edu,dma_mask=0xffffffffffff \
    `# Un disco NVMe, que es de donde D19 dice que el cargador se trae el resto:` \
    `# el blob entra en 256 KiB y el payload no tiene por que. Va vacio salvo que` \
    `# PAYLOAD= diga con que llenarlo, y se crea solo la primera vez.` \
    -drive file="$DISK",if=none,id=payload,format=raw \
    -device nvme,serial=kornelia,drive=payload \
    `# La placa de red, que es lo que le falta al agente para dejar de depender` \
    `# del cordon (D5). Va FORZADA y la MISMA que en aarch64, a proposito.` \
    `#` \
    `# Una placa de red no se puede manejar por clase como el NVMe: 01.08.02` \
    `# quiere decir "cualquier NVMe" y un solo driver los maneja a todos, pero` \
    `# 02.00.00 solo quiere decir "ethernet" y cada modelo tiene sus propios` \
    `# registros. O sea que hay que elegir modelo si o si, y elegir el mismo en` \
    `# las dos maquinas es lo que permite UN solo driver en las dos` \
    `# arquitecturas, que es la regla que no se rompe (D22).` \
    `#` \
    `# Sin esto QEMU pone su placa por defecto, que es distinta en cada maquina:` \
    `# e1000e en q35 y virtio-net en virt.` \
    `#` \
    `# hostfwd: el host puede mandarle datagramas al agente. Es lo que permite` \
    `# probar el camino de vuelta contra un par de verdad en vez de contra` \
    `# nosotros mismos. NETPORT= lo cambia, para correr las dos a la vez.` \
    -nic "user,model=e1000,hostfwd=udp::${NETPORT:-15555}-10.0.2.15:5555" \
    -drive if=pflash,format=raw,unit=0,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,unit=1,file=target/OVMF_VARS-x86_64.fd \
    -drive format=raw,file=fat:rw:target/esp-x86_64 \
    "${SERIAL[@]}" "${QMP_ARGS[@]}" -display none -no-reboot "$@"
