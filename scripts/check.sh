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

failed=0
step() { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }
bad()  { printf '\033[31mFALLA: %s\033[0m\n' "$1"; failed=1; }

# --- 1. La frontera de portabilidad (D23/D24) -------------------------------
step "frontera"
./scripts/check-boundary.sh || bad "frontera"

# --- 2. El idioma del codigo (regla 6) --------------------------------------
# Los identificadores en ingles, los comentarios y los textos del UART en
# espanol. Se comprueba por la misma razon que la frontera: la regla estaba
# escrita y se rompio igual, dos veces.
step "idioma del codigo"
if command -v python3 >/dev/null; then
    python3 ./scripts/check-language.py || bad "idioma"
else
    bad "falta python3, que es lo que corre el chequeo de idioma"
fi

# --- 3. Tests del nucleo portable -------------------------------------------
# Solo kernel-core: los crates de arquitectura son binarios bare-metal y no se
# pueden correr en la maquina de desarrollo.
step "tests del nucleo portable"
cargo test -p kernel-core || bad "tests"

# --- 4. Las dos arquitecturas compilan (D22) --------------------------------
for arch in x86_64 aarch64; do
    step "compila $arch"
    cargo build --release -p "kernel-$arch" --target "$arch-unknown-uefi" || bad "compilar $arch"
done

# --- 5. Las dos arquitecturas ARRANCAN Y CONTESTAN (D22) --------------------
# Que compile no prueba nada: un puntero mal leido compila perfecto. Se bootean
# las dos en QEMU y se les habla en CBOR, que es como las va a usar un agente.
#
# El cliente hace las dos verificaciones de una: mira el banner de texto del
# arranque y despues pide `describe` por el protocolo.
#
# SKIP_QEMU=1 lo saltea, para una maquina sin firmware UEFI instalado.
if [ "${SKIP_QEMU:-0}" = "1" ]; then
    step "arranque y protocolo"
    echo "SALTEADO por SKIP_QEMU=1"
elif ! command -v python3 >/dev/null; then
    step "arranque y protocolo"
    bad "falta python3, que es lo que corre el cliente"
else
    for arch in x86_64 aarch64; do
        step "arranca $arch y contesta el protocolo"
        output=$(timeout 240 ./scripts/client.py --arch "$arch" --smp 4 --what memory,tables --memory --exec --cores --mailbox --doorbell --handler --during --permission --supervised --on-core 2>&1 || true)

        # Lo que tiene que haber dicho en el banner de texto.
        for expected in "arquitectura: $arch" "memoria:" "tablas:" \
                        "en memoria del kernel" "identity-mapeados" \
                        "faults: capturados. autotest ok" "acpi:" \
                        "el nucleo duerme entre pedidos" "-- CBOR --"; do
            grep -qFe "$expected" <<<"$output" || bad "$arch no dijo: $expected"
        done

        # Y lo que tiene que haber contestado por el protocolo.
        grep -qFe "ok=True" <<<"$output" || bad "$arch no contesto ok por el protocolo"
        grep -qFe "tables:" <<<"$output" || bad "$arch no devolvio la seccion tables"

        # Y que el lazo de memoria completo haya cerrado: reclamar, escribir,
        # leer de vuelta lo mismo, y todos los caminos de error.
        if ! grep -qFe "lazo de memoria completo: ok" <<<"$output"; then
            bad "$arch no cerro el lazo de memoria"
            printf '%s\n' "$output" | grep -E "FALLA:|mem\." | head -10
        fi

        # Y que haya corrido codigo maquina de verdad: uno que anda y uno que
        # falla, con la maquina viva despues del fault. Es la tesis del
        # proyecto, y es lo que no puede romperse en silencio.
        if ! grep -qFe "exec: ok" <<<"$output"; then
            bad "$arch no paso la prueba de exec"
            printf '%s\n' "$output" | grep -E "FALLA:|faulted|fault:" | head -10
        fi

        # Y que haya arrancado los otros nucleos: con -smp 4 tienen que quedar
        # tres andando, porque el cuarto es el que esta contestando.
        if ! grep -qFe "nucleos: ok (3 arrancados)" <<<"$output"; then
            bad "$arch no arranco los otros nucleos"
            printf '%s\n' "$output" | grep -E "FALLA:|id=" | head -10
        fi

        # Y el segundo canal: un pedido que entra por el buzon y se contesta
        # por el buzon, que es lo que promete D17.
        if ! grep -qFe "buzon: ok" <<<"$output"; then
            bad "$arch no cerro el segundo canal"
            printf '%s\n' "$output" | grep -E "FALLA:|buzon|listen" | head -10
        fi

        # Y el timbre: el agente lo toca con codigo maquina propio y el kernel
        # lo cuenta. Que el contador suba prueba que la interrupcion llego.
        if ! grep -qFe "timbre: ok" <<<"$output"; then
            bad "$arch no cerro el timbre del buzon"
            printf '%s\n' "$output" | grep -E "FALLA:|timbre|sono" | head -10
        fi

        # Y los handlers del agente: se instalan, se hacen sonar con codigo del
        # agente, y el kernel los llama. Que la cuenta suba lo prueba.
        if ! grep -qFe "handler: ok" <<<"$output"; then
            bad "$arch no cerro los handlers del agente"
            printf '%s\n' "$output" | grep -E "FALLA:|irq.install|atendida" | head -10
        fi

        # Y que el handler corra DURANTE un exec (D9, D29). La prueba no admite
        # interpretacion: el codigo del agente espera a su propio handler, asi
        # que si las interrupciones estuvieran cerradas no volveria nunca.
        if ! grep -qFe "handler durante exec: ok" <<<"$output"; then
            bad "$arch no atiende interrupciones durante un exec"
            printf '%s\n' "$output" | grep -E "FALLA:|durante" | head -10
        fi

        # Y que el permiso de memoria del agente lo haga cumplir el hardware
        # (D27). La prueba no es lo que el kernel dice, es que el kernel deje de
        # poder ejecutar ahi.
        if ! grep -qFe "permiso: ok" <<<"$output"; then
            bad "$arch no hace cumplir el permiso de la memoria del agente"
            printf '%s\n' "$output" | grep -E "FALLA:|user=|separacion" | head -10
        fi
        grep -qFe "separacion kernel/agente: la hace cumplir el hardware" <<<"$output" \
            || bad "$arch no informa que el hardware haga cumplir la separacion"

        # Y que un nucleo reclamado pueda recibir trabajo (D13). La prueba no
        # es lo que el kernel dice: el propio codigo del agente informa en que
        # nucleo esta corriendo, y tiene que dar uno distinto del que atiende.
        if ! grep -qFe "trabajo en otro nucleo: ok" <<<"$output"; then
            bad "$arch no le puede dar trabajo a un nucleo reclamado"
            printf '%s\n' "$output" | grep -E "FALLA:|corriendo|nucleo reclamado" | head -10
        fi

        # Y la mitad de arriba de D27: el agente declara con que privilegio
        # corre, y el hardware lo hace cumplir. La prueba no es lo que el kernel
        # dice: es que la instruccion que apaga las interrupciones —la unica de
        # la que el kernel no podia volver— vuelva como fault estructurado.
        if ! grep -qFe "supervisado: ok" <<<"$output"; then
            bad "$arch no hace cumplir el privilegio declarado"
            printf '%s\n' "$output" | grep -E "FALLA:|supervis|mode=" | head -10
        fi

        n=$(grep -cE '^ +0x[0-9a-f]{16} ' <<<"$output" || true)
        if [ "$n" -lt 5 ]; then
            bad "$arch devolvio solo $n regiones por el protocolo"
            printf '%s\n' "$output" | tail -5
        else
            echo "  $n regiones por el protocolo"
        fi
    done
fi

echo
if [ "$failed" -ne 0 ]; then
    printf '\033[31mHAY FALLAS\033[0m\n'
    exit 1
fi
printf '\033[32mTODO EN VERDE\033[0m\n'
