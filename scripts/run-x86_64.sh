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

# Las variables UEFI tienen que ser escribibles: copia propia.
cp -f "$OVMF_VARS" target/OVMF_VARS-x86_64.fd

# OJO: `-serial stdio` va SIN el prefijo `mon:`, y no es un olvido.
#
# Con `mon:` QEMU multiplexa su monitor sobre la misma terminal, y ese
# multiplexor se come el byte 0x01 (Ctrl-A) como escape junto con el que le
# sigue. Por este puerto viaja CBOR y, mas adelante, codigo maquina (D6): ahi
# 0x01 es un byte tan legitimo como cualquier otro, y perderlo corrompe el
# mensaje en silencio.
#
# El costo es que Ctrl-A X no sale. Se sale con Ctrl-C.
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
    -drive if=pflash,format=raw,unit=0,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,unit=1,file=target/OVMF_VARS-x86_64.fd \
    -drive format=raw,file=fat:rw:target/esp-x86_64 \
    -serial stdio -display none -no-reboot "$@"
