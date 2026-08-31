#!/usr/bin/env bash
# El porton completo. Lo corre CI, y conviene correrlo a mano antes de commitear.
#
# Un solo script para las dos cosas a proposito: si CI hiciera algo distinto de
# lo que se puede correr localmente, la unica forma de saber si algo pasa seria
# empujar y esperar.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null && [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi

fallo=0
paso() { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }
mal()  { printf '\033[31mFALLA: %s\033[0m\n' "$1"; fallo=1; }

# --- 1. La frontera de portabilidad (D23/D24) -------------------------------
paso "frontera"
./scripts/check-frontera.sh || mal "frontera"

# --- 2. Tests del nucleo portable -------------------------------------------
# Solo kernel-core: los crates de arquitectura son binarios bare-metal y no se
# pueden correr en la maquina de desarrollo.
paso "tests del nucleo portable"
cargo test -p kernel-core || mal "tests"

# --- 3. Las dos arquitecturas compilan (D22) --------------------------------
for arq in x86_64 aarch64; do
    paso "compila $arq"
    cargo build --release -p "kernel-$arq" --target "$arq-unknown-uefi" || mal "compilar $arq"
done

# --- 4. Las dos arquitecturas ARRANCAN Y CONTESTAN (D22) --------------------
# Que compile no prueba nada: un puntero mal leido compila perfecto. Se bootean
# las dos en QEMU y se les habla en CBOR, que es como las va a usar un agente.
#
# El cliente hace las dos verificaciones de una: mira el banner de texto del
# arranque y despues pide `describe` por el protocolo.
#
# CHECK_SIN_QEMU=1 lo saltea, para una maquina sin firmware UEFI instalado.
if [ "${CHECK_SIN_QEMU:-0}" = "1" ]; then
    paso "arranque y protocolo"
    echo "SALTEADO por CHECK_SIN_QEMU=1"
elif ! command -v python3 >/dev/null; then
    paso "arranque y protocolo"
    mal "falta python3, que es lo que corre el cliente"
else
    for arq in x86_64 aarch64; do
        paso "arranca $arq y contesta el protocolo"
        salida=$(timeout 240 ./scripts/client.py --arch "$arq" --what memory,tables 2>&1 || true)

        # Lo que tiene que haber dicho en el banner de texto.
        for esperado in "arquitectura: $arq" "memoria:" "tablas:" "-- CBOR --"; do
            grep -qFe "$esperado" <<<"$salida" || mal "$arq no dijo: $esperado"
        done

        # Y lo que tiene que haber contestado por el protocolo.
        grep -qFe "ok=True" <<<"$salida" || mal "$arq no contesto ok por el protocolo"
        grep -qFe "tables:" <<<"$salida" || mal "$arq no devolvio la seccion tables"

        n=$(grep -cE '^ +0x[0-9a-f]{16} ' <<<"$salida" || true)
        if [ "$n" -lt 5 ]; then
            mal "$arq devolvio solo $n regiones por el protocolo"
            printf '%s\n' "$salida" | tail -5
        else
            echo "  $n regiones por el protocolo"
        fi
    done
fi

echo
if [ "$fallo" -ne 0 ]; then
    printf '\033[31mHAY FALLAS\033[0m\n'
    exit 1
fi
printf '\033[32mTODO EN VERDE\033[0m\n'
