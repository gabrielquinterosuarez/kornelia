#!/usr/bin/env bash
# Arranca el kernel en QEMU x86_64. El cordón umbilical sale por stdout.
set -euo pipefail
cd "$(dirname "$0")/.."

OVMF_CODE=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/OVMF/OVMF_VARS_4M.fd}

cargo build --release -p kernel-x86_64 --target x86_64-unknown-uefi
rm -rf target/esp-x86_64/EFI/BOOT && mkdir -p target/esp-x86_64/EFI/BOOT
cp target/x86_64-unknown-uefi/release/kernel.efi target/esp-x86_64/EFI/BOOT/BOOTX64.EFI

# Las variables UEFI tienen que ser escribibles: copia propia.
cp -f "$OVMF_VARS" target/OVMF_VARS-x86_64.fd

exec qemu-system-x86_64 \
    -machine q35 \
    -drive if=pflash,format=raw,unit=0,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,unit=1,file=target/OVMF_VARS-x86_64.fd \
    -drive format=raw,file=fat:rw:target/esp-x86_64 \
    -serial stdio -display none -no-reboot "$@"
