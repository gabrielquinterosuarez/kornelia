#!/usr/bin/env bash
# Arranca el kernel en QEMU aarch64. El cordón umbilical sale por stdout.
set -euo pipefail
cd "$(dirname "$0")/.."

AAVMF_CODE=${AAVMF_CODE:-/usr/share/AAVMF/AAVMF_CODE.no-secboot.fd}
AAVMF_VARS=${AAVMF_VARS:-/usr/share/AAVMF/AAVMF_VARS.fd}

cargo build --release -p kernel-aarch64 --target aarch64-unknown-uefi
rm -rf target/esp-aarch64/EFI/BOOT && mkdir -p target/esp-aarch64/EFI/BOOT
cp target/aarch64-unknown-uefi/release/kernel.efi target/esp-aarch64/EFI/BOOT/BOOTAA64.EFI

cp -f "$AAVMF_VARS" target/AAVMF_VARS-aarch64.fd

exec qemu-system-aarch64 \
    -machine virt -cpu cortex-a57 -m 512 \
    -drive if=pflash,format=raw,unit=0,readonly=on,file="$AAVMF_CODE" \
    -drive if=pflash,format=raw,unit=1,file=target/AAVMF_VARS-aarch64.fd \
    -drive format=raw,file=fat:rw:target/esp-aarch64 \
    -serial stdio -display none -no-reboot "$@"
