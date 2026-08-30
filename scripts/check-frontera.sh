#!/usr/bin/env bash
# D23: la frontera de portabilidad la verifica CI, no la buena voluntad.
# Falla si aparece código específico de arquitectura en el crate portable.
# Los comentarios no cuentan: la regla se puede documentar sin romperla.
set -euo pipefail
cd "$(dirname "$0")/.."

fugas=$(grep -rnE 'target_arch|core::arch|asm!' kernel-core/src/ \
        | grep -vE ':[[:space:]]*//' || true)

if [ -n "$fugas" ]; then
    echo "FALLA: codigo especifico de arquitectura dentro de kernel-core (D23)"
    echo "$fugas"
    exit 1
fi
echo "OK: kernel-core no depende de ninguna arquitectura"
