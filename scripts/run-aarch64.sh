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

exec qemu-system-aarch64 \
    -machine virt -cpu cortex-a57 -m 512 \
    -drive if=pflash,format=raw,unit=0,readonly=on,file="$AAVMF_CODE" \
    -drive if=pflash,format=raw,unit=1,file=target/AAVMF_VARS-aarch64.fd \
    -drive format=raw,file=fat:rw:target/esp-aarch64 \
    -serial mon:stdio -display none -no-reboot "$@"
