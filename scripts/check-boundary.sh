#!/usr/bin/env bash
# D23/D24: la frontera de portabilidad la verifica CI, no la buena voluntad.
#
# Son dos ejes y por eso son dos chequeos:
#   1. Ningun crate portable habla de una arquitectura  (eje arquitectura)
#   2. kernel-core no sabe como arranco la maquina      (eje entorno de arranque)
#
# Los comentarios no cuentan: la regla se puede documentar sin romperla.
set -euo pipefail
cd "$(dirname "$0")/.."

failed=0

# --- Eje 1: nada de arquitectura en los crates portables ---------------------
for crate in kernel-core boot-uefi; do
    [ -d "$crate/src" ] || continue
    leaks=$(grep -rnE 'target_arch|core::arch|asm!' "$crate/src/" \
            | grep -vE ':[[:space:]]*//' || true)
    if [ -n "$leaks" ]; then
        echo "FALLA: codigo especifico de arquitectura dentro de $crate (D23)"
        echo "$leaks"
        failed=1
    fi
done

# --- Eje 2: el nucleo no puede nombrar al entorno de arranque ----------------
# Si kernel-core supiera de UEFI, un dia habria que tocarlo para arrancar por
# device tree o por ROM. Ahi la frontera dejaria de existir (D24).
if [ -d kernel-core/src ]; then
    leaks=$(grep -rniE 'boot.uefi|boot_uefi|\buefi\b|efi_' kernel-core/src/ kernel-core/Cargo.toml \
            | grep -vE ':[[:space:]]*//|^kernel-core/src/[^:]*:[0-9]*:[[:space:]]*//|//!' || true)
    if [ -n "$leaks" ]; then
        echo "FALLA: kernel-core nombra al entorno de arranque (D24)"
        echo "$leaks"
        failed=1
    fi
fi

if [ "$failed" -ne 0 ]; then
    exit 1
fi
echo "OK: los crates portables no dependen ni de una arquitectura ni de como se arranco"
