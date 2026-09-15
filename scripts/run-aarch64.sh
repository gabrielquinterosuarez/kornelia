#!/usr/bin/env bash
# Arranca el kernel en QEMU aarch64. El cordón umbilical sale por stdout.
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

AAVMF_CODE=${AAVMF_CODE:-/usr/share/AAVMF/AAVMF_CODE.no-secboot.fd}
AAVMF_VARS=${AAVMF_VARS:-/usr/share/AAVMF/AAVMF_VARS.fd}

cargo build --release -p kernel-aarch64 --target aarch64-unknown-uefi
rm -rf target/esp-aarch64/EFI/BOOT && mkdir -p target/esp-aarch64/EFI/BOOT
cp target/aarch64-unknown-uefi/release/kernel.efi target/esp-aarch64/EFI/BOOT/BOOTAA64.EFI

# BLOB=<archivo> lo copia a la particion como `blob.bin`, que es donde el kernel
# lo busca (D18). Sin esto no hay blob y el arranque lo dice: D20 lo permite
# explicitamente — el blob es borrable e ignorable.
rm -f target/esp-aarch64/blob.bin
if [ -n "${BLOB:-}" ]; then
    cp -f "$BLOB" target/esp-aarch64/blob.bin
fi

cp -f "$AAVMF_VARS" target/AAVMF_VARS-aarch64.fd

# NO_ACPI=1 arranca la maquina **sin tablas de ACPI**, y entonces el firmware le
# pasa al kernel un device tree en su lugar. Es la unica forma de ejercitar el
# otro dialecto con el que una maquina se describe (deuda 3): las placas ARM
# embebidas no traen ACPI, y ahi el kernel tiene que poder averiguar lo mismo.
MACHINE=virt,iommu=smmuv3
if [ "${NO_ACPI:-0}" = "1" ]; then
    MACHINE=$MACHINE,acpi=off
fi

# El disco del que el cargador se trae el payload (D19). Se arma aca y no en el
# repo: es estado de la maquina de prueba, no fuente.
# DISK=<ruta> usa otro disco. Sirve para correr dos maquinas a la vez sin que
# se peleen: QEMU bloquea la imagen, y la segunda no arranca.
DISK=${DISK:-target/nvme-aarch64.img}
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
# SHOW=1 abre una ventana de verdad en vez de correr a ciegas. Sirve para ver
# con los ojos lo que el kernel escribe en la pantalla (D5) — que es lo que se
# va a ver en una PC de verdad sin puerto serie.
#
# Por omision va apagada porque el porton corre sin nadie mirando, y una ventana
# que se abre sola en cada prueba es una molestia.
if [ "${SHOW:-0}" = "1" ]; then
    DISPLAY_ARGS=(-display gtk)
else
    DISPLAY_ARGS=(-display none)
fi

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
exec qemu-system-aarch64 \
    `# D8: el IOMMU va encendido por defecto. Y el aparato 'edu' es un motor de` \
    `# DMA que se maneja con cuatro escrituras: es lo que permite comprobar que` \
    `# el IOMMU bloquea de verdad, en vez de creerle al kernel.` \
    -machine "$MACHINE" -cpu cortex-a57 -m 512 \
    `# dma_mask: sin esto el aparato recorta la direccion de DMA a 28 bits, en` \
    `# silencio. Como aca la RAM arranca en 1 GiB, ningun destino podia llegar` \
    `# nunca — y un DMA que no ocurre se ve igual que uno que el IOMMU bloqueo.` \
    -device edu,dma_mask=0xffffffffffff \
    `# Un disco NVMe, que es de donde D19 dice que el cargador se trae el resto:` \
    `# el blob entra en 256 KiB y el payload no tiene por que. Va vacio salvo que` \
    `# PAYLOAD= diga con que llenarlo, y se crea solo la primera vez.` \
    -drive file="$DISK",if=none,id=payload,format=raw \
    -device nvme,serial=kornelia,drive=payload \
    `# La misma placa de red que en x86_64, y por eso mismo: una placa de red no` \
    `# se puede manejar por clase —02.00.00 solo dice "ethernet"—, asi que hay` \
    `# que elegir modelo, y elegir el mismo es lo que permite un solo driver en` \
    `# las dos arquitecturas (D22). Sin esto QEMU pone virtio-net, que no se` \
    `# parece en nada a la e1000 que pone en q35.` \
    `#` \
    `# NETPORT= por si se corren las dos maquinas a la vez: si el puerto del host` \
    `# esta tomado, QEMU no arranca.` \
    -nic "user,model=e1000,hostfwd=udp::${NETPORT:-15556}-10.0.2.15:5555" \
    `# Una pantalla. La maquina 'virt' no trae ninguna por omision —a diferencia` \
    `# de q35, que trae VGA— asi que sin esto el firmware no informa framebuffer` \
    `# y el kernel se queda sin su segundo cable (D5).` \
    `#` \
    `# 'ramfb' es lo mas parecido a lo que hay en una placa de verdad para este` \
    `# proposito: un framebuffer que arma el firmware y entrega como una` \
    `# direccion, sin que haga falta ningun driver del lado del kernel.` \
    -device ramfb \
    -drive if=pflash,format=raw,unit=0,readonly=on,file="$AAVMF_CODE" \
    -drive if=pflash,format=raw,unit=1,file=target/AAVMF_VARS-aarch64.fd \
    -drive format=raw,file=fat:rw:target/esp-aarch64 \
    "${SERIAL[@]}" "${QMP_ARGS[@]}" "${DISPLAY_ARGS[@]}" -no-reboot "$@"
