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

cp -f "$AAVMF_VARS" target/AAVMF_VARS-aarch64.fd

# OJO: `-serial stdio` va SIN el prefijo `mon:`, y no es un olvido.
#
# Con `mon:` QEMU multiplexa su monitor sobre la misma terminal, y ese
# multiplexor se come el byte 0x01 (Ctrl-A) como escape junto con el que le
# sigue. Por este puerto viaja CBOR y, mas adelante, codigo maquina (D6): ahi
# 0x01 es un byte tan legitimo como cualquier otro, y perderlo corrompe el
# mensaje en silencio.
#
# El costo es que Ctrl-A X no sale. Se sale con Ctrl-C.
exec qemu-system-aarch64 \
    `# D8: el IOMMU va encendido por defecto. Y el aparato 'edu' es un motor de` \
    `# DMA que se maneja con cuatro escrituras: es lo que permite comprobar que` \
    `# el IOMMU bloquea de verdad, en vez de creerle al kernel.` \
    -machine virt,iommu=smmuv3 -cpu cortex-a57 -m 512 \
    `# dma_mask: sin esto el aparato recorta la direccion de DMA a 28 bits, en` \
    `# silencio. Como aca la RAM arranca en 1 GiB, ningun destino podia llegar` \
    `# nunca — y un DMA que no ocurre se ve igual que uno que el IOMMU bloqueo.` \
    -device edu,dma_mask=0xffffffffffff \
    -drive if=pflash,format=raw,unit=0,readonly=on,file="$AAVMF_CODE" \
    -drive if=pflash,format=raw,unit=1,file=target/AAVMF_VARS-aarch64.fd \
    -drive format=raw,file=fat:rw:target/esp-aarch64 \
    -serial stdio -display none -no-reboot "$@"
