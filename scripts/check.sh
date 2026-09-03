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
# El codigo Y lo que el kernel dice, en ingles; los comentarios y la
# documentacion, en espanol. Se comprueba por la misma razon que la frontera: la
# regla estaba escrita y se rompio igual, dos veces.
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
    # Un payload conocido en el disco. Sin esto, leer un disco de ceros y recibir
    # ceros se ve **igual** que no leer nada: la misma moneda que ya se pago con
    # el IOMMU.
    payload_dir=$(mktemp -d)
    trap 'rm -rf "$payload_dir"' EXIT
    ./scripts/client.py --write-payload "$payload_dir/payload.bin" >/dev/null

    for arch in x86_64 aarch64; do
        step "arranca $arch y contesta el protocolo"
        output=$(PAYLOAD="$payload_dir/payload.bin" timeout 240 ./scripts/client.py --arch "$arch" --smp 4 --what memory,tables,cable --clock --msi --deadline --recover --memory --exec --cores --mailbox --doorbell --handler --during --permission --supervised --on-core --dma --nvme 2>&1 || true)

        # Lo que tiene que haber dicho en el banner de texto.
        for expected in "architecture: $arch" "memory:" "tables:" \
                        "in kernel memory" "identity-mapped" \
                        "faults: captured. selftest ok" "machine: acpi" \
                        "the core sleeps between requests" "-- CBOR --"; do
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
            printf '%s\n' "$output" | grep -E "FALLA:|user=|separation" | head -10
        fi
        grep -qFe "kernel/agent separation: the hardware enforces it" <<<"$output" \
            || bad "$arch no informa que el hardware haga cumplir la separacion"

        # Y que se le pueda escribir un driver a un aparato de verdad usando
        # **solo los once verbos**. Es la primera prueba de que la superficie
        # alcanza: no hay un verbo `disco`, hay memoria, permiso de DMA y
        # registros. Que el controlador conteste quien es prueba el camino
        # entero — leyo el pedido de una cola en RAM por DMA, el IOMMU lo dejo,
        # y escribio la respuesta donde se le dijo.
        if ! grep -qFe "nvme: ok" <<<"$output"; then
            bad "$arch no pudo manejar el controlador NVMe"
            printf '%s\n' "$output" | grep -E "FALLA:|controlador|nvme" | head -10
        fi
        grep -qFe "serie 'kornelia'" <<<"$output" \
            || bad "$arch no le hablo al disco que le pusimos"
        # Y que haya leido del disco lo que se escribio. Que devuelva el bloque
        # que se le pidio y no siempre el primero es parte de la prueba.
        grep -qFe "el bloque 0 trae el payload" <<<"$output" \
            || bad "$arch no leyo el payload del disco"
        grep -qFe "el bloque 2 es el bloque 2" <<<"$output" \
            || bad "$arch no devuelve el bloque que se le pide"

        # Y que no se haya perdido **ni un byte** del cable. Un pedido al que le
        # falta un byte se ve como una maquina colgada, y sin esto seria un
        # misterio: paso, y costo un rato encontrarlo.
        if ! grep -qFe "'dropped': 0" <<<"$output"; then
            bad "$arch perdio bytes del cable"
            printf '%s\n' "$output" | grep -E "cable:" | head -2
        fi

        # Y que el puerto serie en uso sea el que dice la maquina, no el
        # horneado (deuda 2). La prueba no es que lo informe: es que **todo lo
        # que sigue sale por ahi**, asi que si esa direccion fuera mala esta
        # linea no llegaria. Solo aarch64: en x86_64 el UART esta en puertos de
        # E/S y no hay a donde mudarse, cosa que el kernel tambien dice.
        if [ "$arch" = "aarch64" ]; then
            grep -qFe ", the machine says so" <<<"$output" \
                || bad "$arch no usa el puerto serie que informa la maquina"
        fi

        # Y que un nucleo reclamado pueda recibir trabajo (D13). La prueba no
        # es lo que el kernel dice: el propio codigo del agente informa en que
        # nucleo esta corriendo, y tiene que dar uno distinto del que atiende.
        if ! grep -qFe "trabajo en otro nucleo: ok" <<<"$output"; then
            bad "$arch no le puede dar trabajo a un nucleo reclamado"
            printf '%s\n' "$output" | grep -E "FALLA:|corriendo|nucleo reclamado" | head -10
        fi

        # Y el IOMMU (D8). La prueba no es lo que el kernel dice: se le pide a
        # un aparato de verdad que escriba en la memoria del agente **sin haberlo
        # declarado** y la memoria tiene que quedar intacta; declarado, la misma
        # escritura tiene que llegar; y al soltar el reclamo, dejar de llegar.
        #
        # Se exige en las dos, y con el mismo texto: VT-d en x86_64, SMMUv3 en
        # aarch64. Que el kernel diga "todavia no lo programo" dejo de alcanzar
        # el dia que hubo con que programarlo.
        # Y que un aparato de verdad dispare su interrupcion **escribiendo en
        # memoria** (MSI), que es como interrumpen los aparatos de hoy. La
        # prueba no le cree al kernel: el handler del agente deja una marca en
        # su propia memoria y se comprueba que aparezca.
        if ! grep -qFe "msi: ok" <<<"$output"; then
            bad "$arch no deja que un aparato dispare su interrupcion por escritura"
            printf '%s\n' "$output" | grep -E "FALLA:|marca|eligio" | head -6
        fi

        # Y el plazo que declara el agente (D18 tenia el mismo problema, pero
        # aca es el nucleo del protocolo el que se salva). La prueba no admite
        # interpretacion: se corre un bucle sin salida **sin nucleo**, o sea
        # justo donde antes se perdia la maquina. Si el plazo no se cumpliera,
        # el cliente se quedaria esperando para siempre.
        if ! grep -qFe "plazo: ok" <<<"$output"; then
            bad "$arch no hace cumplir el plazo que declara el agente"
            printf '%s\n' "$output" | grep -E "FALLA:|plazo|cortado" | head -6
        fi

        # Y que un nucleo cuyo codigo no vuelve se pueda recuperar. Las dos
        # mitades: el que no enmascara se corta y **vuelve a servir**, y el que
        # si enmascara no se puede cortar y el kernel lo dice en vez de mentir.
        if ! grep -qFe "recuperar un nucleo: ok" <<<"$output"; then
            bad "$arch no recupera un nucleo cuyo codigo no vuelve"
            printf '%s\n' "$output" | grep -E "FALLA:|recuperado|no lo pudo cortar" | head -6
        fi

        # Y el reloj (deuda 17). La prueba no es que informe un numero: es que
        # dos lecturas separadas por un tiempo conocido den ese tiempo.
        if ! grep -qFe "reloj: ok" <<<"$output"; then
            bad "$arch no mide tiempo con su propio reloj"
            printf '%s\n' "$output" | grep -E "FALLA:|reloj|marco" | head -5
        fi

        if ! grep -qFe "dma: ok" <<<"$output"; then
            bad "$arch no cerro la prueba del IOMMU"
            printf '%s\n' "$output" | grep -E "FALLA:|iommu|dma|memoria quedo" | head -10
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

    # --- 6. El blob: persistencia a traves del reinicio (D18, D19, D20) ----
    #
    # El firmware trae `blob.bin` de la particion y el kernel lo corre antes de
    # escuchar el cable. Se exigen **las tres mitades**:
    #
    #   1. que corra, y que el kernel pueda contar que corrio;
    #   2. que le pueda **pedir cosas al kernel** por la ventanilla que recibe al
    #      arrancar — sin eso el blob solo tiene la maquina cruda, que alcanza
    #      para un cargador (D19) y no para algo que quiera reclamar memoria;
    #   3. que un byte por el cable lo cancele — sin eso, un blob roto deja la
    #      maquina inutil en cada arranque y hay que sacar el disco.
    #
    # La (2) no se cree por lo que devuelve la ventanilla: se comprueba mirando
    # los reclamos de la maquina, que es donde tiene que haber quedado la huella
    # de un pedido que nunca paso por el cable.
    step "el blob se carga, le habla al kernel y se puede cancelar"
    blob_dir=$(mktemp -d)
    trap 'rm -rf "$blob_dir"' EXIT
    for arch in x86_64 aarch64; do
        ./scripts/client.py --arch "$arch" --write-blob "$blob_dir/blob.bin" >/dev/null

        output=$(BLOB="$blob_dir/blob.bin" timeout 240 ./scripts/client.py \
            --arch "$arch" --what claims 2>&1 || true)
        # Con reloj, la ventana se anuncia en milisegundos y no en vueltas: es
        # la diferencia entre un plazo que se puede cumplir y uno que no.
        if ! grep -qE "send any byte within [0-9]+ ms" <<<"$output"; then
            bad "$arch no dice cuanto dura la ventana de rescate"
            printf '%s\n' "$output" | grep -E "blob|mandar" | head -4
        fi
        if ! grep -qE "the blob returned, leaving 0x[0-9a-f]+" <<<"$output"; then
            bad "$arch no corrio el blob"
            printf '%s\n' "$output" | grep -E "blob|FALLA:" | head -5
        fi
        # 28672 = 0x7000, que es lo que pide el blob de prueba y nadie mas.
        if ! grep -qFe "'bytes': 28672" <<<"$output"; then
            bad "$arch: el blob no le pudo pedir memoria al kernel"
            printf '%s\n' "$output" | grep -E "blob|claims" | head -5
        fi

        output=$(BLOB="$blob_dir/blob.bin" timeout 240 ./scripts/client.py \
            --arch "$arch" --cancel-blob --what claims 2>&1 || true)
        # Y cancelado no tiene que quedar la huella: si el reclamo apareciera
        # igual, el que lo hizo seria otro y la prueba de arriba no probaria nada.
        if grep -qFe "'bytes': 28672" <<<"$output"; then
            bad "$arch: hay un reclamo del blob aunque el blob no corrio"
        fi
        if ! grep -qFe "cancelled: someone is on the other side" <<<"$output"; then
            bad "$arch no deja cancelar el blob por el cable"
            printf '%s\n' "$output" | grep -E "blob|FALLA:" | head -5
        fi
        # Y en los dos casos la maquina tiene que quedar contestando: un blob
        # que corre no puede dejar al kernel sin cordon (D5, D17).
        grep -qFe "ok=True" <<<"$output" || bad "$arch no contesta despues del blob"
    done

    # --- 7. Y la misma maquina, describiendose por el OTRO dialecto ---------
    #
    # Sin ACPI el firmware pasa un device tree, que es lo que traen las placas
    # ARM embebidas. Es un formato completamente distinto —un arbol de nodos con
    # nombres de texto, en vez de tablas con firma y checksum— y el kernel tiene
    # que poder averiguar lo mismo por los dos (P4, deuda 3).
    #
    # Se corre el IOMMU a proposito: es la prueba que usa **todo** lo que sale
    # de la descripcion junto —los nucleos, el controlador de interrupciones,
    # donde se configura PCIe y donde esta el IOMMU— y encima contra un aparato
    # de verdad. Si algo de eso saliera mal del arbol, esto no cierra.
    step "aarch64 se describe por device tree"
    output=$(NO_ACPI=1 timeout 240 ./scripts/client.py --arch aarch64 --no-acpi \
        --what cpus --memory --cores --dma 2>&1 || true)
    if ! grep -qFe "machine: device tree" <<<"$output"; then
        bad "aarch64 sin ACPI no lee el device tree"
        printf '%s\n' "$output" | grep -E "maquina:|FALLA:" | head -5
    fi
    for expected in "lazo de memoria completo: ok" "nucleos: ok" "dma: ok"; do
        if ! grep -qFe "$expected" <<<"$output"; then
            bad "aarch64 por device tree no cierra: $expected"
            printf '%s\n' "$output" | grep -E "FALLA:|maquina:" | head -8
        fi
    done
fi

echo
if [ "$failed" -ne 0 ]; then
    printf '\033[31mHAY FALLAS\033[0m\n'
    exit 1
fi
printf '\033[32mTODO EN VERDE\033[0m\n'
