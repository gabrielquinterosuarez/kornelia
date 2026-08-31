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

# --- 4. Las dos arquitecturas ARRANCAN (D22) --------------------------------
# Que compile no prueba nada: un puntero mal leido compila perfecto. Se bootean
# las dos en QEMU y se busca en el serie lo que tienen que decir.
#
# CHECK_SIN_QEMU=1 lo saltea, para una maquina sin firmware UEFI instalado.
if [ "${CHECK_SIN_QEMU:-0}" = "1" ]; then
    paso "arranque en QEMU"
    echo "SALTEADO por CHECK_SIN_QEMU=1"
else
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    for arq in x86_64 aarch64; do
        paso "arranca $arq en QEMU"
        # El kernel ya no se cuelga: se queda escuchando el UART. Por eso hay
        # que matarlo, y por eso el timeout no es una falla.
        timeout -s KILL 90 "./scripts/run-$arq.sh" </dev/null >"$tmp/$arq.log" 2>&1 || true
        salida=$(tr -d '\r' <"$tmp/$arq.log")

        for esperado in \
            "arquitectura: $arq" \
            "mapa de memoria:" \
            "descripcion de la maquina:" \
            "El cordon umbilical esta vivo." \
            "escuchando."
        do
            if ! grep -qF "$esperado" <<<"$salida"; then
                mal "$arq no dijo: $esperado"
            fi
        done

        # El mapa tiene que traer regiones de verdad, no venir vacio.
        n=$(grep -cE '^  0x[0-9a-f]{16}' <<<"$salida" || true)
        if [ "$n" -lt 5 ]; then
            mal "$arq reporto solo $n regiones de memoria"
        else
            echo "  $n regiones de memoria"
        fi

        # Y el mapa NO se tuvo que haber caido a la rama de error.
        if grep -qF "NO SE PUDO OBTENER" <<<"$salida"; then
            mal "$arq no pudo describir la maquina"
        fi
    done
fi

echo
if [ "$fallo" -ne 0 ]; then
    printf '\033[31mHAY FALLAS\033[0m\n'
    exit 1
fi
printf '\033[32mTODO EN VERDE\033[0m\n'
