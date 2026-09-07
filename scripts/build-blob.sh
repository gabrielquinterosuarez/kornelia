#!/usr/bin/env bash
# Compila `blob/` y deja un binario **plano** listo para la particion (D19/D20).
#
#     ./scripts/build-blob.sh x86_64   [salida]
#     ./scripts/build-blob.sh aarch64  [salida]
#
# Por que un binario plano y no un ejecutable: el kernel no carga formatos. Trae
# el archivo de la particion, lo copia a memoria que reclamo y **salta al byte
# cero**. Asi que lo que se necesita es exactamente el codigo, sin encabezados y
# con la entrada adelante — de eso se encarga `blob/blob.ld`.
set -euo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:?uso: build-blob.sh <x86_64|aarch64> [salida]}
OUT=${2:-target/blob-$ARCH.bin}

case "$ARCH" in
    x86_64)  TARGET=x86_64-unknown-none ;;
    aarch64) TARGET=aarch64-unknown-none ;;
    *) echo "arquitectura desconocida: $ARCH" >&2; exit 1 ;;
esac

if ! command -v cargo >/dev/null && [ -x "$HOME/.cargo/bin/cargo" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi
if ! command -v cargo >/dev/null; then
    echo "FALTA cargo. Instalar con:" >&2
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    exit 1
fi

# El target de bare-metal puede no estar. Se agrega solo, que es lo mismo que
# hace este script con el PATH: la alternativa es que el porton falle en una
# maquina nueva por algo que se arregla con un comando.
#
# NO va en `rust-toolchain.toml` a proposito. Ahi rustup **resincroniza el
# toolchain entero** en vez de agregar un componente, y eso falla en cualquier
# instalacion que tenga un conflicto viejo — con el sintoma de que deja de
# compilar todo, no solo el blob.
if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
    echo "agregando el target $TARGET..."
    rustup target add "$TARGET" >/dev/null
fi

# `-C relocation-model=pic`: el blob se carga en una direccion que recien se sabe
# en tiempo de ejecucion, asi que **no puede tener direcciones absolutas**. Con
# esto las referencias salen relativas al propio codigo y da igual donde caiga.
# Si algun dia aparece una absoluta, el enlazador lo dice en vez de dejar un
# blob que salta a cualquier lado.
#
# `--gc-sections` saca lo que no se usa, que en un blob que tiene que entrar en
# la particion no es cosmetico.
# `--no-pie` no es un detalle: rustc pasa `-pie` por omision, y un ejecutable
# independiente de la posicion se arma **con GOT** — una tabla de direcciones que
# rellena el enlazador dinamico al cargarlo. Aca no hay ninguno, asi que hay que
# pedir codigo independiente de la posicion (`pic`) SIN el andamiaje que supone
# que alguien lo va a terminar de armar.
RUSTFLAGS="-C relocation-model=pic \
-C link-arg=-T$(pwd)/blob/blob.ld \
-C link-arg=--gc-sections \
-C link-arg=--no-pie \
-C link-arg=-znoexecstack" \
    cargo build --release -p blob --target "$TARGET"

ELF="target/$TARGET/release/blob"

# Y de ahi salen los bytes. Se extraen con Python y no con `objcopy` porque el
# objcopy de esta maquina es el de su propia arquitectura y no sabe leer un ELF
# de la otra: extraer el segmento a mano anda para las dos por igual.
python3 scripts/flatten-elf.py "$ELF" "$OUT"
echo "$ARCH: $OUT ($(stat -c%s "$OUT") bytes)"
