#!/usr/bin/env python3
"""Cliente del protocolo: arranca el kernel en QEMU y le habla en CBOR.

Es el primer programa que usa el kernel como lo va a usar un agente, y sirve
para dos cosas: mirar de verdad lo que contesta, y que el porton pueda probar
el protocolo de punta a punta.

Trae su propio CBOR en unas 60 lineas, a proposito: si usara una biblioteca,
un desacuerdo entre el kernel y esa biblioteca se leeria como "el kernel esta
bien" cuando quiza los dos esten mal de la misma manera.

    ./scripts/client.py                          # el indice (D16)
    ./scripts/client.py --what memory            # el mapa de memoria
    ./scripts/client.py --what tables --raw      # mostrando los bytes crudos
    ./scripts/client.py --arch aarch64
    ./scripts/client.py --memory                 # el lazo: claim, write, read, release
"""

import argparse
import json
import os
import select
import socket
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Donde sale el cable cuando la maquina corre por su cuenta. Un default para que
# levantarla y engancharse no pidan ponerse de acuerdo en una ruta.
DEFAULT_SOCKET = "/tmp/kornelia.sock"
MARKER = b"-- CBOR --"


# --------------------------------------------------------------------------
# CBOR minimo
# --------------------------------------------------------------------------

class Incomplete(Exception):
    """Todavia no llegaron todos los bytes."""


def enc_head(major, value):
    if value < 24:
        return bytes([major << 5 | value])
    for cap, ai, length in ((0x100, 24, 1), (0x10000, 25, 2),
                              (0x100000000, 26, 4), (1 << 64, 27, 8)):
        if value < cap:
            return bytes([major << 5 | ai]) + value.to_bytes(length, "big")
    raise ValueError("valor fuera de rango")


def enc(v):
    if isinstance(v, bool):
        return b"\xf5" if v else b"\xf4"
    if v is None:
        return b"\xf6"
    if isinstance(v, int):
        return enc_head(0, v)
    if isinstance(v, bytes):
        return enc_head(2, len(v)) + v
    if isinstance(v, str):
        b = v.encode()
        return enc_head(3, len(b)) + b
    if isinstance(v, list):
        return enc_head(4, len(v)) + b"".join(enc(x) for x in v)
    if isinstance(v, dict):
        return enc_head(5, len(v)) + b"".join(enc(k) + enc(x) for k, x in v.items())
    raise TypeError(f"no se como codificar {type(v)}")


def dec(b, i=0):
    """Devuelve (valor, posicion_siguiente). Levanta Incompleto si falta."""
    if i >= len(b):
        raise Incomplete
    ib = b[i]
    major, ai = ib >> 5, ib & 0x1F
    i += 1

    if ai < 24:
        value = ai
    elif ai in (24, 25, 26, 27):
        length = {24: 1, 25: 2, 26: 4, 27: 8}[ai]
        if i + length > len(b):
            raise Incomplete
        value = int.from_bytes(b[i:i + length], "big")
        i += length
    else:
        raise ValueError(f"cbor invalido: 0x{ib:02x}")

    if major == 0:
        return value, i
    if major == 1:
        return -1 - value, i
    if major in (2, 3):
        if i + value > len(b):
            raise Incomplete
        raw = b[i:i + value]
        return (raw if major == 2 else raw.decode()), i + value
    if major == 4:
        output = []
        for _ in range(value):
            x, i = dec(b, i)
            output.append(x)
        return output, i
    if major == 5:
        output = {}
        for _ in range(value):
            k, i = dec(b, i)
            v, i = dec(b, i)
            output[k] = v
        return output, i
    if major == 7:
        return {20: False, 21: True, 22: None}.get(value, f"simple({value})"), i
    raise ValueError(f"tipo mayor no soportado: {major}")


# --------------------------------------------------------------------------
# Hablar con el kernel
# --------------------------------------------------------------------------

# Lo que el kernel dice cuando abre la ventana de rescate del blob (D18).
RESCUE_PROMPT = b"send any byte"


def read_until_marker(proc, timeout, show, cancel_blob=False):
    """Consume la salida de texto del arranque hasta que empieza el binario.

    Con `cancel_blob`, manda un byte **cuando el kernel avisa** que abrio la
    ventana de rescate. No sirve mandarlo antes: el firmware usa el serie como
    su propia consola durante el arranque y se lo come — el rescate parecia no
    andar y el byte nunca habia llegado al kernel.
    """
    buf = b""
    cancelled = False
    while MARKER not in buf:
        ready, _, _ = select.select([proc.stdout], [], [], timeout)
        if not ready:
            raise TimeoutError("el kernel nunca llego a la marca del protocolo")
        c = proc.stdout.read(1)
        if not c:
            raise EOFError("QEMU se cerro antes de arrancar el protocolo")
        buf += c
        if cancel_blob and not cancelled and RESCUE_PROMPT in buf:
            proc.stdin.write(b"\x00")
            proc.stdin.flush()
            cancelled = True
    # El resto del renglon de la marca.
    while not buf.endswith(b"\n"):
        buf += proc.stdout.read(1)

    if show:
        text = buf.decode(errors="replace").replace("\r", "")
        for line in text.splitlines():
            if line.strip():
                print(f"  {line}")
    return buf


def ask(proc, message, timeout):
    """Manda un pedido y espera una respuesta completa."""
    proc.stdin.write(enc(message))
    proc.stdin.flush()

    buf = b""
    while True:
        ready, _, _ = select.select([proc.stdout], [], [], timeout)
        if not ready:
            raise TimeoutError(f"sin respuesta (llegaron {len(buf)} bytes: {buf.hex()})")
        chunk = proc.stdout.read(1)
        if not chunk:
            raise EOFError("QEMU se cerro")
        buf += chunk
        try:
            value, end = dec(buf)
            return value, buf[:end]
        except Incomplete:
            continue


# --------------------------------------------------------------------------
# Presentacion
# --------------------------------------------------------------------------

def human(n):
    for unit, size in (("GiB", 1 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)):
        if n >= size and n % size == 0:
            return f"{n // size} {unit}"
    return f"{n} B"


def show(load):
    if "memory" in load and isinstance(load["memory"], list):
        regions = load["memory"]
        print(f"\n  memory: {len(regions)} regiones")
        for r in regions:
            raw = f"  (tipo crudo {r[3]})" if len(r) > 3 else ""
            print(f"    {r[0]:#018x}  {human(r[1]):>10}  {r[2]}{raw}")
        load = {k: v for k, v in load.items() if k != "memory"}

    for key, value in load.items():
        print(f"\n  {key}: {value}")


def test_memory(proc, timeout):
    """El lazo completo: reclamar, escribir, leer de vuelta, soltar.

    Es la primera vez que el agente no solo mira la maquina sino que la usa.
    """
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        print(f"  {verb:<10} {'ok ' if ok else 'ERROR'} {load}")
        return ok, load

    # 1. Reclamar 4 KiB alineados a 4 KiB.
    ok, c = ask_verb(1, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        return 1
    h, start = c["handle"], c["start"]
    if start % 4096 != 0:
        failures.append(f"no respeto la alineacion: {start:#x}")
    if c["kind"] != "free":
        failures.append(f"entrego memoria que no es libre: {c['kind']}")
    # La cacheabilidad sale de lo que informo el firmware region por region, no
    # de deducirla de la clase (deuda 11). RAM comun tiene que venir cacheable.
    if c["caching"] != "write-back":
        failures.append(f"la RAM no vino cacheable: {c['caching']}")

    # 2. Escribir un patron reconocible.
    pattern = bytes([0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x11, 0x22, 0x33])
    ok, w = ask_verb(2, "mem.write", {"handle": h, "off": 16, "bytes": pattern})
    if not ok or w.get("written") != len(pattern):
        failures.append("la escritura no informo lo que se escribio")

    # 3. Leerlo de vuelta del mismo lugar.
    ok, rd = ask_verb(3, "mem.read", {"handle": h, "off": 16, "len": len(pattern)})
    if not ok or rd.get("bytes") != pattern:
        failures.append(f"lo leido no es lo escrito: {rd}")

    # 4. Fuera del reclamo tiene que fallar, no leer memoria ajena.
    ok, e = ask_verb(4, "mem.read", {"handle": h, "off": 4090, "len": 16})
    if ok or e.get("error") != "out-of-bounds":
        failures.append("dejo leer fuera del reclamo")

    # 5. La memoria del kernel no se entrega.
    ok, e = ask_verb(5, "mem.claim", {"at": start, "bytes": 4096})
    if ok or e.get("error") != "already-claimed":
        failures.append("dejo reclamar dos veces lo mismo")

    # 6. Lo reclamado se ve en describe (D14).
    ok, d = ask_verb(6, "describe", {"what": ["claims"]})
    if not ok or not any(x["handle"] == h for x in d.get("claims", [])):
        failures.append("el reclamo no aparece en describe")

    # 7. Soltarlo, y que deje de existir.
    ok, _ = ask_verb(7, "release", {"handle": h})
    if not ok:
        failures.append("no se pudo soltar")
    ok, e = ask_verb(8, "mem.read", {"handle": h, "len": 4})
    if ok or e.get("error") != "no-such-handle":
        failures.append("el handle sigue vivo despues de soltarlo")

    # Y la otra mitad de la deuda 11: que la cacheabilidad **distinga**. Si todo
    # viniera con la misma etiqueta, el dato no vendria de la maquina — vendria
    # de un valor fijo, y esta prueba pasaria igual sin haber leido nada.
    ok, d = ask_verb(9, "describe", {"what": ["pcie"]})
    if ok and d["pcie"]:
        ok, mmio = ask_verb(10, "mem.claim", {"at": d["pcie"]["base"], "bytes": 4096})
        if ok:
            print(f"    y los registros de PCIe: kind={mmio['kind']}, "
                  f"caching={mmio['caching']}")
            if mmio["caching"] == "write-back":
                failures.append("los registros de un aparato vinieron cacheables")
            ask_verb(11, "release", {"handle": mmio["handle"]})

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  lazo de memoria completo: ok")
    return 0


# Codigo maquina escrito a mano. Son los dos programas mas chicos que sirven
# para probar las dos salidas de `exec`: volver bien y fallar.
PROGRAMS = {
    "x86_64": {
        # mov rax, 0x00C0FFEE ; ret
        "ok": bytes([0x48, 0xC7, 0xC0, 0xEE, 0xFF, 0xC0, 0x00, 0xC3]),
        # mov rax, 0xBEEF ; ret — otro programa, para distinguir "se grabo lo
        # nuevo" de "quedo lo que ya estaba".
        "otro": bytes([0x48, 0xC7, 0xC0, 0xEF, 0xBE, 0x00, 0x00, 0xC3]),
        # mov rax, 0x0000400000000000 ; mov [rax], rax ; ret
        "falla": bytes([0x48, 0xB8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00,
                        0x48, 0x89, 0x00, 0xC3]),
        # mov rsp, 0x400000000000 ; mov rax, 0x400000000000 ; mov [rax], rax ; ret
        # Deja el puntero de pila apuntando a memoria que no existe y despues
        # falla. Sin la IST, el CPU apilaria el marco de excepcion ahi y eso
        # escalaria a triple fault.
        "pila_rota": bytes([0x48, 0xBC, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00,
                            0x48, 0xB8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00,
                            0x48, 0x89, 0x00, 0xC3]),
        "registro": "rax",
    },
    "aarch64": {
        # movz x0, #0xFFEE ; movk x0, #0xC0, lsl #16 ; ret
        "ok": bytes([0xC0, 0xFD, 0x9F, 0xD2, 0x00, 0x18, 0xA0, 0xF2,
                     0xC0, 0x03, 0x5F, 0xD6]),
        # movz x0, #0xBEEF ; ret — otro programa, para distinguir "se grabo lo
        # nuevo" de "quedo lo que ya estaba".
        "otro": bytes([0xE0, 0xDD, 0x97, 0xD2, 0xC0, 0x03, 0x5F, 0xD6]),
        # movz x9, #0x4000, lsl #32 ; str x9, [x9] ; ret
        "falla": bytes([0x09, 0x00, 0xC8, 0xD2, 0x29, 0x01, 0x00, 0xF9,
                        0xC0, 0x03, 0x5F, 0xD6]),
        # movz x9, #0x4000, lsl #32 ; mov sp, x9 ; str x9, [x9] ; ret
        # Deja el puntero de pila apuntando a memoria que no existe y despues
        # falla. Sin SP_EL0 aparte, la excepcion se apilaria ahi y no habria
        # nada que capturar.
        "pila_rota": bytes([0x09, 0x00, 0xC8, 0xD2, 0x3F, 0x01, 0x00, 0x91,
                            0x29, 0x01, 0x00, 0xF9, 0xC0, 0x03, 0x5F, 0xD6]),
        "registro": "x0",
    },
}


# Cuanta memoria le pide al kernel el blob de prueba. Es un numero raro a
# proposito: asi, al ver los reclamos desde afuera, no hay duda de quien lo pidio.
BLOB_CLAIM = 0x7000

# Donde caen las tres partes del blob dentro del archivo. El codigo va primero
# porque el kernel salta al byte cero; lo demas son datos suyos, y las
# direcciones las calcula en runtime sumando a lo que recibe en el primer
# argumento (el kernel le pasa ahi su propia direccion).
BLOB_REQUEST_AT = 64
BLOB_REPLY_AT = 256
BLOB_REPLY_CAP = 256


def blob_program(arch, request_len):
    """El codigo del blob: le pide algo al kernel y devuelve el largo de la respuesta.

    Recibe en el primer registro de argumento su propia direccion y en el
    segundo la ventanilla del protocolo — cuales son esos dos registros lo dice
    la maquina en `describe exec arguments` (P4). La ventanilla es una funcion
    comun, porque el blob corre privilegiado y en el mismo espacio: no hay
    trampa, hay una llamada.
    """
    if arch == "x86_64":
        # rcx = base, rdx = ventanilla. **No rdi/rsi**: el kernel se compila para
        # UEFI, donde la ABI de C es la de Windows. Esa misma ABI pide dos cosas
        # mas que en Linux no hacen falta: 32 bytes de "shadow space" que reserva
        # el que llama, y la pila alineada a 16 en el `call`.
        return (
            bytes([0x49, 0x89, 0xD2])                      # mov r10, rdx  (ventanilla)
            + bytes([0x48, 0x89, 0xC8])                    # mov rax, rcx  (base)
            + bytes([0x48, 0x89, 0xE3])                    # mov rbx, rsp  (para volver)
            + bytes([0x48, 0x83, 0xE4, 0xF0])              # and rsp, -16
            + bytes([0x48, 0x83, 0xEC, 0x20])              # sub rsp, 32   (shadow space)
            + bytes([0x48, 0x8D, 0x88]) + BLOB_REQUEST_AT.to_bytes(4, "little")   # lea rcx,[rax+..]
            + bytes([0xBA]) + request_len.to_bytes(4, "little")                   # mov edx, len
            + bytes([0x4C, 0x8D, 0x80]) + BLOB_REPLY_AT.to_bytes(4, "little")     # lea r8,[rax+..]
            + bytes([0x41, 0xB9]) + BLOB_REPLY_CAP.to_bytes(4, "little")          # mov r9d, cap
            + bytes([0x41, 0xFF, 0xD2])                    # call r10
            + bytes([0x48, 0x89, 0xDC])                    # mov rsp, rbx
            + bytes([0xC3])                                # ret (deja rax como vino)
        )

    # x0 = base, x1 = ventanilla. Hay que guardar x30: `blr` lo pisa, y por ahi
    # es por donde el blob vuelve al kernel.
    def word(w):
        return w.to_bytes(4, "little")

    return (
        word(0xF81F0FFE)                                   # str x30, [sp, #-16]!
        + word(0xAA0103E9)                                 # mov x9, x1
        + word(0xAA0003E8)                                 # mov x8, x0
        + word(0x91000100 | (BLOB_REQUEST_AT << 10))       # add x0, x8, #req
        + word(0xD2800001 | (request_len << 5))            # mov x1, #len
        + word(0x91000102 | (BLOB_REPLY_AT << 10))         # add x2, x8, #reply
        + word(0xD2800003 | (BLOB_REPLY_CAP << 5))         # mov x3, #cap
        + word(0xD63F0120)                                 # blr x9
        + word(0xF84107FE)                                 # ldr x30, [sp], #16
        + word(0xD65F03C0)                                 # ret (deja x0 como vino)
    )


def blob_image(arch):
    """El blob.bin entero: codigo, el pedido ya armado, y lugar para la respuesta."""
    request = enc([900, "mem.claim", {"bytes": BLOB_CLAIM, "align": 4096}])
    code = blob_program(arch, len(request))
    assert len(code) <= BLOB_REQUEST_AT, f"el codigo pisa el pedido: {len(code)}"
    assert BLOB_REQUEST_AT + len(request) <= BLOB_REPLY_AT, "el pedido pisa la respuesta"

    image = bytearray(BLOB_REPLY_AT + BLOB_REPLY_CAP)
    image[:len(code)] = code
    image[BLOB_REQUEST_AT:BLOB_REQUEST_AT + len(request)] = request
    return bytes(image)


def test_exec(proc, timeout, arch):
    """Sube codigo maquina de verdad, lo corre, y comprueba las dos salidas.

    Esta es la tesis del proyecto: el agente escribe codigo, lo corre, y si
    esta mal el fault vuelve como un dato en vez de matar la maquina.
    """
    prog = PROGRAMS[arch]
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    # Estos programas corren con privilegio —uno rompe la pila a proposito— asi
    # que van a un nucleo reclamado (D29).
    core = core_for_raw(ask_verb, 9)
    if core is None:
        print("  no hay un nucleo donde correr con privilegio")
        return 1

    for name, code, expects_fault in (
        ("un programa que anda", prog["ok"], False),
        ("un programa que falla", prog["falla"], True),
        ("un programa que rompe la pila y falla", prog["pila_rota"], True),
    ):
        ok, c = ask_verb(10, "mem.claim", {"bytes": 4096, "align": 4096})
        if not ok:
            failures.append(f"{name}: no se pudo reclamar")
            continue
        h = c["handle"]

        ok, _ = ask_verb(11, "mem.write", {"handle": h, "bytes": code})
        if not ok:
            failures.append(f"{name}: no se pudo subir")
            continue

        print(f"  {name}: {code.hex()}")
        ok, r = ask_verb(12, "exec", {"handle": h, "mode": "raw", "core": core})
        if not ok:
            failures.append(f"{name}: exec fallo: {r}")
            continue

        print(f"    faulted={r['faulted']}")
        if r["faulted"] != expects_fault:
            failures.append(f"{name}: faulted={r['faulted']}, se esperaba {expects_fault}")

        if expects_fault:
            f = r["fault"]
            print(f"    fault: {f}")
            if not f or f.get("cause") != "page-fault":
                failures.append(f"{name}: la causa no es page-fault: {f}")
            if f and f.get("address") != 0x400000000000:
                failures.append(f"{name}: la direccion no es la que se toco: {f}")
        else:
            reg = prog["registro"]
            value = r["registers"].get(reg)
            print(f"    {reg}={value:#x}")
            if value != 0xC0FFEE:
                failures.append(f"{name}: {reg}={value:#x}, se esperaba 0xc0ffee")

        ask_verb(13, "release", {"handle": h})

    # Y lo mas importante: la maquina sigue contestando despues del fault.
    ok, _ = ask_verb(14, "describe", {})
    if not ok:
        failures.append("la maquina dejo de contestar despues del fault")
    else:
        print("  la maquina sigue viva despues del fault")

    # Y los registros con los que arranca los pone el agente (deuda 6). Cuales
    # se pueden poner lo dice la maquina, que es la que sabe como se llaman (D3).
    ok, d = ask_verb(16, "describe", {"what": ["exec"]})
    settable = d["exec"]["initial"] if ok else []
    print(f"  registros que se pueden poner: {len(settable)}, empezando por {settable[:3]}")
    if not settable:
        failures.append("el kernel no publica que registros se pueden poner")
    else:
        # Un programa que no hace nada: lo unico que se mira es con que valores
        # arranco. Si el kernel no los cargara, volverian en cero.
        ok, c = ask_verb(17, "mem.claim", {"bytes": 4096, "align": 4096})
        h = c["handle"]
        ret = (0xD65F03C0).to_bytes(4, "little") if arch == "aarch64" else b"\xc3"
        ask_verb(18, "mem.write", {"handle": h, "bytes": ret})
        # El segundo y el tercero: el primero lleva por omision la direccion de
        # entrada, asi que no distinguiria "lo puso el agente" de "lo puso el
        # kernel".
        want = {settable[1]: 0xCAFE, settable[2]: 0xD00D}
        ok, r = ask_verb(19, "exec",
                         {"handle": h, "mode": "raw", "core": core, "regs": want})
        if not ok or r.get("faulted"):
            failures.append(f"no se pudo correr con registros iniciales: {r}")
        else:
            got = {k: r["registers"][k] for k in want}
            print(f"  se pidio {want} y volvio {got}")
            if got != want:
                failures.append(f"los registros no arrancaron como se pidio: {got}")

        # Y uno que la maquina no tiene se rechaza en vez de ignorarse: correr
        # con un registro sin poner seria hacer algo distinto de lo pedido.
        ok, r = ask_verb(20, "exec",
                         {"handle": h, "mode": "raw", "core": core, "regs": {"nada": 1}})
        if ok:
            failures.append("acepto un registro que esta maquina no tiene")
        else:
            print(f"    y un registro que no existe se rechaza: {r}")
        ask_verb(21, "release", {"handle": h})

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  exec: ok")
    return 0


def test_cores(proc, timeout):
    """Reclama todos los nucleos menos el que atiende, y los arranca."""
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, d = ask_verb(20, "describe", {"what": ["cpus"]})
    if not ok:
        print("  no se pudo listar los nucleos")
        return 1
    cpus = d["cpus"]
    print(f"  la maquina informa {len(cpus)} nucleos: {[c['id'] for c in cpus]}")

    # Los que ya estan reclamados por una prueba anterior cuentan como
    # arrancados: desde D29 todo lo que corre con privilegio pide un nucleo, asi
    # que para cuando llega esta prueba puede haber varios tomados.
    ok, d = ask_verb(19, "describe", {"what": ["cores"]})
    arrancados = [c["id"] for c in (d.get("cores") or [])] if ok else []
    if arrancados:
        print(f"    ya reclamados por otra prueba: {arrancados}")

    for c in cpus:
        if c["id"] in arrancados:
            continue
        ok, r = ask_verb(21, "core.claim", {"id": c["id"]})
        if ok:
            print(f"    id={c['id']} -> handle {r['handle']}, {r['state']}")
            arrancados.append(c["id"])
            if r["state"] != "idle":
                failures.append(f"el nucleo {c['id']} quedo en {r['state']}")
        else:
            # Uno tiene que fallar: el que esta contestando.
            print(f"    id={c['id']} -> {r['error']}")
            if r["error"] not in ("is-boot-core", "core-not-usable"):
                failures.append(f"el nucleo {c['id']} fallo con {r['error']}")

    if not arrancados and len(cpus) > 1:
        failures.append("no se pudo arrancar ni un nucleo")

    # Reclamarlo dos veces tiene que fallar.
    if arrancados:
        ok, e = ask_verb(22, "core.claim", {"id": arrancados[0]})
        if ok or e.get("error") != "already-claimed":
            failures.append("dejo reclamar dos veces el mismo nucleo")

    # Y uno que no existe, tambien.
    ok, e = ask_verb(23, "core.claim", {"id": 9999})
    if ok or e.get("error") != "no-such-core":
        failures.append("dejo reclamar un nucleo inexistente")

    # Los reclamados se ven en describe.
    ok, d = ask_verb(24, "describe", {"what": ["cores"]})
    if not ok or len(d.get("cores", [])) != len(arrancados):
        failures.append(f"describe no informa los nucleos reclamados: {d}")
    else:
        print(f"  describe informa {len(d['cores'])} reclamados")

    # Y la maquina sigue contestando con los otros nucleos corriendo.
    ok, _ = ask_verb(25, "describe", {})
    if not ok:
        failures.append("la maquina dejo de contestar")

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print(f"  nucleos: ok ({len(arrancados)} arrancados)")
    return 0


def mov_reg_imm64(reg, v):
    """movz/movk para dejar un inmediato de 64 bits en xN (aarch64)."""
    out = b""
    first = True
    for hw in range(4):
        chunk = (v >> (16 * hw)) & 0xFFFF
        if chunk == 0 and not first:
            continue
        base = 0xD2800000 if first else 0xF2800000
        out += (base | (hw << 21) | (chunk << 5) | reg).to_bytes(4, "little")
        first = False
    return out or (0xD2800000 | reg).to_bytes(4, "little")


def emit_writes(arch, writes, con_ret=True):
    """Codigo maquina que hace esas escrituras de 32 bits y vuelve.

    Es lo que haria el driver de red del agente para tocar el timbre. Se genera
    a mano porque el kernel publica direcciones, no codigo.
    """
    if arch == "x86_64":
        code = b""
        for addr, val, width in writes:
            code += b"\x48\xb8" + addr.to_bytes(8, "little")   # mov rax, addr
            if width == 8:
                # Los registros de un motor de DMA son de 64 bits y hay que
                # escribirlos enteros: partirlos en dos mitades es escribirle
                # dos veces media direccion.
                code += b"\x48\xb9" + (val & 0xFFFFFFFFFFFFFFFF).to_bytes(8, "little")
                code += b"\x48\x89\x08"                        # mov [rax], rcx
            else:
                code += b"\xc7\x00" + (val & 0xFFFFFFFF).to_bytes(4, "little")
        return code + (b"\xc3" if con_ret else b"")            # ret

    # aarch64: armar la direccion en x0 y el valor en x1 o w1, y guardar.
    def mov_imm(reg, v, wide):
        """movz/movk hasta armar el valor. `wide` elige el registro de 64 bits."""
        out = b""
        first = True
        # El bit 31 del opcode es el que distingue x de w. Un movz de 32 bits
        # pone en cero la mitad de arriba del registro, asi que no alcanza con
        # el mismo codigo: hay que pedir el ancho.
        sf = (1 << 31) if wide else 0
        for hw in range(4 if wide else 2):
            chunk = (v >> (16 * hw)) & 0xFFFF
            if chunk == 0 and not first:
                continue
            base = 0x52800000 if first else 0x72800000
            out += (sf | base | (hw << 21) | (chunk << 5) | reg).to_bytes(4, "little")
            first = False
        return out or (sf | 0x52800000 | reg).to_bytes(4, "little")

    code = b""
    for addr, val, width in writes:
        code += mov_imm(0, addr, True)
        if width == 8:
            # Los registros de un motor de DMA son de 64 bits y hay que
            # escribirlos enteros: partirlos en dos mitades es escribirle dos
            # veces media direccion. En x86_64 esto ya estaba; aca aparecio
            # recien cuando el aparato de DMA se pudo probar de este lado, y el
            # sintoma era el peor posible — el aparato pedia la direccion 0.
            code += mov_imm(1, val & 0xFFFFFFFFFFFFFFFF, True)
            code += (0xF9000001).to_bytes(4, "little")   # str x1, [x0]
        else:
            code += mov_imm(1, val & 0xFFFFFFFF, False)
            code += (0xB9000001).to_bytes(4, "little")   # str w1, [x0]
    return code + ((0xD65F03C0).to_bytes(4, "little") if con_ret else b"")  # ret


def test_recover(proc, timeout, arch):
    """Un nucleo cuyo codigo no vuelve se puede recuperar (D13, D29).

    Es el caso que dejaba un recurso perdido para siempre: el agente manda un
    bucle infinito, el nucleo queda ocupado, y `describe` lo muestra corriendo
    sin cambiar nunca. La prueba no es que el kernel lo diga — es que despues de
    recuperarlo **el nucleo vuelva a correr codigo**.

    El corte va por el mismo timbre que despierta a los nucleos: desde afuera no
    se puede desviar la ejecucion de otro nucleo, solo pedirle que se desvie
    solo. El handler de la interrupcion lo manda al mismo punto de recuperacion
    que usa un fault.
    """
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    core = core_for_raw(ask_verb, 140)
    if core is None:
        print("  no hay un nucleo donde correr con privilegio")
        return 0

    ok, c = ask_verb(141, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar memoria: {c}")
        return 1
    h = c["handle"]
    ask_verb(142, "mem.write", {"handle": h, "bytes": FOREVER[arch]})

    # Sin esperar: si se esperara, el que se cuelga es el pedido.
    ok, r = ask_verb(143, "exec",
                     {"handle": h, "mode": "raw", "core": core, "wait": False})
    if not ok:
        print(f"  no se pudo mandar el trabajo: {r}")
        return 1
    print(f"  mandado un bucle infinito al nucleo: state={r['state']}")

    ok, d = ask_verb(144, "describe", {"what": ["cores"]})
    w = next((c2["work"] for c2 in d["cores"] if c2["handle"] == core), None) if ok else None
    if not w or w["state"] != "running":
        failures.append(f"el kernel no lo ve corriendo: {w}")
    else:
        print("  y describe lo ve corriendo, como tiene que ser")

    # Y ahora recuperarlo. `release` le pide que corte y espera.
    ok, r = ask_verb(145, "release", {"handle": core})
    if not ok:
        # Que falle es un resultado legitimo: si el codigo hubiera enmascarado
        # las interrupciones no habria forma de sacarlo (D29). Pero este bucle
        # no enmascara nada, asi que tiene que dejarse cortar.
        failures.append(f"no se pudo cortar un bucle que no enmascara nada: {r}")
    else:
        print(f"  recuperado: {r}")
        # La prueba de verdad: el nucleo tiene que volver a servir.
        ok, again = ask_verb(146, "core.claim", {"id": core_id_of(ask_verb, core)})
        if not ok:
            failures.append(f"no se pudo reclamar de nuevo: {again}")
        else:
            code, register = WHICH_CORE[arch]
            ask_verb(147, "mem.write", {"handle": h, "bytes": code})
            ok, r = ask_verb(148, "exec",
                             {"handle": h, "mode": "raw", "core": again["handle"]})
            if not ok or r.get("faulted"):
                failures.append(f"el nucleo recuperado no corre: {r}")
            else:
                mask = 0x00FFFFFF if arch == "aarch64" else 0xFFFFFFFF
                print(f"  y vuelve a correr codigo: {register}="
                      f"{r['registers'][register] & mask}")

    # Y la otra mitad, que es la que hace que la primera se pueda creer: un
    # codigo que **si** enmascara no se deja cortar, y ahi el kernel tiene que
    # fallar y decirlo. Prometer que se recupero un nucleo que sigue corriendo
    # codigo de otro seria lo peor de los dos mundos.
    # Lo que la maquina promete se le pregunta a ella, no se deduce de como se
    # llama la arquitectura (P4).
    ok, d = ask_verb(156, "describe", {"what": ["exec"]})
    reach = d["exec"].get("cancel") if ok else None
    print(f"  hasta donde llega el corte en esta maquina: {reach}")

    core2 = core_for_raw(ask_verb, 151)
    if core2 is not None:
        ask_verb(152, "mem.write", {"handle": h, "bytes": DEAF_FOREVER[arch]})
        ok, r = ask_verb(153, "exec",
                         {"handle": h, "mode": "raw", "core": core2, "wait": False})
        if ok:
            print("  y ahora uno que se tapa los oidos antes de colgarse")
            ok, r = ask_verb(154, "release", {"handle": core2})
            if reach == "even-if-masked":
                # Hay una linea que la mascara comun no tapa: tiene que caer.
                if not ok:
                    failures.append(f"prometio cortar aun enmascarado y no pudo: {r}")
                else:
                    print(f"  y la linea que no se puede enmascarar lo corta: {r}")
            elif ok:
                failures.append("dijo haber recuperado un nucleo que enmascaro")
            else:
                print(f"  el kernel no lo pudo cortar, y lo dice: {r}")
                if r.get("error") != "core-did-not-stop":
                    failures.append(f"el motivo no es el que corresponde: {r}")
                # Y queda marcado, para que no se le mande mas trabajo.
                ok, d = ask_verb(155, "describe", {"what": ["cores"]})
                st = next((c2["state"] for c2 in d["cores"] if c2["handle"] == core2), "?")
                print(f"  y queda marcado como: {st}")
                if st != "lost":
                    failures.append(f"un nucleo perdido no quedo marcado: {st}")

    ask_verb(149, "release", {"handle": h})

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  recuperar un nucleo: ok")
    return 0


def core_id_of(ask_verb, handle):
    """Con que numero nombra la maquina al nucleo de ese handle."""
    ok, d = ask_verb(150, "describe", {"what": ["cores"]})
    if ok:
        for c in d.get("cores") or []:
            if c["handle"] == handle:
                return c["id"]
    return 1


def test_msi(proc, timeout, arch):
    """El agente le hace disparar una interrupcion a un aparato de verdad (D4).

    Los aparatos PCIe de hoy no tienen cable de interrupcion: **escriben un dato
    en una direccion** y el silicio lo convierte en interrupcion. Saber que cable
    le tocaria al aparato requeriria interpretar AML, un lenguaje entero adentro
    de ACPI; esto no lo necesita.

    El kernel elige el numero, instala el handler y publica **la escritura que lo
    dispara**. El agente le pone esa direccion y ese dato al aparato en su
    registro de MSI, y desde ahi el aparato interrumpe solo.

    La prueba no le cree al kernel: el handler del agente escribe una marca en su
    propia memoria, y lo que se comprueba es que **la marca aparezca** despues de
    que el aparato haya hablado.
    """
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, c = ask_verb(170, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar: {c}")
        return 1
    h, base = c["handle"], c["start"]
    flag = base + 2048

    # El handler: escribe una marca en la bandera y vuelve.
    if arch == "x86_64":
        handler = b"\x48\xb8" + flag.to_bytes(8, "little") + b"\xc6\x00\x2a\xc3"
    else:
        handler = mov_reg_imm64(0, flag) + \
                  (0x52800541).to_bytes(4, "little") + \
                  (0x39000001).to_bytes(4, "little") + \
                  (0xD65F03C0).to_bytes(4, "little")
    ask_verb(171, "mem.write", {"handle": h, "off": 0, "bytes": handler})
    ask_verb(172, "mem.write", {"handle": h, "off": 2048, "bytes": b"\x00"})

    # El kernel elige el numero: el agente no tiene como saber cual esta libre.
    ok, r = ask_verb(173, "irq.install", {"handle": h, "msi": True})
    if not ok:
        print(f"  esta maquina no ofrece interrupciones por escritura: {r}")
        return 0
    writes = r["trigger"]
    print(f"  el kernel eligio la interrupcion {r['interrupt']}")
    print(f"  y se dispara escribiendo: {[(hex(a), hex(v), w) for a, v, w in writes]}")
    if not writes:
        failures.append("no publico como dispararla")
        return 1
    msi_addr, msi_data, _ = writes[0]

    # Antes del aparato: **hacerla sonar a mano**, con la misma escritura que el
    # kernel acaba de publicar. Separa las dos mitades — si esto no anda, el
    # problema es como quedo instalada la interrupcion; si anda y el aparato no
    # la dispara, el problema es del aparato o de lo que hay entre los dos.
    core0 = core_for_raw(ask_verb, 193)
    if core0 is not None:
        ring = emit_writes(arch, [(msi_addr, msi_data, 4)])
        ok, rp = ask_verb(194, "mem.claim", {"bytes": 4096, "align": 4096})
        ask_verb(195, "mem.write", {"handle": rp["handle"], "bytes": ring})
        ask_verb(196, "exec", {"handle": rp["handle"], "mode": "raw", "core": core0})
        for _ in range(10):
            ask_verb(197, "describe", {})
        ok, m = ask_verb(198, "mem.read", {"handle": h, "off": 2048, "len": 1})
        by_hand = m["bytes"][0] if ok else 0
        print(f"  sonandola a mano, la marca queda en: {by_hand:#x}")
        if by_hand != 0x2A:
            failures.append("la interrupcion no suena ni escribiendola a mano")
        # Y se limpia, para que lo que venga despues no herede esta marca.
        ask_verb(199, "mem.write", {"handle": h, "off": 2048, "bytes": b"\x00"})
        ask_verb(200, "release", {"handle": rp["handle"]})

    # Y ahora el aparato. Se lo busca en el bus, como haria el agente.
    ok, d = ask_verb(174, "describe", {"what": ["pcie"]})
    if not ok or not d["pcie"]:
        print("  la maquina no informa PCIe")
        return 0
    ok, cfg = ask_verb(175, "mem.claim", {"at": d["pcie"]["base"], "bytes": 1 << 20})
    if not ok:
        print(f"  no se pudo mirar el bus: {cfg}")
        return 1
    slot = None
    for dev in range(32):
        off = dev << 15
        ok, x = ask_verb(176, "mem.read", {"handle": cfg["handle"], "off": off, "len": 4})
        if ok and int.from_bytes(x["bytes"], "little") == EDU_ID:
            slot = off
            break
    if slot is None:
        print("  no hay un aparato con MSI en este bus")
        return 0

    # Su registro de MSI. En el aparato `edu` la capacidad arranca en 0x40:
    # control en 0x42, direccion en 0x44, dato en 0x4c.
    ask_verb(177, "mem.write", {"handle": cfg["handle"], "off": slot + 0x44,
                                "bytes": (msi_addr & 0xFFFFFFFF).to_bytes(4, "little")})
    ask_verb(178, "mem.write", {"handle": cfg["handle"], "off": slot + 0x48,
                                "bytes": (msi_addr >> 32).to_bytes(4, "little")})
    ask_verb(179, "mem.write", {"handle": cfg["handle"], "off": slot + 0x4C,
                                "bytes": (msi_data & 0xFFFF).to_bytes(2, "little")})
    # Y habilitarla: bit 0 del control, mas ser maestro del bus.
    ask_verb(180, "mem.write", {"handle": cfg["handle"], "off": slot + 0x42,
                                "bytes": (1).to_bytes(2, "little")})
    ask_verb(181, "mem.write", {"handle": cfg["handle"], "off": slot + 4,
                                "bytes": bytes([0x06, 0x00])})
    print("  el aparato quedo configurado para disparar esa interrupcion")

    # El aparato interrumpe cuando termina un DMA. Se le pide uno.
    ok, r = ask_verb(182, "mem.read", {"handle": cfg["handle"], "off": slot + 0x10, "len": 4})
    bar = int.from_bytes(r["bytes"], "little") & ~0xF
    ok, buf = ask_verb(183, "mem.claim", {"bytes": 4096, "align": 4096})
    ask_verb(184, "dma.allow", {"device": (slot >> 15) << 3, "handle": buf["handle"]})
    code = emit_writes(arch, [
        (bar + EDU_DMA_SRC, EDU_INTERNAL, 8),
        (bar + EDU_DMA_DST, buf["start"], 8),
        (bar + EDU_DMA_COUNT, 8, 8),
        # Bit 2: que avise con una interrupcion al terminar.
        (bar + EDU_DMA_CMD, EDU_DMA_START | EDU_DMA_TO_RAM | 0x4, 8),
    ])
    # **En ARM la escritura del MSI tambien es un acceso del aparato**, asi que
    # el IOMMU la bloquea igual que bloquearia un DMA a memoria no declarada
    # (D8). Hay que declararla, y el sintoma de no hacerlo no se parece a la
    # causa: el aparato queda configurado, el DMA llega, y la interrupcion
    # simplemente no aparece nunca.
    ok, frame = ask_verb(191, "mem.claim", {"at": msi_addr & ~0xFFF, "bytes": 4096})
    if ok:
        ok2, _ = ask_verb(192, "dma.allow",
                          {"device": (slot >> 15) << 3, "handle": frame["handle"]})
        print(f"  y se le declara al aparato la pagina por donde interrumpe: {ok2}")

    ok, prog = ask_verb(185, "mem.claim", {"bytes": 4096, "align": 4096})
    ask_verb(186, "mem.write", {"handle": prog["handle"], "bytes": code})
    core = core_for_raw(ask_verb, 187)
    if core is None:
        print("  no hay un nucleo donde tocar el aparato")
        return 0
    ok, r = ask_verb(188, "exec", {"handle": prog["handle"], "mode": "raw", "core": core})
    if not ok or r.get("faulted"):
        failures.append(f"el codigo que toca el aparato fallo: {r}")

    # Y darle tiempo a que el aparato hable.
    for _ in range(30):
        ask_verb(189, "describe", {})
    ok, m = ask_verb(190, "mem.read", {"handle": h, "off": 2048, "len": 1})
    got = m["bytes"][0] if ok else 0
    print(f"  la marca que deja el handler del agente: {got:#x}")
    if got != 0x2A:
        failures.append("el aparato no disparo la interrupcion, o el handler no corrio")

    # Y se devuelve todo lo reclamado. Las pruebas comparten un solo arranque,
    # asi que una que se queda con el espacio de configuracion deja sin aparato
    # a la que sigue — y el sintoma aparece en la otra prueba, no en esta.
    for n, handle in enumerate([cfg["handle"], buf["handle"], prog["handle"], h]):
        ask_verb(201 + n, "release", {"handle": handle})

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  msi: ok")
    return 0


def test_deadline(proc, timeout, arch):
    """El agente declara cuanto puede tardar su codigo, y el kernel lo cumple.

    Es el caso que dejaba la maquina **escuchando sin contestar**: un bucle
    infinito en el nucleo del protocolo. Ahi las interrupciones entran —D29 hace
    que el agente corra sin privilegio, asi que no las puede tapar— pero `exec`
    no vuelve, y el bucle que atiende el cable no corre mas.

    La prueba no admite interpretacion: se manda un bucle sin salida con un
    plazo de cien milisegundos, **sin nucleo**, o sea justo donde antes se
    perdia la maquina. Si el plazo no se cumpliera, esto no volveria nunca.
    """
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, d = ask_verb(160, "describe", {"what": ["exec"]})
    if not ok or not d["exec"].get("deadline"):
        print("  esta maquina no puede hacer cumplir un plazo")
        return 0
    print("  la maquina dice que se puede declarar un plazo")
    # Como se vuelve de `supervised` lo publica la maquina, no se hornea (P4).
    back = d["exec"]["return"]

    # En el nucleo del protocolo el agente corre supervisado (D29), asi que la
    # memoria tiene que ser suya.
    ok, u = ask_verb(161, "mem.claim", {"bytes": 4096, "user": True})
    if not ok:
        print(f"  no se pudo reclamar para el agente: {u}")
        return 1
    h = u["handle"]
    ask_verb(162, "mem.write", {"handle": h, "bytes": FOREVER[arch]})

    print("  corriendo un bucle sin salida con 100 ms de plazo...")
    try:
        ok, r = ask_verb(163, "exec",
                         {"handle": h, "mode": "supervised", "deadline_ms": 100})
    except TimeoutError:
        print("  FALLA: el exec no volvio — el plazo no se cumplio")
        return 1

    if not ok:
        failures.append(f"exec con plazo fallo: {r}")
    elif not r.get("cancelled"):
        failures.append(f"volvio sin decir que lo cortaron: {r}")
    else:
        print(f"  volvio cortado: cancelled={r['cancelled']}, faulted={r['faulted']}")

    # Y lo mas importante: la maquina sigue contestando.
    ok, _ = ask_verb(164, "describe", {})
    if not ok:
        failures.append("la maquina dejo de contestar")
    else:
        print("  y la maquina sigue contestando")

    # Y un plazo que no vence no molesta: el mismo verbo con codigo que termina.
    ask_verb(165, "mem.write", {"handle": h, "bytes": PROGRAMS[arch]["ok"] + back})
    ok, r = ask_verb(166, "exec",
                     {"handle": h, "mode": "supervised", "deadline_ms": 5000})
    if not ok or r.get("cancelled"):
        failures.append(f"corto un codigo que termino solo: {r}")
    else:
        print("  y un plazo que no vence no corta nada")

    # Y el mismo plazo, pero en **otro** nucleo. Ahi el corte no lo hace el reloj
    # local sino el timbre, pero para el agente tiene que verse igual: declaro
    # cuanto podia tardar y el kernel lo cumple.
    core = core_for_raw(ask_verb, 168)
    if core is not None:
        ok, cr = ask_verb(169, "mem.claim", {"bytes": 4096, "align": 4096})
        ask_verb(170, "mem.write", {"handle": cr["handle"], "bytes": FOREVER[arch]})
        ok, r = ask_verb(171, "exec", {"handle": cr["handle"], "mode": "raw",
                                       "core": core, "deadline_ms": 100})
        if not ok:
            failures.append(f"el plazo en otro nucleo no se cumplio: {r}")
        elif not r.get("cancelled"):
            failures.append(f"volvio sin decir que lo cortaron: {r}")
        else:
            print("  y en otro nucleo tambien: volvio cortado")
        ask_verb(172, "release", {"handle": cr["handle"]})

    ask_verb(167, "release", {"handle": h})

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  plazo: ok")
    return 0


def test_clock(proc, timeout):
    """El reloj de la maquina mide tiempo, no vueltas (deuda 17).

    La prueba no es que el kernel informe un numero: es que **dos lecturas
    separadas por un tiempo conocido den ese tiempo**. Un reloj que no avanza, o
    uno cuya frecuencia esta mal por un factor grande, no pasa esto.

    El margen es amplio a proposito: el reloj del emulador no avanza al mismo
    ritmo que el del host, asi que lo que se comprueba es el orden de magnitud —
    que es justamente lo que la ventana de rescate del blob necesita.
    """
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, d = ask_verb(130, "describe", {"what": ["clock"]})
    if not ok or d["clock"] is None:
        print("  esta maquina no dice a que ritmo sube su contador")
        return 0
    clock = d["clock"]
    print(f"  reloj: {clock['kind']} a {clock['hz']} Hz")
    if clock["hz"] < 1000:
        failures.append(f"una frecuencia de {clock['hz']} Hz no sirve para medir")

    first = clock["ticks"]
    delay = 0.5
    time.sleep(delay)
    ok, d = ask_verb(131, "describe", {"what": ["clock"]})
    second = d["clock"]["ticks"] if ok else first

    if second <= first:
        failures.append("el contador no avanzo: no es un reloj")
    else:
        medido = (second - first) / clock["hz"]
        print(f"  esperando {delay}s el reloj marco {medido:.3f}s")
        # Orden de magnitud: descarta una frecuencia equivocada por mucho, que es
        # lo que haria inutil la ventana de rescate.
        if not (delay * 0.1 <= medido <= delay * 4):
            failures.append(f"marco {medido:.3f}s donde paso {delay}s")

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  reloj: ok")
    return 0


def core_for_raw(ask_verb, n=200):
    """Un nucleo donde se pueda correr `raw` (D29).

    En el nucleo que atiende el protocolo el agente corre **supervisado y
    punto**: ahi manda el kernel, y para que eso sea verdad y no una intencion,
    el agente no puede *poder* enmascarar las interrupciones — que es
    privilegiado. Asi que todo lo que necesita privilegio de verdad —tocar los
    registros de un aparato, romper la pila a proposito— pide un nucleo.

    Se reutiliza el que ya este reclamado: son varias pruebas en el mismo
    arranque, y reclamar uno por prueba se quedaria sin nucleos.
    """
    ok, d = ask_verb(n, "describe", {"what": ["cores"]})
    if ok:
        for c in d.get("cores") or []:
            if c["state"] == "idle":
                return c["handle"]
    ok, cpus = ask_verb(n, "describe", {"what": ["cpus"]})
    if not ok:
        return None
    for c in cpus["cpus"]:
        ok, r = ask_verb(n, "core.claim", {"id": c["id"]})
        if ok:
            return r["handle"]
    return None


def test_doorbell(proc, timeout, arch):
    """El agente toca el timbre del kernel con codigo maquina propio.

    Es lo que va a hacer su driver de red: dejar el pedido en el buzon y avisar.
    """
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, d = ask_verb(40, "describe", {"what": ["channel"]})
    if not ok:
        print("  no se pudo leer el acuerdo del canal")
        return 1
    ch = d["channel"]
    bell = ch.get("doorbell")
    if not bell:
        print("  el kernel no publica timbre del buzon")
        return 1

    before = ch["rings"]
    print(f"  el timbre es el {bell['id']}, sono {before} veces hasta ahora")
    for a, v, w in bell["writes"]:
        print(f"    escribir {v:#x} ({w} bytes) en {a:#x}")

    # Codigo maquina que hace esas escrituras: exactamente lo que haria el
    # driver del agente.
    code = emit_writes(arch, bell["writes"])
    ok, c = ask_verb(41, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar memoria: {c}")
        return 1
    h = c["handle"]
    ask_verb(42, "mem.write", {"handle": h, "bytes": code})

    # Tocar el timbre es escribirle a un registro del controlador de
    # interrupciones: necesita privilegio, asi que va a un nucleo (D29).
    core = core_for_raw(ask_verb, 46)
    if core is None:
        print("  no hay un nucleo donde correr con privilegio")
        return 1

    print(f"  el agente toca el timbre: {code.hex()}")
    ok, r = ask_verb(43, "exec", {"handle": h, "mode": "raw", "core": core})
    if not ok or r.get("faulted"):
        failures.append(f"el codigo del timbre fallo: {r}")

    ok, d = ask_verb(44, "describe", {"what": ["channel"]})
    after = d["channel"]["rings"] if ok else -1
    print(f"  y ahora sono {after} veces")

    if after <= before:
        failures.append("el timbre no sono: la interrupcion del agente no llego")

    ask_verb(45, "release", {"handle": h})

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  timbre: ok")
    return 0


def test_permission(proc, timeout, arch):
    """Memoria pedida para el agente, y la prueba de que el bit esta puesto.

    La comprobacion no es mirar lo que dice el kernel: es que una memoria
    marcada para el agente **deja de ser ejecutable con privilegio**. Asi que si
    el bit se puso de verdad, correr codigo ahi con `exec` tiene que fallar con
    un fault de permiso al buscar la instruccion.
    """
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ret = (0xD65F03C0).to_bytes(4, "little") if arch == "aarch64" else b"\xc3"

    # Todo lo de abajo corre `raw`, asi que va a un nucleo reclamado (D29). Y
    # tiene que ir: si se pidiera sin nucleo, el rechazo vendria de D29 y esta
    # prueba dejaria de probar lo que dice probar — que lo que niega el pedido
    # es el permiso de la memoria.
    core = core_for_raw(ask_verb, 79)
    if core is None:
        print("  no hay un nucleo donde correr con privilegio")
        return 1

    # Primero, memoria comun: correr ahi tiene que andar.
    ok, c = ask_verb(80, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar: {c}")
        return 1
    print(f"  memoria comun:      user={c['user']}, {c['bytes']} bytes")
    if c["user"]:
        failures.append("una memoria que no se pidio para el agente vino marcada")
    ask_verb(81, "mem.write", {"handle": c["handle"], "bytes": ret})
    ok, r = ask_verb(82, "exec", {"handle": c["handle"], "mode": "raw", "core": core})
    if not ok or r.get("faulted"):
        failures.append(f"no se pudo correr codigo en memoria comun: {r}")
    else:
        print("    y el kernel puede correr codigo ahi")

    # Ahora memoria para el agente.
    ok, u = ask_verb(83, "mem.claim", {"bytes": 4096, "user": True})
    if not ok:
        print(f"  no se pudo reclamar para el agente: {u}")
        return 1
    print(f"  para el agente:     user={u['user']}, {u['bytes']} bytes, en {u['start']:#x}")
    if not u["user"]:
        failures.append("se pidio para el agente y no quedo marcada")
    # Pedirlo redondea al bloque de la tabla: el kernel informa lo que quedo.
    if u["bytes"] < 2 * 1024 * 1024:
        failures.append(f"no se redondeo al bloque: {u['bytes']} bytes")
    if u["start"] % (2 * 1024 * 1024) != 0:
        failures.append(f"no quedo alineada al bloque: {u['start']:#x}")

    ask_verb(84, "mem.write", {"handle": u["handle"], "bytes": ret})
    # Desde que existe la transicion de privilegio, el kernel ni lo intenta:
    # `raw` sobre memoria del agente se rechaza **antes** de correr nada. La
    # misma pagina no puede ser las dos cosas, y aceptar el pedido seria
    # prometer algo que el hardware niega un microsegundo despues.
    ok, r = ask_verb(85, "exec", {"handle": u["handle"], "mode": "raw", "core": core})
    if ok:
        failures.append("el kernel acepto correr privilegiado en memoria del agente")
    else:
        print(f"    y el kernel ya no acepta correr privilegiado ahi: {r}")

    # Y al soltarla, el permiso se saca: si quedara, seria un agujero silencioso.
    ask_verb(86, "release", {"handle": u["handle"]})
    ok, v = ask_verb(87, "mem.claim", {"at": u["start"], "bytes": 4096})
    if ok:
        ask_verb(88, "mem.write", {"handle": v["handle"], "bytes": ret})
        ok2, r2 = ask_verb(89, "exec", {"handle": v["handle"], "mode": "raw", "core": core})
        if not ok2 or r2.get("faulted"):
            failures.append("al soltarla no se le saco el permiso")
        else:
            print("    y al soltarla vuelve a ser del kernel")

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  permiso: ok")
    return 0


# Los dos programas que prueban `supervised`. El segundo es el que le da sentido
# a D27: una sola instruccion privilegiada, la que hoy puede dejar la maquina
# muda sin recuperacion posible.
SUPERVISED = {
    "x86_64": {
        # mov rax, 0x00C0FFEE
        "ok": bytes([0x48, 0xC7, 0xC0, 0xEE, 0xFF, 0xC0, 0x00]),
        # cli — apagar las interrupciones. En anillo 0 se lleva el cordon
        # umbilical puesto; en anillo 3 es proteccion general.
        "privilegiada": bytes([0xFA]),
        "register": "rax",
    },
    "aarch64": {
        # movz x0, #0xFFEE ; movk x0, #0xC0, lsl #16
        "ok": bytes([0xC0, 0xFD, 0x9F, 0xD2, 0x00, 0x18, 0xA0, 0xF2]),
        # msr daifset, #2 — lo mismo, del otro lado.
        "privilegiada": bytes([0xDF, 0x42, 0x03, 0xD5]),
        "register": "x0",
    },
}


def test_supervised(proc, timeout, arch):
    """El agente declara con que privilegio corre, y lo hace cumplir el hardware.

    Es la mitad de arriba de D27. Lo que se prueba no es que el kernel diga que
    bajo de privilegio, sino las dos consecuencias observables:

      1. el codigo corre y vuelve — o sea que entro a anillo 3 / EL0, que la
         memoria estaba de verdad marcada para el agente (si no, ni la
         instruccion se podria buscar) y que la ventanilla de vuelta funciona;
      2. la instruccion que podia dejar la maquina muda **vuelve como fault**.

    La segunda es la unica que importa de verdad: es lo que el kernel no podia
    recuperar antes de D27.
    """
    prog = SUPERVISED[arch]
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    # El acuerdo lo publica la maquina: como se vuelve no se hornea (P4).
    ok, d = ask_verb(90, "describe", {"what": ["exec"]})
    if not ok:
        print(f"  el kernel no publica la seccion exec: {d}")
        return 1
    agreement = d["exec"]
    back_bytes = agreement["return"]
    print(f"  modos: {agreement['modes']}, pila: {agreement['stack']}")
    print(f"  para volver hay que emitir: {back_bytes.hex()}")
    if "supervised" not in agreement["modes"] or "raw" not in agreement["modes"]:
        failures.append(f"el kernel no ofrece los dos modos: {agreement['modes']}")

    # D29: en este nucleo manda el kernel, asi que aca el agente corre
    # supervisado y punto. Se publica en vez de dejar que el agente lo
    # descubra chocandose: que `raw` exista pero no en cualquier lado no se
    # puede adivinar desde afuera (P4).
    print(f"  y en el nucleo que atiende se corre: {agreement.get('this_core')}")
    if agreement.get("this_core") != "supervised":
        failures.append("el kernel no publica con que modo corre su propio nucleo")

    ok, u = ask_verb(91, "mem.claim", {"bytes": 4096, "user": True})
    if not ok:
        print(f"  no se pudo reclamar para el agente: {u}")
        return 1
    h = u["handle"]

    # 1. Corre sin privilegio y vuelve por la ventanilla.
    code = prog["ok"] + back_bytes
    ask_verb(92, "mem.write", {"handle": h, "bytes": code})
    print(f"  codigo supervisado: {code.hex()}")
    ok, r = ask_verb(93, "exec", {"handle": h, "mode": "supervised"})
    if not ok:
        failures.append(f"exec supervised fallo: {r}")
    elif r.get("faulted"):
        failures.append(f"el codigo supervisado fallo: {r['fault']}")
    else:
        value = r["registers"].get(prog["register"])
        print(f"    volvio: mode={r['mode']}, {prog['register']}={value:#x}")
        if value != 0xC0FFEE:
            failures.append(f"{prog['register']}={value:#x}, se esperaba 0xc0ffee")
        if r.get("mode") != "supervised":
            failures.append(f"el kernel dice que corrio {r.get('mode')}")

    # 2. Y la instruccion privilegiada vuelve como dato, no como muerte.
    code = prog["privilegiada"] + back_bytes
    ask_verb(94, "mem.write", {"handle": h, "bytes": code})
    print(f"  y ahora una instruccion privilegiada: {code.hex()}")
    ok, r = ask_verb(95, "exec", {"handle": h, "mode": "supervised"})
    if not ok:
        failures.append(f"exec supervised fallo: {r}")
    elif not r.get("faulted"):
        failures.append("apago las interrupciones sin privilegio: NO bajo de anillo")
    else:
        print(f"    volvio como fault: {r['fault']['cause']}")

    # 3. Y los pedidos que el kernel tiene que rechazar.
    #
    # El primero es D29 y es la parte que no se puede ablandar: si `raw` se
    # aceptara aca, el agente podria enmascarar las interrupciones en el nucleo
    # que sostiene el cordon, y "la interrupcion tiene prioridad" pasaria a ser
    # una intencion. No se le da nucleo a proposito.
    ok, r = ask_verb(96, "exec", {"handle": h, "mode": "raw"})
    if ok:
        failures.append("acepto raw en el nucleo que atiende el protocolo")
    else:
        print(f"    y raw en este nucleo no corre: {r}")
    ok, r = ask_verb(97, "exec", {"handle": h})
    if ok:
        failures.append("acepto un exec sin modo: el kernel eligio por el agente")
    else:
        print(f"    y sin declarar modo no corre: {r}")

    ask_verb(98, "release", {"handle": h})

    # Lo de siempre: la maquina sigue contestando.
    ok, _ = ask_verb(99, "describe", {})
    if not ok:
        failures.append("la maquina dejo de contestar")

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  supervisado: ok")
    return 0


# Un programa que devuelve el numero con el que la maquina nombra al nucleo en
# el que esta corriendo. Es lo que convierte "corrio en otro nucleo" de una
# afirmacion del kernel en algo comprobable desde afuera.

# Un bucle del que no se sale. Es el caso que deja un nucleo perdido: no falla
# —un fault volveria como dato— sino que **no vuelve**, que es lo que el kernel
# no puede distinguir de un trabajo largo.
FOREVER = {
    "x86_64": bytes([0xEB, 0xFE]),                      # jmp -2
    "aarch64": (0x14000000).to_bytes(4, "little"),       # b .
}

# Lo mismo, pero tapandose los oidos primero. Es el caso que **no** se puede
# recuperar: enmascarar es privilegiado y D29 dice que en su nucleo eso lo decide
# el agente, incluido no ser molestado. El kernel no puede prometer sacarlo de
# ahi, y lo que corresponde es decirlo en vez de mentir.
DEAF_FOREVER = {
    # cli ; jmp -2
    "x86_64": bytes([0xFA, 0xEB, 0xFE]),
    # msr daifset, #0xf ; b .
    "aarch64": (0xD50342DF).to_bytes(4, "little") + (0x14000000).to_bytes(4, "little"),
}
WHICH_CORE = {
    # mov eax, 1 ; cpuid ; shr ebx, 24 ; mov eax, ebx ; ret
    # Hoja 1 de CPUID: los 8 bits de arriba de EBX son el APIC ID, que es el
    # mismo numero con el que la MADT nombra a cada nucleo.
    "x86_64": (bytes([0xB8, 0x01, 0x00, 0x00, 0x00, 0x0F, 0xA2,
                      0xC1, 0xEB, 0x18, 0x89, 0xD8, 0xC3]), "rax"),
    # mrs x0, mpidr_el1 ; ret
    "aarch64": (bytes([0xA0, 0x00, 0x38, 0xD5, 0xC0, 0x03, 0x5F, 0xD6]), "x0"),
}


def test_on_core(proc, timeout, arch):
    """El agente elige en que nucleo corre su codigo (D13, D29).

    Reclamar un nucleo servia para reservarlo, no para usarlo: `exec` corria
    siempre en el que atiende el protocolo. La prueba de que eso cambio no es
    lo que el kernel dice, es que **el propio codigo del agente informe donde
    esta corriendo**: se corre el mismo programa en dos nucleos reclamados y
    los dos numeros tienen que ser distintos, y cada uno el que se pidio.

    Son dos nucleos reclamados y no uno contra el del protocolo porque desde
    D29 ahi el agente corre supervisado, y el registro que dice que nucleo es
    —`mpidr_el1` en aarch64— no se puede leer sin privilegio.
    """
    code, register = WHICH_CORE[arch]
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    # Dos nucleos: los ya reclamados por pruebas anteriores mas los que hagan
    # falta.
    ok, d = ask_verb(60, "describe", {"what": ["cores"]})
    mine = [c for c in (d.get("cores") or []) if ok and c["state"] == "idle"]
    ok, cpus = ask_verb(61, "describe", {"what": ["cpus"]})
    if ok:
        taken = {c["id"] for c in mine}
        for c in cpus["cpus"]:
            if len(mine) >= 2:
                break
            if c["id"] in taken:
                continue
            ok2, r = ask_verb(62, "core.claim", {"id": c["id"]})
            if ok2:
                mine.append(r)
    if len(mine) < 2:
        print(f"  hacen falta dos nucleos reclamados y hay {len(mine)}")
        return 0
    first, second = mine[0], mine[1]
    print(f"  dos nucleos reclamados: la maquina los llama {first['id']} y {second['id']}")

    ok, c = ask_verb(63, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar memoria: {c}")
        return 1
    h = c["handle"]
    ask_verb(64, "mem.write", {"handle": h, "bytes": code})

    mask = 0x00FFFFFF if arch == "aarch64" else 0xFFFFFFFF

    # El mismo programa en los dos nucleos: cada uno tiene que informar el suyo.
    ok, r = ask_verb(65, "exec", {"handle": h, "mode": "raw", "core": first["handle"]})
    if not ok or r.get("faulted"):
        print(f"  FALLA: no se pudo correr en el primer nucleo: {r}")
        return 1
    here = r["registers"][register] & mask
    print(f"  corriendo en uno:   {register}={here}  (core={r['core']})")

    ok, r = ask_verb(66, "exec", {"handle": h, "mode": "raw", "core": second["handle"]})
    if not ok:
        print(f"  FALLA: exec en el otro nucleo fallo: {r}")
        return 1
    if r.get("faulted"):
        failures.append(f"el codigo fallo en el otro nucleo: {r['fault']}")
    there = r["registers"][register] & mask
    print(f"  y en el otro:       {register}={there}  (core={r['core']})")

    if there == here:
        failures.append("los dos dieron el mismo nucleo: no se movio a ningun lado")
    if here != first["id"]:
        failures.append(f"corrio en el nucleo {here} y se habia pedido el {first['id']}")
    if there != second["id"]:
        failures.append(f"corrio en el nucleo {there} y se habia pedido el {second['id']}")

    # 3. Un handle que no es de un nucleo se rechaza en vez de correr aca.
    ok, r = ask_verb(67, "exec", {"handle": h, "mode": "raw", "core": 9999})
    if ok:
        failures.append("acepto un nucleo que no existe")
    else:
        print(f"    y un nucleo que no existe no cae de vuelta aca: {r}")

    # 4. Sin esperar (deuda 13): el kernel contesta enseguida y el resultado se
    #    busca despues en describe. La prueba de que el resultado es de verdad
    #    y no un eco: tiene que traer el numero de nucleo que informo el codigo
    #    del agente, el mismo que dio la corrida sincronica de arriba.
    ok, r = ask_verb(69, "exec",
                     {"handle": h, "mode": "raw", "core": second["handle"], "wait": False})
    if not ok:
        failures.append(f"no acepto un exec sin esperar: {r}")
    else:
        print(f"  sin esperar:        state={r['state']}, registers={r['registers']}")
        if r["state"] != "running" or r["registers"] is not None:
            failures.append(f"un exec sin esperar contesto como si hubiera terminado: {r}")

        # Se pregunta hasta que termine. Es lo que haria el agente: mandar algo
        # largo, irse a hacer otra cosa, y volver a buscar el resultado.
        done = None
        for _ in range(40):
            ok, d = ask_verb(70, "describe", {"what": ["cores"]})
            if not ok:
                break
            w = next((c["work"] for c in d["cores"] if c["handle"] == second["handle"]), None)
            if w and w["state"] == "done":
                done = w
                break
        if done is None:
            failures.append("el trabajo mandado sin esperar nunca aparecio terminado")
        else:
            got = done["registers"][register] & mask
            print(f"  y el resultado estaba en describe: {register}={got}")
            if got != second["id"]:
                failures.append(f"el resultado dice nucleo {got} y era el {second['id']}")
            if done["faulted"]:
                failures.append(f"el trabajo sin esperar fallo: {done['fault']}")

    # 5. Y se puede devolver el nucleo. `core.claim` no tenia inverso: un nucleo
    #    reclamado quedaba reclamado para siempre, aunque su codigo hubiera
    #    terminado bien. La prueba es que despues de soltarlo se lo pueda
    #    **volver a reclamar y usar**, porque soltarlo no lo apaga: sigue
    #    durmiendo en su buzon.
    ok, r = ask_verb(71, "release", {"handle": second["handle"]})
    if not ok:
        failures.append(f"no se pudo devolver el nucleo: {r}")
    else:
        print(f"  devuelto: {r}")
        ok, again = ask_verb(72, "core.claim", {"id": second["id"]})
        if not ok:
            failures.append(f"un nucleo devuelto no se puede reclamar de nuevo: {again}")
        else:
            # Handle nuevo: los handles no se reusan nunca (D14).
            if again["handle"] == second["handle"]:
                failures.append("le dio el mismo handle a un reclamo nuevo")
            # Y el buzon tiene que estar limpio: el resultado del trabajo
            # anterior no puede aparecer como si fuera de este reclamo.
            ok, d = ask_verb(73, "describe", {"what": ["cores"]})
            w = next((c["work"] for c in d["cores"] if c["handle"] == again["handle"]), "?")
            print(f"  reclamado otra vez: handle {again['handle']}, work={w}")
            if w is not None:
                failures.append(f"el reclamo nuevo vino con el trabajo del anterior: {w}")
            # Y sirve: sigue vivo, no hubo que arrancarlo de nuevo.
            ok, r = ask_verb(74, "exec",
                             {"handle": h, "mode": "raw", "core": again["handle"]})
            if not ok or r.get("faulted"):
                failures.append(f"un nucleo devuelto y reclamado no corre: {r}")
            elif (r["registers"][register] & mask) != again["id"]:
                failures.append("corrio en otro nucleo del que se pidio")
            else:
                print("  y sigue sirviendo, sin haberlo arrancado de nuevo")

    ask_verb(68, "release", {"handle": h})

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  trabajo en otro nucleo: ok")
    return 0


# Como se identifica un controlador NVMe en el bus: no por fabricante y modelo,
# sino por lo que **hace**. Los tres bytes son clase, subclase y la interfaz de
# programacion, y juntos quieren decir "almacenamiento / no volatil / NVMe".
# Buscar asi es lo que hace que el cargador ande contra cualquier NVMe y no
# contra el de QEMU (P4: la maquina se describe, no se la adivina).
NVME_CLASS = (0x01, 0x08, 0x02)


def pcie_scan(ask_verb, ident, ecam, buses=1):
    """Recorre el bus y devuelve lo que hay: (bdf, offset, id, clase, bar0).

    Es lo mismo que hara el cargador desde el blob, y por los mismos verbos: el
    kernel publica **donde se configura** PCIe, no que hay conectado (D4).
    """
    ok, cfg = ask_verb(ident, "mem.claim", {"at": ecam, "bytes": buses << 20})
    if not ok:
        # El motivo importa: "ya reclamado" quiere decir que otra prueba de esta
        # misma sesion no lo solto, y eso no se parece en nada a "no hay bus".
        return None, cfg
    found = []
    for bus in range(buses):
        for dev in range(32):
            off = (bus << 20) | (dev << 15)
            ok, r = ask_verb(ident, "mem.read", {"handle": cfg["handle"], "off": off, "len": 4})
            if not ok:
                continue
            who = int.from_bytes(r["bytes"], "little")
            # Un lugar vacio del bus se lee como todos unos: no hay nadie que
            # conteste y el bus devuelve eso en vez de fallar.
            if who in (0xFFFFFFFF, 0):
                continue
            ok, r = ask_verb(ident, "mem.read", {"handle": cfg["handle"], "off": off + 8, "len": 4})
            klass = int.from_bytes(r["bytes"], "little")
            ok, r = ask_verb(ident, "mem.read", {"handle": cfg["handle"], "off": off + 0x10, "len": 4})
            bar0 = int.from_bytes(r["bytes"], "little")
            # Un BAR de 64 bits ocupa DOS ranuras: los bits 2:1 en `10` dicen
            # que la mitad de arriba esta en la siguiente. Leer solo la primera
            # da una direccion truncada, que es peor que ninguna.
            wide = (bar0 & 0x6) == 0x4
            high = 0
            if wide:
                ok, r = ask_verb(ident, "mem.read",
                                 {"handle": cfg["handle"], "off": off + 0x14, "len": 4})
                high = int.from_bytes(r["bytes"], "little")
            ok, r = ask_verb(ident, "mem.read", {"handle": cfg["handle"], "off": off + 4, "len": 4})
            command = int.from_bytes(r["bytes"], "little") & 0xFFFF
            found.append({
                "bdf": (bus << 8) | (dev << 3),
                "off": off,
                "id": who,
                "class": ((klass >> 24) & 0xFF, (klass >> 16) & 0xFF, (klass >> 8) & 0xFF),
                "bar0": bar0,
                "wide": wide,
                "window": ((high << 32) | (bar0 & ~0xF)) if wide else (bar0 & ~0xF),
                "command": command,
            })
    return cfg, found


def test_lspci(proc, timeout):
    """Lista lo que hay en el bus, que es el primer paso de cualquier driver."""
    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, d = ask_verb(300, "describe", {"what": ["pcie"]})
    if not ok or not d["pcie"]:
        print("  esta maquina no informa PCIe")
        return 1
    cfg, devices = pcie_scan(ask_verb, 301, d["pcie"]["base"])
    if cfg is None:
        print(f"  no se pudo reclamar la ventana de configuracion: {devices}")
        return 1

    for dev in devices:
        vendor, model = dev["id"] & 0xFFFF, dev["id"] >> 16
        c, sub, prog = dev["class"]
        mark = "  <- NVMe" if dev["class"] == NVME_CLASS else ""
        # El bit 1 del comando es "responde a accesos de memoria" y el 2 es
        # "puede ser maestro del bus", que es lo que hace falta para que inicie
        # un DMA por su cuenta.
        flags = ("mem " if dev["command"] & 2 else "---- ") + ("master" if dev["command"] & 4 else "------")
        print(f"  {dev['bdf']:#06x}  {vendor:04x}:{model:04x}  "
              f"clase {c:02x}.{sub:02x}.{prog:02x}  ventana={dev['window']:#012x}"
              f"  [{flags}]{mark}")

    ask_verb(302, "release", {"handle": cfg["handle"]})
    nvme = [d for d in devices if d["class"] == NVME_CLASS]
    print(f"\n  {len(devices)} aparatos, {len(nvme)} de ellos NVMe")
    return 0 if nvme else 1


# --- El controlador NVMe ----------------------------------------------------
#
# Esto es un driver, y corre del lado del agente: el kernel no sabe que existe
# (D4). Usa los once verbos y nada mas — `mem.claim` para las colas, `dma.allow`
# para que el aparato las alcance, y lecturas y escrituras con el ancho exacto
# que pide cada registro.
#
# Los numeros salen de la especificacion NVMe 1.4, que es publica. No hay codigo
# copiado de ningun lado: los drivers que existen estan atados a la
# infraestructura de su sistema, y aca lo unico que hay son los once verbos.

# Registros del controlador, en su ventana de memoria.
NVME_CAP = 0x00       # que sabe hacer (64 bits)
NVME_VS = 0x08        # version
NVME_CC = 0x14        # configuracion: aca se lo prende y se lo apaga
NVME_CSTS = 0x1C      # estado: aca contesta si esta listo
NVME_AQA = 0x24       # cuantas entradas tienen las colas de administracion
NVME_ASQ = 0x28       # donde esta la cola de pedidos de administracion
NVME_ACQ = 0x30       # y la de respuestas

NVME_CC_ENABLE = 1 << 0

# Cuantas entradas tienen las colas de administracion. Cuatro alcanzan: los
# pedidos de administracion son un punado y se hacen de a uno.
ADMIN_ENTRIES = 4
# Una entrada de pedido son 64 bytes; una de respuesta, 16.
SQ_ENTRY = 64
CQ_ENTRY = 16


class Nvme:
    """Le habla a un controlador NVMe usando solo los verbos del kernel."""

    def __init__(self, ask_verb, ident):
        self.ask = ask_verb
        self.n = ident
        self.window = None      # el reclamo de su ventana de registros
        self.queues = {}        # numero de cola -> sus dos anillos y por donde van
        self.claims = []        # todo lo reclamado, para poder devolverlo
        self.tag = 1            # con que se reconoce cada respuesta
        self.stride = 4

    def _id(self):
        self.n += 1
        return self.n

    def reg_read(self, off, width=4):
        ok, r = self.ask(self._id(), "mem.read",
                         {"handle": self.window["handle"], "off": off,
                          "len": width, "width": width})
        if not ok:
            return None
        return int.from_bytes(r["bytes"], "little")

    def reg_write(self, off, value, width=4):
        ok, _ = self.ask(self._id(), "mem.write",
                         {"handle": self.window["handle"], "off": off,
                          "bytes": int(value).to_bytes(width, "little"), "width": width})
        return ok

    def find(self, ecam):
        """Busca el controlador en el bus y reclama su ventana de registros."""
        cfg, devices = pcie_scan(self.ask, self._id(), ecam)
        if cfg is None:
            return f"no se pudo mirar el bus: {devices}"
        mine = [d for d in devices if d["class"] == NVME_CLASS]
        if not mine:
            self.ask(self._id(), "release", {"handle": cfg["handle"]})
            return "no hay un controlador NVMe en este bus"
        dev = mine[0]

        # Que responda a accesos de memoria y que pueda ser maestro del bus: sin
        # lo segundo no puede leer sus propias colas, que viven en RAM.
        self.ask(self._id(), "mem.write",
                 {"handle": cfg["handle"], "off": dev["off"] + 4,
                  "bytes": bytes([0x06, 0x00])})
        self.ask(self._id(), "release", {"handle": cfg["handle"]})

        self.bdf = dev["bdf"]
        # 16 KiB: los registros entran en la primera pagina, pero los timbres
        # de las colas viven a partir de 0x1000 y hay uno por cola.
        ok, win = self.ask(self._id(), "mem.claim",
                           {"at": dev["window"], "bytes": 16384})
        if not ok:
            return f"no se pudo alcanzar su ventana: {win}"
        self.window = win
        return None

    def describe(self):
        """Lo que el controlador dice de si mismo, antes de tocarlo."""
        cap = self.reg_read(NVME_CAP, 8)
        vs = self.reg_read(NVME_VS)
        return {
            "version": f"{(vs >> 16) & 0xFFFF}.{(vs >> 8) & 0xFF}",
            # Cuantas entradas soporta una cola, menos uno.
            "max_entries": (cap & 0xFFFF) + 1,
            # Cada cuanto esta el timbre de la cola siguiente.
            "doorbell_stride": 4 << ((cap >> 32) & 0xF),
            # Cuanto puede tardar en estar listo, en pasos de 500 ms.
            "timeout_ms": ((cap >> 24) & 0xFF) * 500,
            # La pagina mas chica que sabe usar.
            "page_bytes": 1 << (12 + ((cap >> 48) & 0xF)),
        }

    def wait_ready(self, want, timeout_ms):
        """Espera a que CSTS.RDY diga lo que se le pidio a CC.EN."""
        # El plazo lo declara el propio controlador en CAP.TO: esperar un numero
        # inventado seria decidir por el cuanto puede tardar.
        deadline = time.time() + max(timeout_ms, 500) / 1000.0
        while time.time() < deadline:
            csts = self.reg_read(NVME_CSTS)
            if csts is None:
                return "el controlador dejo de contestar"
            if csts & 1 == want:
                return None
            # Bit 1 de CSTS: se murio y hay que resetearlo entero.
            if csts & 2:
                return "el controlador informa una falla fatal"
        return f"no llego a RDY={want} en {timeout_ms} ms"


    def shared_page(self, bytes_wanted=4096):
        """Memoria que el aparato tambien va a tocar: pedida, limpia y declarada.

        Los tres pasos van juntos **siempre**, y por eso van en una sola funcion.
        Dejarla sin limpiar hace leer respuestas que nadie escribio —una entrada
        vieja tiene el bit de fase puesto— y no declararla hace que el IOMMU la
        bloquee, que se ve igual que un aparato que no contesta.
        """
        ok, claim = self.ask(self._id(), "mem.claim",
                             {"bytes": bytes_wanted, "align": 4096})
        if not ok:
            return None, str(claim)
        self.claims.append(claim)
        self.ask(self._id(), "mem.write",
                 {"handle": claim["handle"], "bytes": bytes(bytes_wanted)})
        ok, e = self.ask(self._id(), "dma.allow",
                         {"device": self.bdf, "handle": claim["handle"]})
        if not ok:
            return None, f"el IOMMU no la dejo declarar: {e}"
        return claim, None

    def start(self, spec):
        """Apaga el controlador, le da sus colas de administracion y lo prende.

        Es la secuencia que manda la especificacion y no se puede acortar: hay
        que verlo apagado antes de configurarlo, porque los registros de las
        colas solo se leen cuando pasa de apagado a prendido.
        """
        # 1. Apagarlo y esperar a que lo confirme.
        self.reg_write(NVME_CC, 0)
        if (why := self.wait_ready(0, spec["timeout_ms"])):
            return f"no se apago: {why}"

        # 2. Memoria para las dos colas. Una pagina para cada una, que es de
        #    sobra: cuatro entradas de 64 bytes son 256.
        sq, why = self.shared_page()
        if why:
            return f"sin memoria para la cola de pedidos: {why}"
        cq, why = self.shared_page()
        if why:
            return f"sin memoria para la cola de respuestas: {why}"
        self.queues[0] = {"sq": sq, "cq": cq, "entries": ADMIN_ENTRIES,
                          "sq_tail": 0, "cq_head": 0, "phase": 1}

        # 5. Decirle donde estan y de que tamano. AQA lleva las dos cantidades
        #    menos uno, cada una en su mitad.
        self.reg_write(NVME_AQA, ((ADMIN_ENTRIES - 1) << 16) | (ADMIN_ENTRIES - 1))
        self.reg_write(NVME_ASQ, sq["start"], 8)
        self.reg_write(NVME_ACQ, cq["start"], 8)

        # 6. Prenderlo. Los ceros del medio son los tamanos de entrada por
        #    omision (64 y 16 bytes) y el conjunto de comandos NVM.
        self.reg_write(NVME_CC, NVME_CC_ENABLE | (6 << 16) | (4 << 20))
        if (why := self.wait_ready(1, spec["timeout_ms"])):
            return f"no se prendio: {why}"
        # Cada cuanto esta el timbre siguiente lo dice el aparato, no la
        # costumbre: con stride distinto de 4 los timbres caen en otro lado.
        self.stride = spec["doorbell_stride"]
        return None

    def release(self):
        """Suelta lo reclamado. Lo que el agente toma, el agente devuelve."""
        for claim in self.claims + ([self.window] if self.window else []):
            self.ask(self._id(), "release", {"handle": claim["handle"]})
        self.claims = []


    def bell(self, qid, which):
        """Donde esta el timbre de una cola. `which` es 0 para pedidos, 1 para
        respuestas.

        Van todos seguidos a partir de 0x1000, de a dos por cola, separados por
        lo que el aparato dijo en CAP.DSTRD. Suponer que la separacion es 4
        anda en QEMU y falla en silencio donde no lo sea.
        """
        return 0x1000 + (2 * qid + which) * self.stride

    def command(self, qid, opcode, nsid=0, prp1=0, cdw10=0, cdw11=0, cdw12=0):
        """Manda un comando a una cola y espera su respuesta.

        Una cola NVMe son dos anillos en RAM: uno donde el que manda escribe
        pedidos, otro donde el aparato escribe respuestas. Nadie interrumpe a
        nadie: se avisa tocando un timbre, que es una escritura en la ventana de
        registros del aparato.
        """
        q = self.queues[qid]
        # El pedido: 64 bytes. Solo se llenan los campos que se usan; el resto
        # va en cero, que para este comando significa "por omision".
        cmd = bytearray(SQ_ENTRY)
        cmd[0] = opcode
        cmd[1] = 0                                     # sin banderas
        cmd[2:4] = self.tag.to_bytes(2, "little")      # con que se reconoce la respuesta
        cmd[4:8] = nsid.to_bytes(4, "little")
        cmd[24:32] = prp1.to_bytes(8, "little")        # a donde escribe lo que devuelva
        cmd[40:44] = cdw10.to_bytes(4, "little")
        cmd[44:48] = cdw11.to_bytes(4, "little")
        cmd[48:52] = cdw12.to_bytes(4, "little")

        slot = q["sq_tail"]
        ok, _ = self.ask(self._id(), "mem.write",
                         {"handle": q["sq"]["handle"], "off": slot * SQ_ENTRY,
                          "bytes": bytes(cmd)})
        if not ok:
            return None, "no se pudo dejar el pedido en la cola"

        # Tocarle el timbre: "hay algo nuevo hasta aca".
        q["sq_tail"] = (slot + 1) % q["entries"]
        self.reg_write(self.bell(qid, 0), q["sq_tail"])

        # Y esperar la respuesta. Se reconoce por un bit que **alterna** en cada
        # vuelta del anillo: no alcanza con mirar si hay algo escrito, porque lo
        # de la vuelta anterior tambien esta escrito.
        deadline = time.time() + 5.0
        while time.time() < deadline:
            ok, r = self.ask(self._id(), "mem.read",
                             {"handle": q["cq"]["handle"], "off": q["cq_head"] * CQ_ENTRY,
                              "len": CQ_ENTRY})
            if not ok:
                return None, "no se pudo mirar la cola de respuestas"
            entry = r["bytes"]
            status = int.from_bytes(entry[14:16], "little")
            if (status & 1) != q["phase"]:
                continue
            q["cq_head"] = (q["cq_head"] + 1) % q["entries"]
            if q["cq_head"] == 0:
                # Dio la vuelta: de aca en adelante el bit vale al reves.
                q["phase"] ^= 1
            # Y decirle hasta donde se leyo, o la cola se llena y se traba.
            self.reg_write(self.bell(qid, 1), q["cq_head"])
            code = status >> 1
            if code:
                return None, f"el controlador rechazo el comando: estado {code:#x}"
            self.tag += 1
            return entry, None
        return None, "el controlador no contesto en 5 s"

    def admin(self, *args, **kw):
        """Atajo: la cola de administracion es siempre la cero."""
        return self.command(0, *args, **kw)

    def identify(self):
        """Le pregunta al controlador quien es. Es el primer comando de todos."""
        ok, buf = self.ask(self._id(), "mem.claim", {"bytes": 4096, "align": 4096})
        if not ok:
            return None, f"sin memoria para la respuesta: {buf}"
        self.ask(self._id(), "mem.write", {"handle": buf["handle"], "bytes": bytes(4096)})
        # El aparato escribe ahi por su cuenta, asi que hay que declararlo.
        ok, e = self.ask(self._id(), "dma.allow",
                         {"device": self.bdf, "handle": buf["handle"]})
        if not ok:
            return None, f"el IOMMU no dejo declarar el buffer: {e}"

        # Opcode 6 = Identify. cdw10 = 1 pide la ficha del controlador.
        _, why = self.admin(0x06, prp1=buf["start"], cdw10=1)
        if why:
            self.ask(self._id(), "release", {"handle": buf["handle"]})
            return None, why

        ok, r = self.ask(self._id(), "mem.read",
                         {"handle": buf["handle"], "off": 0, "len": 128})
        self.ask(self._id(), "release", {"handle": buf["handle"]})
        if not ok:
            return None, "no se pudo leer lo que dejo"
        data = r["bytes"]
        return {
            "vendor": int.from_bytes(data[0:2], "little"),
            # Vienen rellenados con espacios a la derecha, no terminados en cero.
            "serial": data[4:24].decode("ascii", "replace").strip(),
            "model": data[24:64].decode("ascii", "replace").strip(),
        }, None


    def io_queue(self, qid=1, entries=4):
        """Crea una cola de datos. Las de administracion no leen discos.

        Son dos comandos y el orden importa: primero la de respuestas, porque la
        de pedidos se crea diciendo a cual contesta. Al reves, el controlador
        rechaza el segundo comando.
        """
        cq, why = self.shared_page()
        if why:
            return f"sin memoria para su cola de respuestas: {why}"
        # Opcode 5 = Create I/O Completion Queue. El bit 0 de cdw11 dice que la
        # cola es un bloque contiguo, que es lo que acabamos de reclamar.
        _, why = self.admin(0x05, prp1=cq["start"],
                            cdw10=((entries - 1) << 16) | qid, cdw11=1)
        if why:
            return f"no acepto crear la cola de respuestas: {why}"

        sq, why = self.shared_page()
        if why:
            return f"sin memoria para su cola de pedidos: {why}"
        # Opcode 1 = Create I/O Submission Queue. En cdw11 va, arriba, a que
        # cola de respuestas le contesta.
        _, why = self.admin(0x01, prp1=sq["start"],
                            cdw10=((entries - 1) << 16) | qid, cdw11=(qid << 16) | 1)
        if why:
            return f"no acepto crear la cola de pedidos: {why}"

        self.queues[qid] = {"sq": sq, "cq": cq, "entries": entries,
                            "sq_tail": 0, "cq_head": 0, "phase": 1}
        return None

    def namespace(self, nsid=1):
        """Cuanto mide el disco y de que tamano son sus bloques."""
        buf, why = self.shared_page()
        if why:
            return None, why
        # Opcode 6 = Identify, cdw10 = 0 pide la ficha de un namespace.
        _, why = self.admin(0x06, nsid=nsid, prp1=buf["start"], cdw10=0)
        if why:
            return None, why
        ok, r = self.ask(self._id(), "mem.read",
                         {"handle": buf["handle"], "off": 0, "len": 200})
        if not ok:
            return None, "no se pudo leer la ficha"
        data = r["bytes"]
        blocks = int.from_bytes(data[0:8], "little")
        # Cual de los formatos esta en uso, y de ahi el tamano de bloque: viene
        # como potencia de dos, no como numero de bytes.
        which = data[26] & 0xF
        lbaf = data[128 + which * 4: 132 + which * 4]
        shift = lbaf[2]
        return {"blocks": blocks, "block_bytes": 1 << shift}, None

    def write_from(self, claim, off, lba, count, ns, nsid=1, qid=1):
        """Escribe bloques al disco desde memoria del agente.

        Es el mismo camino que `read_into` al reves, y por eso comparte todo:
        misma cola, mismo DMA, mismo timbre. Lo unico que cambia es el opcode y
        hacia donde van los bytes — aca el aparato **lee** de la memoria del
        agente en vez de escribirla, pero el permiso del IOMMU es el mismo: se
        declara una vez y sirve para las dos direcciones.
        """
        per_page = 4096 // ns["block_bytes"]
        done = 0
        while done < count:
            chunk = min(per_page, count - done)
            here = claim["start"] + off + done * ns["block_bytes"]
            # Opcode 1 = Write.
            _, why = self.command(qid, 0x01, nsid=nsid, prp1=here,
                                  cdw10=(lba + done) & 0xFFFFFFFF,
                                  cdw11=((lba + done) >> 32) & 0xFFFFFFFF,
                                  cdw12=chunk - 1)
            if why:
                return f"escribiendo el bloque {lba + done}: {why}"
            done += chunk
        return None

    def flush(self, nsid=1, qid=1):
        """Le pide que baje a disco lo que tenga en vuelo.

        Sin esto, "escribi" quiere decir "lo tome", no "esta en el disco". La
        diferencia se ve recien en el proximo arranque, que es el peor momento
        para enterarse.
        """
        _, why = self.command(qid, 0x00, nsid=nsid)
        return why

    def store(self, body, ns, entry=0, nsid=1, qid=1):
        """Graba un payload en el disco, con su cabecera. El reverso de `load`.

        Esto es lo que cierra el ciclo: un agente sube un programa por el cable,
        lo deja grabado, y en el proximo arranque el cargador lo encuentra sin
        que haya nadie del otro lado.
        """
        block = ns["block_bytes"]
        header = payload_header(body, entry)
        # Cabecera y payload de una: un solo reclamo, y se escribe seguido.
        padded = bytes(header) + body + bytes((-len(body)) % block)
        room = (len(padded) + 4095) // 4096 * 4096
        ok, claim = self.ask(self._id(), "mem.claim", {"bytes": room, "align": 4096})
        if not ok:
            return f"sin memoria para armar lo que se graba: {claim}"
        self.claims.append(claim)
        ok, e = self.ask(self._id(), "dma.allow",
                         {"device": self.bdf, "handle": claim["handle"]})
        if not ok:
            return f"el IOMMU no dejo declarar el origen: {e}"

        # Subir los bytes por el cable a esa memoria, y de ahi al disco por DMA.
        ok, _ = self.ask(self._id(), "mem.write",
                         {"handle": claim["handle"], "off": 0, "bytes": padded})
        if not ok:
            return "no se pudo dejar en memoria lo que se va a grabar"

        blocks = len(padded) // block
        if (why := self.write_from(claim, 0, 0, blocks, ns, nsid=nsid, qid=qid)):
            return why
        if (why := self.flush(nsid=nsid, qid=qid)):
            return f"no se pudo bajar a disco: {why}"
        return None

    def read_into(self, claim, off, lba, count, ns, nsid=1, qid=1):
        """Lee bloques directo a un reclamo del agente, sin pasar por el cable.

        Es lo que hace que el cargador tenga sentido: el aparato escribe en la
        memoria del agente por DMA, y nadie mueve esos bytes a mano.

        Con una sola direccion en el comando se llega hasta una pagina, asi que
        se parte de a paginas. Una lista de punteros permitiria mas por comando
        y no hace falta todavia: se dice en vez de fingir que se lee de una.
        """
        per_page = 4096 // ns["block_bytes"]
        done = 0
        while done < count:
            chunk = min(per_page, count - done)
            here = claim["start"] + off + done * ns["block_bytes"]
            _, why = self.command(qid, 0x02, nsid=nsid, prp1=here,
                                  cdw10=(lba + done) & 0xFFFFFFFF,
                                  cdw11=((lba + done) >> 32) & 0xFFFFFFFF,
                                  cdw12=chunk - 1)
            if why:
                return f"leyendo el bloque {lba + done}: {why}"
            done += chunk
        return None

    def load(self, ns, nsid=1):
        """Trae el payload del disco a memoria. Esto es el cargador de D19.

        Devuelve el reclamo donde quedo, listo para que alguien salte ahi.
        """
        head, why = self.read(0, 1, nsid=nsid)
        if why:
            return None, f"no se pudo leer la cabecera: {why}"
        if head[0:8] != HEADER_MAGIC:
            return None, f"el bloque 0 no es una cabecera: {head[0:8]!r}"
        version = int.from_bytes(head[8:12], "little")
        if version != HEADER_VERSION:
            # Negarse es lo correcto: un formato que no se entiende leido como
            # si se entendiera termina en un salto a cualquier lado.
            return None, f"formato {version}, y este cargador sabe el {HEADER_VERSION}"
        size = int.from_bytes(head[16:24], "little")
        entry = int.from_bytes(head[24:32], "little")
        want = int.from_bytes(head[32:40], "little")
        if size == 0:
            return None, "la cabecera dice que el payload esta vacio"
        if entry >= size:
            return None, "la entrada cae fuera del payload"

        # Memoria para el payload. Va sin `user`: el payload corre con
        # privilegio completo, y el silicio no deja que una pagina sea del
        # agente y ejecutable por el kernel a la vez (D27).
        room = (size + 4095) // 4096 * 4096
        ok, claim = self.ask(self._id(), "mem.claim", {"bytes": room, "align": 4096})
        if not ok:
            return None, f"sin memoria para el payload: {claim}"
        self.claims.append(claim)
        ok, e = self.ask(self._id(), "dma.allow",
                         {"device": self.bdf, "handle": claim["handle"]})
        if not ok:
            return None, f"el IOMMU no dejo declarar el destino: {e}"

        blocks = (size + ns["block_bytes"] - 1) // ns["block_bytes"]
        if (why := self.read_into(claim, 0, PAYLOAD_LBA, blocks, ns, nsid=nsid)):
            return None, why

        # Y comprobar que llego entero. Sin esto, media lectura se veria como
        # una lectura buena hasta que el salto termina en cualquier lado.
        ok, r = self.ask(self._id(), "mem.read",
                         {"handle": claim["handle"], "off": 0, "len": size})
        if not ok:
            return None, "no se pudo releer lo cargado"
        got = sum(r["bytes"]) & 0xFFFFFFFFFFFFFFFF
        if got != want:
            return None, f"la suma no da: la cabecera dice {want:#x} y salio {got:#x}"
        return {"claim": claim, "size": size, "entry": entry}, None

    def read(self, lba, count, nsid=1, qid=1):
        """Lee bloques del disco. Esto es, al fin, lo que el cargador necesita."""
        ns, why = self.namespace(nsid)
        if why:
            return None, f"no dijo como es el disco: {why}"
        size = ns["block_bytes"] * count
        if size > 4096:
            # Con una sola direccion se llega hasta dos paginas; mas necesita una
            # lista, y el cargador todavia no la necesita. Se dice en vez de
            # leer de menos y callarse.
            return None, f"{size} bytes no entran en una pagina"
        buf, why = self.shared_page()
        if why:
            return None, why

        # Opcode 2 = Read. El bloque va partido en dos palabras de 32 bits, y
        # cdw12 lleva **cuantos menos uno**: pedir cero bloques es pedir uno.
        _, why = self.command(qid, 0x02, nsid=nsid, prp1=buf["start"],
                              cdw10=lba & 0xFFFFFFFF, cdw11=(lba >> 32) & 0xFFFFFFFF,
                              cdw12=count - 1)
        if why:
            return None, why
        ok, r = self.ask(self._id(), "mem.read",
                         {"handle": buf["handle"], "off": 0, "len": size})
        if not ok:
            return None, "no se pudo leer lo que dejo"
        return r["bytes"], None


# Lo que se escribe en el disco para poder comprobar que se leyo de verdad.
#
# Un disco recien creado son ceros, y leer ceros de un disco de ceros se ve
# **igual** que no leer nada. Este proyecto ya pago esa moneda con el IOMMU: hay
# que comprobar que la cosa que se prueba ocurre.
PAYLOAD_MAGIC = b"KORNELIA-PAYLOAD"

# --- El formato del payload en el disco -------------------------------------
#
# D19 dice que el blob es un cargador y que el resto vive "en el bloque tal".
# Esto es ese acuerdo, y es lo mas chico que puede ser: una cabecera en el
# bloque 0 y el payload a continuacion.
#
# No hay sistema de archivos y no es una carencia: un cargador que entiende
# FAT32 es mucho mas grande que uno que lee bloques por numero, y el payload lo
# escribe el mismo que escribe el cargador. Nombres de archivo no hacen falta
# cuando hay una sola cosa que traer.
HEADER_MAGIC = b"KORNELIA"
HEADER_VERSION = 1
# La cabecera entra en un bloque, y el payload arranca en el siguiente.
PAYLOAD_LBA = 1


def payload_header(body, entry=0):
    """La cabecera del bloque 0: que hay, cuanto mide, y por donde se empieza."""
    h = bytearray(512)
    h[0:8] = HEADER_MAGIC
    h[8:12] = HEADER_VERSION.to_bytes(4, "little")
    h[16:24] = len(body).to_bytes(8, "little")
    h[24:32] = entry.to_bytes(8, "little")
    # Una suma, no un hash: alcanza para distinguir "se leyo entero" de "se
    # leyo la mitad", que es lo unico que puede fallar aca. Un disco no miente
    # a proposito.
    h[32:40] = (sum(body) & 0xFFFFFFFFFFFFFFFF).to_bytes(8, "little")
    return bytes(h)


def payload_image(blocks=4, block_bytes=512, body=None):
    """El disco entero: cabecera, payload, y despues un patron reconocible.

    El patron es distinto en cada bloque a proposito: si fueran todos iguales,
    pedir el bloque 5 y recibir el 0 pasaria la prueba igual.
    """
    body = body if body is not None else b""
    out = bytearray()
    out += payload_header(body)
    # El payload, redondeado a bloque.
    padded = body + bytes((-len(body)) % block_bytes)
    out += padded
    # Y el relleno reconocible, para las pruebas de lectura cruda.
    for n in range(blocks):
        block = bytearray(block_bytes)
        block[0:len(PAYLOAD_MAGIC)] = PAYLOAD_MAGIC
        block[16:20] = n.to_bytes(4, "little")
        for i in range(20, block_bytes):
            block[i] = (n * 31 + i) & 0xFF
        out += block
    return bytes(out)


def payload_pattern_lba(body=b"", block_bytes=512):
    """En que bloque empieza el patron reconocible, despues del payload."""
    return 1 + (len(body) + block_bytes - 1) // block_bytes


def test_persist(proc, timeout, arch, only_check=False):
    """El ciclo entero: grabar un programa en el disco y correr el que estaba.

    Es la prueba de la persistencia, y esta hecha para que **no pueda pasar por
    casualidad**: se graba un programa distinto del que ya habia, se lo relee
    del disco, y recien despues se comprueba. Si la escritura no ocurriera, lo
    que se relee seria el viejo y daria el valor viejo.
    """
    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, d = ask_verb(500, "describe", {"what": ["pcie"]})
    if not ok or not d["pcie"]:
        print("  esta maquina no informa PCIe")
        return 1

    core = core_for_raw(ask_verb, 501)
    nvme = Nvme(ask_verb, 502)
    if (why := nvme.find(d["pcie"]["base"])):
        print(f"  {why}")
        return 1
    spec = nvme.describe()
    if (why := nvme.start(spec)) or (why := nvme.io_queue()):
        print(f"  FALLA: {why}")
        nvme.release()
        return 1
    ns, why = nvme.namespace()
    if why:
        print(f"  FALLA: {why}")
        nvme.release()
        return 1

    # Un programa **distinto** del que el disco ya tiene: deja 0xbeef en vez de
    # 0xc0ffee. Si la escritura no ocurriera, al releer saldria el viejo.
    body = PROGRAMS[arch]["otro"]
    if only_check:
        # Segunda mitad de la prueba: esta es una maquina **recien arrancada**,
        # y lo unico que hay en el disco es lo que grabo la corrida anterior.
        # Aca no se escribe nada: si sale 0xbeef, sobrevivio al reinicio.
        print("  sin grabar nada: se lee lo que dejo el arranque anterior")
    else:
        if (why := nvme.store(body, ns)):
            print(f"  FALLA al grabar: {why}")
            nvme.release()
            return 1
        print(f"  grabados {len(body)} bytes en el disco, con su cabecera")

    # Y ahora se lo relee **del disco**, como lo haria el cargador en el proximo
    # arranque. Nada de esto mira lo que quedo en memoria.
    loaded, why = nvme.load(ns)
    if why:
        print(f"  FALLA al releer lo grabado: {why}")
        nvme.release()
        return 1
    print(f"  releidos del disco: {loaded['size']} bytes")

    if core is None:
        print("  (sin un nucleo libre donde correrlo)")
        nvme.release()
        return 0
    ok, out = ask_verb(520, "exec", {"handle": loaded["claim"]["handle"],
                                     "mode": "raw", "core": core,
                                     "off": loaded["entry"], "deadline_ms": 1000})
    if not ok or out.get("faulted"):
        print(f"  FALLA al correr lo grabado: {out}")
        nvme.release()
        return 1
    first = list(out["registers"].values())[0] if out.get("registers") else 0
    print(f"  y corre lo que se grabo: dejo {first:#x}")
    nvme.release()
    if first != 0xBEEF:
        print("  FALLA: corrio el programa viejo, asi que no se grabo nada")
        return 1
    print("\n  persistencia: ok")
    return 0


def test_nvme(proc, timeout, arch):
    """Le habla a un controlador NVMe de verdad, con los once verbos y nada mas.

    Es la primera prueba de que la superficie del kernel alcanza para escribir
    un driver: no hay un verbo `disco`, hay memoria, permiso de DMA y registros.
    """
    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, d = ask_verb(400, "describe", {"what": ["pcie"]})
    if not ok or not d["pcie"]:
        print("  esta maquina no informa PCIe")
        return 1

    # El nucleo donde va a correr el payload, **antes de reclamar nada**.
    #
    # En x86_64 el trampolin que arranca un nucleo pasa por una pagina baja y
    # fija, y todo lo que reclama el driver cae justo ahi: las colas, el buffer
    # y el payload. Pedirlo despues es pedirle al kernel que arranque un nucleo
    # con esa pagina ya tomada. El kernel lo dice claro
    # (`trampoline-page-taken`) desde que se distingue de "esta maquina no sabe
    # arrancar nucleos", pero el orden correcto evita el problema entero.
    core = core_for_raw(ask_verb, 450)

    nvme = Nvme(ask_verb, 401)
    if (why := nvme.find(d["pcie"]["base"])):
        print(f"  {why}")
        return 1
    print(f"  controlador encontrado: el bus lo llama {nvme.bdf:#x}")

    spec = nvme.describe()
    print(f"  dice ser NVMe {spec['version']}, colas de hasta {spec['max_entries']}"
          f" entradas, paginas de {spec['page_bytes']} bytes")
    print(f"  y que puede tardar hasta {spec['timeout_ms']} ms en estar listo")

    if (why := nvme.start(spec)):
        print(f"  FALLA: {why}")
        nvme.release()
        return 1
    print("  apagado, configurado y prendido: dice estar listo")

    # Y ahora la prueba de que las colas andan: un comando de verdad, ida y
    # vuelta. Que conteste prueba tres cosas de una — que leyo el pedido de la
    # cola, que el IOMMU lo dejo, y que escribio la respuesta donde debia.
    who, why = nvme.identify()
    if why:
        print(f"  FALLA: {why}")
        nvme.release()
        return 1
    print(f"  y contesta quien es: modelo {who['model']!r}, "
          f"serie {who['serial']!r}, fabricante {who['vendor']:#06x}")

    # Y ahora lo que el cargador necesita: una cola de datos y leer del disco.
    if (why := nvme.io_queue()):
        print(f"  FALLA: {why}")
        nvme.release()
        return 1
    ns, why = nvme.namespace()
    if why:
        print(f"  FALLA: {why}")
        nvme.release()
        return 1
    size = ns["blocks"] * ns["block_bytes"]
    print(f"  el disco tiene {ns['blocks']} bloques de {ns['block_bytes']} bytes"
          f" ({size // (1 << 20)} MiB)")

    # Leer y **comprobar que dice lo que se escribio**. Sin esto la prueba
    # pasaria con un disco de ceros aunque no se leyera nada, que es la misma
    # moneda que ya se pago con el IOMMU.
    head, why = nvme.read(0, 1)
    if why:
        print(f"  FALLA al leer el bloque 0: {why}")
        nvme.release()
        return 1

    if head == bytes(len(head)):
        # Sin payload el disco son ceros, y de ahi no se puede concluir nada.
        print("  el disco esta vacio, asi que leer no prueba nada:"
              " correr con PAYLOAD= para comprobarlo de verdad")
        nvme.release()
        print("\n  nvme: ok")
        return 0
    if head[0:8] != HEADER_MAGIC:
        print(f"  FALLA: el bloque 0 trae algo que nadie escribio: {head[:16].hex()}")
        nvme.release()
        return 1

    # El patron reconocible vive despues del payload. Que cada bloque sea
    # distinto es parte de la prueba: si devolviera siempre el mismo, "leyo
    # algo" pasaria por "leyo el que se le pidio".
    body = PROGRAMS[arch]["ok"]
    first = payload_pattern_lba(body, ns["block_bytes"])
    expected = payload_image(blocks=4, block_bytes=ns["block_bytes"], body=body)
    for n in (0, 2):
        lba = first + n
        got, why = nvme.read(lba, 1)
        if why:
            print(f"  FALLA al leer el bloque {lba}: {why}")
            nvme.release()
            return 1
        want = expected[lba * ns["block_bytes"]: (lba + 1) * ns["block_bytes"]]
        if got != want:
            print(f"  FALLA: pedido el bloque {lba}, vino otra cosa: {got[:20]!r}")
            nvme.release()
            return 1
    print(f"  y los bloques {first} y {first + 2} traen lo que se escribio,"
          f" cada uno el suyo")

    # Y ahora el cargador entero: traer el payload del disco y **correrlo**.
    # Esto es D19 de punta a punta.
    loaded, why = nvme.load(ns)
    if why:
        print(f"  no se pudo cargar un payload: {why}")
        nvme.release()
        # Sin payload no hay nada que cargar, y eso no es una falla del driver.
        print("\n  nvme: ok")
        return 0
    print(f"  payload cargado: {loaded['size']} bytes en"
          f" {loaded['claim']['start']:#x}, entrada en +{loaded['entry']:#x}")

    # Correrlo necesita un nucleo propio: en el del protocolo manda el kernel y
    # solo se corre supervisado (D29), y este codigo vuelve con `ret`.
    if core is None:
        print("  (sin un nucleo libre donde correrlo)")
        nvme.release()
        print("\n  nvme: ok")
        return 0
    ok, out = ask_verb(451, "exec", {"handle": loaded["claim"]["handle"],
                                     "mode": "raw", "core": core,
                                     "off": loaded["entry"], "deadline_ms": 1000})
    if not ok or out.get("faulted"):
        print(f"  FALLA al saltar al payload: {out}")
        nvme.release()
        return 1
    first = list(out["registers"].values())[0] if out.get("registers") else 0
    print(f"  y corrio: dejo {first:#x}")
    if first != 0xC0FFEE:
        print(f"  FALLA: el payload del disco no dejo lo que tenia que dejar")
        nvme.release()
        return 1
    ask_verb(452, "release", {"handle": core})

    nvme.release()
    print("\n  nvme: ok")
    return 0


# El aparato `edu` de QEMU: un motor de DMA que se maneja con cuatro escrituras.
# Existe para ensenar, y por eso sirve justo para esto — cualquier otra placa
# con DMA necesitaria un driver entero antes de poder probar nada.
EDU_ID = 0x11E81234           # dispositivo y fabricante, como vienen juntos
EDU_VERSION = 0x010000ED      # lo que dice su primer registro, ya en el BAR
EDU_INTERNAL = 0x40000        # su memoria interna, del lado del aparato
EDU_DMA_SRC = 0x80
EDU_DMA_DST = 0x88
EDU_DMA_COUNT = 0x90
EDU_DMA_CMD = 0x98
EDU_DMA_START = 0x1           # bit 0: arrancar
EDU_DMA_TO_RAM = 0x2          # bit 1: del aparato hacia la RAM


def test_dma(proc, timeout, arch):
    """El IOMMU hace cumplir lo que el agente declaro (D8).

    Un aparato que hace DMA escribe en la RAM por su cuenta: no pasa por el CPU
    ni mira las tablas de paginas. Sin IOMMU, un puntero mal puesto no da fault
    — da memoria distinta, en silencio.

    La prueba es la unica que vale: se le pide al aparato que escriba en una
    direccion, **sin haberlo declarado**, y la memoria tiene que quedar intacta.
    Despues se declara con `dma.allow` y la misma escritura tiene que llegar.
    """
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, d = ask_verb(100, "describe", {"what": ["iommu", "pcie"]})
    if not ok or d["iommu"] is None:
        print("  dma: sin iommu en esta maquina, no hay nada que hacer cumplir")
        return 0
    print(f"  iommu: {d['iommu']['kind']} en {d['iommu']['address']:#x}")
    if not d["iommu"]["enabled"]:
        # La maquina tiene IOMMU y el kernel todavia no se lo programa. Decirlo
        # es la mitad del punto: un agente que crea que hay una garantia que no
        # hay escribe drivers contra una suposicion falsa (P4).
        print("  dma: el kernel todavia no programa este iommu, asi que no traduce")
        return 0
    if d["pcie"] is None:
        print("  la maquina no informa PCIe")
        return 1
    ecam = d["pcie"]["base"]

    # Buscar el aparato recorriendo el bus, que es lo que haria el agente: el
    # kernel publica donde se configura PCIe, no que hay conectado (D4).
    ok, cfg = ask_verb(101, "mem.claim", {"at": ecam, "bytes": 1 << 20})
    if not ok:
        print(f"  no se pudo mirar el bus: {cfg}")
        return 1
    bdf, slot = None, None
    for dev in range(32):
        off = dev << 15
        ok, r = ask_verb(102, "mem.read", {"handle": cfg["handle"], "off": off, "len": 4})
        if ok and int.from_bytes(r["bytes"], "little") == EDU_ID:
            bdf, slot = dev << 3, off
            break
    if bdf is None:
        print("  no hay un aparato con DMA en este bus")
        return 0
    print(f"  aparato encontrado: el bus lo llama {bdf:#x}")

    # Su ventana de registros, y permiso para que sea maestro del bus — sin eso
    # el aparato no puede iniciar un DMA y la prueba no probaria nada.
    ok, r = ask_verb(103, "mem.read", {"handle": cfg["handle"], "off": slot + 0x10, "len": 4})
    bar = int.from_bytes(r["bytes"], "little") & ~0xF
    ask_verb(104, "mem.write", {"handle": cfg["handle"], "off": slot + 4,
                                "bytes": bytes([0x06, 0x00])})
    print(f"  sus registros estan en {bar:#x}")

    # Y se le puede reclamar esa ventana, aunque el firmware no la haya listado
    # en el mapa (deuda 15). Antes se rechazaba con `unmapped`, asi que el
    # agente solo podia tocar los registros desde su codigo en `exec` — un
    # rodeo que no protegia nada, porque el identity map ya los cubria.
    #
    # La prueba no es que el kernel acepte: es **leer un registro del aparato**
    # con `mem.read` y que diga lo que tiene que decir. El de identificacion
    # trae el mismo numero que se encontro recorriendo el bus.
    ok, win = ask_verb(119, "mem.claim", {"at": bar, "bytes": 4096})
    if not ok:
        failures.append(f"no se pudo reclamar la ventana de registros: {win}")
    else:
        print(f"  y su ventana se puede reclamar: clase '{win['kind']}'")

        # Y hay que pedir el ancho: este registro solo acepta accesos de cuatro
        # bytes y descarta los mas angostos. Leerlo de a un byte devuelve ceros
        # sin avisar, que es la peor forma de fallar — por eso el ancho lo
        # declara el agente y el kernel no lo adivina.
        ok, r = ask_verb(120, "mem.read",
                         {"handle": win["handle"], "off": 0, "len": 4, "width": 4})
        got = int.from_bytes(r["bytes"], "little") if ok else 0
        print(f"  y leerle un registro de 4 bytes da: {got:#x}")
        if got != EDU_VERSION:
            failures.append(f"el registro del aparato dio {got:#x} y no {EDU_VERSION:#x}")

        # Y leerlo mal a proposito, que es lo que hace que el ancho no sea
        # decorativo. **Las dos maquinas no fallan igual, y las dos tienen que
        # sobrevivir** (P5): en x86_64 el acceso angosto se descarta y devuelve
        # ceros; en aarch64 el bus lo rechaza con un abort externo que antes
        # dejaba la maquina muda, porque `mem.read` corre en el camino del
        # protocolo. Ahora vuelve como respuesta.
        ok, r = ask_verb(122, "mem.read", {"handle": win["handle"], "off": 0, "len": 4})
        if not ok:
            print(f"  y de a un byte, la maquina lo rechaza y lo dice: {r}")
            if r.get("error") != "access-refused":
                failures.append(f"el acceso rechazado no se informa como tal: {r}")
        else:
            narrow = int.from_bytes(r["bytes"], "little")
            print(f"  y de a un byte, el mismo registro da: {narrow:#x}")
            if narrow == got:
                failures.append("el ancho no cambio nada: la prueba no prueba nada")

        # Lo que no admite interpretacion: despues de eso la maquina contesta.
        ok, _ = ask_verb(123, "describe", {})
        if not ok:
            failures.append("la maquina dejo de contestar despues del acceso rechazado")
        else:
            print("  y despues de eso la maquina sigue contestando")

    # La memoria donde el aparato va a intentar escribir, con un patron puesto
    # por el CPU: si el DMA llega, lo pisa con ceros.
    ok, buf = ask_verb(105, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar memoria: {buf}")
        return 1
    pattern = bytes([0xAA] * 8)
    print(f"  la memoria del agente esta en {buf['start']:#x}")



    # Los registros del aparato no hace falta reclamarlos: el codigo del agente
    # les escribe directo, que es como se maneja cualquier placa (D4, D12).
    code = emit_writes(arch, [
        (bar + EDU_DMA_SRC, EDU_INTERNAL, 8),
        (bar + EDU_DMA_DST, buf["start"], 8),
        (bar + EDU_DMA_COUNT, 8, 8),
        (bar + EDU_DMA_CMD, EDU_DMA_START | EDU_DMA_TO_RAM, 8),
    ])
    ok, prog = ask_verb(109, "mem.claim", {"bytes": 4096, "align": 4096})
    ask_verb(110, "mem.write", {"handle": prog["handle"], "bytes": code})

    # Escribirle a los registros del aparato es privilegiado, asi que va a un
    # nucleo reclamado (D29).
    core = core_for_raw(ask_verb, 108)
    if core is None:
        print("  no hay un nucleo donde correr con privilegio")
        return 1

    def try_dma(label, handle=None):
        handle = handle or buf["handle"]
        ask_verb(111, "mem.write", {"handle": handle, "bytes": pattern})
        ok, r = ask_verb(112, "exec", {"handle": prog["handle"], "mode": "raw", "core": core})
        if not ok or r.get("faulted"):
            failures.append(f"{label}: el codigo que toca el aparato fallo: {r}")
            return None
        # El DMA no es inmediato: el aparato lo hace por su cuenta. Se le
        # **pregunta a el** si termino —limpia el bit de arranque al terminar—
        # en vez de darle un rato y esperar que alcance. Antes esto eran veinte
        # pedidos cualquiera, y funcionaba hasta que la prueba se hizo un poco
        # mas lenta: una espera medida en "un rato" es una prueba que falla o
        # pasa por motivos que no tienen que ver con lo que prueba.
        for _ in range(200):
            ok, r = ask_verb(113, "mem.read",
                             {"handle": win["handle"], "off": EDU_DMA_CMD,
                              "len": 8, "width": 8})
            if not ok or not int.from_bytes(r["bytes"], "little") & EDU_DMA_START:
                break
        ok, r = ask_verb(114, "mem.read", {"handle": handle, "off": 0, "len": 8})
        return r["bytes"] if ok else None

    # 1. Sin declarar nada: el aparato no tiene que llegar.
    got = try_dma("sin permiso")
    print(f"  sin declarar nada, la memoria quedo: {got.hex() if got else '?'}")
    if got != pattern:
        failures.append("el aparato escribio en memoria que nadie le permitio")

    # Y el intento negado no se pierde: el silicio lo anota, y por eso el agente
    # puede enterarse de que su driver apunto a donde no debia (P5).
    ok, d = ask_verb(118, "describe", {"what": ["iommu"]})
    if ok and d["iommu"]["faults"]:
        print(f"  y el iommu lo anoto: faults={d['iommu']['faults']:#x}")
    else:
        failures.append("el iommu nego el acceso pero no lo anoto")

    # 2. Declarado: la misma escritura tiene que llegar.
    ok, r = ask_verb(115, "dma.allow", {"device": bdf, "handle": buf["handle"]})
    if not ok:
        print(f"  FALLA: dma.allow: {r}")
        return 1
    print(f"  declarado: el aparato {bdf:#x} puede tocar {r['bytes']} bytes en {r['start']:#x}")

    got = try_dma("con permiso")
    print(f"  y ahora la memoria quedo:       {got.hex() if got else '?'}")
    if got == pattern:
        failures.append("el aparato no llego a la memoria que si se le permitio")

    # 3. Y al soltar el reclamo, el permiso se saca: memoria devuelta que un
    #    aparato sigue alcanzando es el agujero que todo esto viene a cerrar.
    ask_verb(116, "release", {"handle": buf["handle"]})
    ok, v = ask_verb(117, "mem.claim", {"at": buf["start"], "bytes": 4096})
    if ok:
        got = try_dma("despues de soltar", v["handle"])
        ask_verb(121, "release", {"handle": win["handle"]})
        print(f"  y despues de soltarla:          {got.hex() if got else '?'}")
        if got != pattern:
            failures.append("al soltar el reclamo no se le saco el permiso al aparato")

    # Soltar la ventana de configuracion. **No es prolijidad**: la siguiente
    # prueba que quiera mirar el bus se encuentra con `already-claimed` y falla
    # por un motivo que no tiene nada que ver con lo que prueba. Este proyecto ya
    # pago esa moneda una vez.
    ask_verb(122, "release", {"handle": cfg["handle"]})

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  dma: ok")
    return 0


def test_handler_during_exec(proc, timeout, arch):
    """El handler del agente corre DURANTE un exec largo (D9, D29).

    La prueba es de las que no admiten interpretacion: el codigo del agente
    dispara su interrupcion y despues se queda esperando a que su propio handler
    le escriba una bandera. Si las interrupciones estuvieran cerradas durante el
    exec, esa espera no terminaria nunca y esto colgaria.
    """
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    # Otra interrupcion que la que usa --handler: son dos pruebas que pueden
    # correr en el mismo arranque, y un handler ya instalado no se reemplaza.
    INT = 35 if arch == "aarch64" else 6
    FLAG_OFF = 2048

    ok, c = ask_verb(70, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar: {c}")
        return 1
    h, base = c["handle"], c["start"]
    flag = base + FLAG_OFF

    # El handler: escribe 1 en la bandera y vuelve.
    if arch == "x86_64":
        handler = b"\x48\xb8" + flag.to_bytes(8, "little") + b"\xc6\x00\x01\xc3"
    else:
        handler = mov_reg_imm64(0, flag) + \
                  (0x52800021).to_bytes(4, "little") + \
                  (0x39000001).to_bytes(4, "little") + \
                  (0xD65F03C0).to_bytes(4, "little")
    ask_verb(71, "mem.write", {"handle": h, "off": 0, "bytes": handler})
    ask_verb(72, "mem.write", {"handle": h, "off": FLAG_OFF, "bytes": b"\x00"})

    ok, r = ask_verb(73, "irq.install", {"handle": h, "interrupt": INT})
    if not ok:
        print(f"  no se pudo instalar el handler: {r}")
        return 1

    ok, d = ask_verb(74, "describe", {"what": ["handlers"]})
    # El de esta prueba, que puede no ser el unico instalado.
    hh = next(x for x in d["handlers"] if x["interrupt"] == INT)
    before = hh["served"]

    # El codigo del agente: disparar y esperar la bandera. Sin el `ret` del
    # disparo, porque despues viene la espera — y el largo del `ret` no es el
    # mismo en las dos arquitecturas, asi que se pide sin el en vez de recortarlo.
    disparo = emit_writes(arch, hh["trigger"], con_ret=False)
    if arch == "x86_64":
        wait = b"\x48\xb8" + flag.to_bytes(8, "little")   # mov rax, flag
        wait += b"\x80\x38\x00"                          # cmp byte [rax], 0
        wait += b"\x74\xfb"                               # je -5
        wait += b"\xc3"                                    # ret
    else:
        wait = mov_reg_imm64(0, flag)
        wait += (0x39400001).to_bytes(4, "little")          # ldrb w1, [x0]
        wait += (0x34FFFFC1).to_bytes(4, "little")          # cbz w1, -8
        wait += (0xD65F03C0).to_bytes(4, "little")          # ret

    ok, c2 = ask_verb(75, "mem.claim", {"bytes": 4096, "align": 4096})
    h2 = c2["handle"]
    ask_verb(76, "mem.write", {"handle": h2, "bytes": disparo + wait})

    # Disparar una interrupcion es escribirle al controlador: privilegiado, asi
    # que va a un nucleo reclamado (D29).
    core = core_for_raw(ask_verb, 79)
    if core is None:
        print("  no hay un nucleo donde correr con privilegio")
        return 1

    print("  el agente dispara su interrupcion y espera a su propio handler...")
    try:
        ok, r = ask_verb(77, "exec", {"handle": h2, "mode": "raw", "core": core})
    except TimeoutError:
        print("  FALLA: el exec no volvio — el handler no corrio durante el exec")
        return 1

    if not ok or r.get("faulted"):
        failures.append(f"el exec fallo: {r}")
    else:
        print("  volvio: el handler corrio mientras el exec seguia")

    ok, d = ask_verb(78, "describe", {"what": ["handlers"]})
    after = -1
    if ok:
        for x in d.get("handlers", []):
            if x["interrupt"] == INT:
                after = x["served"]
    if after <= before:
        failures.append(f"la cuenta no subio: {before} -> {after}")

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  handler durante exec: ok")
    return 0


def test_handler(proc, timeout, arch):
    """El agente pone su codigo a atender una interrupcion (D9)."""
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    # Un numero de interrupcion que la maquina tenga y que no sea del kernel.
    # 34 en ARM es el reloj de tiempo real; 5 en x86 es un cable libre.
    INT = 34 if arch == "aarch64" else 5

    # El handler: un `ret` pelado. Lo unico que se prueba es que el kernel lo
    # llame — lo que haga adentro es asunto del agente (P2).
    ret = (0xD65F03C0).to_bytes(4, "little") if arch == "aarch64" else b"\xc3"

    ok, c = ask_verb(50, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar: {c}")
        return 1
    h = c["handle"]
    ask_verb(51, "mem.write", {"handle": h, "bytes": ret})

    ok, r = ask_verb(52, "irq.install", {"handle": h, "interrupt": INT})
    print(f"  irq.install (interrupcion {INT})  {'ok ' if ok else 'ERROR'} {r}")
    if not ok:
        failures.append(f"no se pudo instalar: {r}")
        return 1

    # El cable del kernel NO se entrega: seria quedarse sin cordon.
    ok, d = ask_verb(53, "describe", {"what": ["interrupts"]})
    cable = d["interrupts"].get("serial")
    if cable and cable.get("gsi"):
        ok, e = ask_verb(54, "irq.install", {"handle": h, "interrupt": cable["gsi"]})
        print(f"  y el cable del kernel:            {'ok ' if ok else 'ERROR'} {e}")
        if ok or e.get("error") != "is-kernel-interrupt":
            failures.append("dejo instalar un handler sobre el cable del kernel")

    # Dos veces la misma tampoco.
    ok, e = ask_verb(55, "irq.install", {"handle": h, "interrupt": INT})
    if ok or e.get("error") != "already-installed":
        failures.append("dejo instalar dos veces la misma interrupcion")

    # Ahora hacerla sonar con codigo del agente, y ver si el kernel la atendio.
    ok, d = ask_verb(56, "describe", {"what": ["handlers"]})
    if not ok or not d.get("handlers"):
        failures.append("el handler no aparece en describe")
        return 1
    # El de esta prueba, buscado por su numero: puede no ser el unico instalado.
    # Tomar "el primero de la lista" andaba solo mientras esta fuera la unica
    # prueba que instala handlers, y dejo de andar apenas hubo otra.
    hh = next((x for x in d["handlers"] if x["interrupt"] == INT), None)
    if hh is None:
        failures.append("el handler no aparece en describe")
        return 1
    before = hh["served"]
    print(f"  atendida {before} veces hasta ahora")

    if not hh["trigger"]:
        failures.append("el kernel no publica como hacerla sonar")
        return 1

    code = emit_writes(arch, hh["trigger"])
    ok, c2 = ask_verb(57, "mem.claim", {"bytes": 4096, "align": 4096})
    h2 = c2["handle"]
    ask_verb(58, "mem.write", {"handle": h2, "bytes": code})

    # Hacerla sonar es escribirle al controlador de interrupciones: va a un
    # nucleo reclamado (D29).
    core = core_for_raw(ask_verb, 56)
    if core is None:
        print("  no hay un nucleo donde correr con privilegio")
        return 1

    print(f"  el agente la hace sonar: {code.hex()}")
    ok, r = ask_verb(59, "exec", {"handle": h2, "mode": "raw", "core": core})
    if not ok or r.get("faulted"):
        failures.append(f"el codigo que la hace sonar fallo: {r}")

    ok, d = ask_verb(60, "describe", {"what": ["handlers"]})
    after = next((x["served"] for x in d.get("handlers") or [] if x["interrupt"] == INT), -1)
    print(f"  y ahora {after} veces")
    if after <= before:
        failures.append("el kernel nunca llamo al handler del agente")

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  handler: ok")
    return 0


def test_mailbox(proc, timeout):
    """Arma el segundo canal en memoria y le manda un pedido por ahi (D17).

    El agente de verdad va a llenar ese buzon desde su driver de red. Aca lo
    llenamos con mem.write, que para el kernel es indistinguible.
    """
    import struct
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    # El acuerdo lo publica el kernel: no se hornea nada de esto.
    ok, ch = ask_verb(30, "describe", {"what": ["channel"]})
    if not ok:
        print("  no se pudo leer el acuerdo del canal")
        return 1
    ch = ch["channel"]
    L = ch["layout"]
    print(f"  el kernel pide magic={ch['magic']:#x} version={ch['version']}")

    CAP = 1024
    ok, c = ask_verb(31, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar memoria: {c}")
        return 1
    h = c["handle"]

    # El pedido que va a viajar por el buzon.
    request = enc([777, "describe", {"what": ["channel"]}])

    # El encabezado, armado con los offsets que publico el kernel.
    cab = bytearray(L["rings"])
    struct.pack_into("<I", cab, L["magic"], ch["magic"])
    struct.pack_into("<I", cab, L["version"], ch["version"])
    struct.pack_into("<I", cab, L["capacity"], CAP)
    struct.pack_into("<I", cab, L["request_head"], len(request))

    ok, _ = ask_verb(32, "mem.write", {"handle": h, "off": 0, "bytes": bytes(cab)})
    if not ok:
        failures.append("no se pudo escribir el encabezado")
    ok, _ = ask_verb(33, "mem.write",
                        {"handle": h, "off": L["rings"], "bytes": request})
    if not ok:
        failures.append("no se pudo escribir el pedido en el anillo")

    # Y el kernel lo adopta.
    ok, r = ask_verb(34, "listen", {"handle": h})
    print(f"  listen     {'ok ' if ok else 'ERROR'} {r}")
    if not ok:
        failures.append(f"listen fallo: {r}")
        return 1

    # Un pedido por el cable, para despertar al nucleo. Todavia no hay timbre
    # propio del buzon: eso es lo que sigue.
    ask_verb(35, "describe", {})

    # Y ahora la pregunta: ¿contesto por el buzon?
    ok, hdr = ask_verb(36, "mem.read", {"handle": h, "off": 0, "len": L["rings"]})
    if not ok:
        failures.append("no se pudo leer el encabezado de vuelta")
        return 1
    cab = hdr["bytes"]
    resp_head = struct.unpack_from("<I", cab, L["response_head"])[0]
    req_tail = struct.unpack_from("<I", cab, L["request_tail"])[0]

    print(f"  el kernel leyo {req_tail} de {len(request)} bytes del pedido")
    print(f"  y dejo {resp_head} bytes de respuesta en el buzon")

    if req_tail != len(request):
        failures.append(f"no consumio el pedido entero: {req_tail}/{len(request)}")
    if resp_head == 0:
        failures.append("no contesto por el buzon")
    else:
        ok, rd = ask_verb(37, "mem.read",
                             {"handle": h, "off": L["rings"] + CAP, "len": resp_head})
        if ok:
            try:
                value, _ = dec(rd["bytes"])
                print(f"  respuesta por el buzon: id={value[0]} ok={value[1]}")
                if value[0] != 777:
                    failures.append(f"el id no es el del pedido del buzon: {value[0]}")
            except Exception as e:
                failures.append(f"la respuesta del buzon no decodifica: {e}")
        else:
            failures.append("no se pudo leer la respuesta del buzon")

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  buzon: ok")
    return 0


# Los atajos de la consola: nombre corto -> (verbo, argumentos por posicion).
# Lo que no este aca se manda igual con `send`, porque el kernel tiene once
# verbos y la consola no puede ser la que decida cuales se pueden pedir.
CONSOLE_VERBS = {
    "describe": ("describe", ["what"]),
    "claim": ("mem.claim", ["bytes"]),
    "read": ("mem.read", ["handle", "off", "len"]),
    "write": ("mem.write", ["handle", "bytes"]),
    "core": ("core.claim", ["id"]),
    "exec": ("exec", ["handle", "mode"]),
    "irq": ("irq.install", ["interrupt", "handle"]),
    "dma": ("dma.allow", ["device", "handle"]),
    "listen": ("listen", ["handle"]),
    "release": ("release", ["handle"]),
}

CONSOLE_HELP = """
La consola corre en TU maquina, no en el kernel: el kernel no tiene shell y no
va a tenerlo. Esto traduce lo que escribis a CBOR y lo manda por el cable, que
es exactamente lo que hace un agente (P3, D1).

  describe                     el indice de lo que la maquina sabe de si misma
  describe memory              una seccion; varias con coma: describe cpus,pcie
  claim 4096 align=4096        reclamar memoria -> devuelve un handle
  claim at=0xfed90000 bytes=4096   reclamar una direccion concreta
  write 1 48c7c0eeffc000c3     subir bytes (en hex) al reclamo 1
  read 1 0 8                   leer 8 bytes del reclamo 1; width=4 si es un registro de aparato
  core 1                       reclamar el nucleo que la maquina llama 1
  exec 1 raw core=2            correr lo que subiste; mode es obligatorio (D27)
  exec 1 supervised ms=100     con plazo declarado
  release 1                    devolver un reclamo o un nucleo
  send exec {"handle":1,"mode":"raw"}   cualquier verbo, con los argumentos crudos

  help / ayuda                 esto
  quit / salir / Ctrl-D        apagar la maquina

Los numeros se pueden escribir 0x... o en decimal. true/false para los booleanos.

Hay DOS recetas y no una, porque el privilegio cambia todo lo demas (D27). Los
handles son los que va devolviendo cada pedido.

Receta A — privilegio completo (`raw`), en un nucleo propio:

  claim 4096 align=4096        SIN `user`: raw no corre memoria del agente
  write 1 48c7c0eeffc000c3     x86_64: mov rax,0xc0ffee ; ret
  core 1                       arranca el nucleo 1 -> devuelve handle 2
  exec 1 raw core=2            OJO: core= lleva el HANDLE, no el id
  release 2
  release 1

Receta B — sin privilegio (`supervised`), aca mismo:

  claim 4096 align=4096 user=true    CON `user`, o supervised no arranca
  write 1 48c7c0eeffc000cd80         ...pero se vuelve con `int 0x80`, no con ret
  exec 1 supervised

Por que no se pueden mezclar: el silicio no deja que una pagina sea alcanzable
por el agente y ejecutable por el kernel a la vez. Asi que `user` sirve para
supervised y estorba para raw, y el kernel lo dice en vez de hacer algo raro.
Y ojo con el `ret` en supervised: desde ahi un retorno comun salta a lo que
haya en la pila y termina en fault — se captura como cualquier otro (P5), pero
el programa no volvio por donde debia.

En aarch64 los bytes son otros: c0fd9fd20018a0f2c0035fd6 para la receta A, y
para la B se cambia el ultimo `ret` por `svc #0` (010000d4).
Y `exec raw` sin `core=` se rechaza a proposito: en el nucleo que atiende el
protocolo manda el kernel, asi que ahi solo corre supervised (D29).
"""


class Attached:
    """Una maquina que ya estaba viva, alcanzada por su socket.

    Presenta lo mismo que un QEMU lanzado por nosotros —`stdin`, `stdout`,
    `kill`— para que el resto del cliente no tenga que saber cual de las dos es.
    La diferencia esta en `kill`: aca solo se corta el cable. La maquina sigue
    andando, que es de lo que se trata (D14).
    """

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.stdin = self.sock.makefile("wb", buffering=0)
        self.stdout = self.sock.makefile("rb", buffering=0)

    def kill(self):
        self.sock.close()

    def wait(self):
        pass


def console_value(token):
    """Traduce lo que escribio un humano al tipo que espera el protocolo."""
    low = token.lower()
    if low in ("true", "si"):
        return True
    if low in ("false", "no"):
        return False
    if low.startswith("0x"):
        return int(token, 16)
    if token.isdigit():
        return int(token)
    if "," in token:
        return [console_value(t) for t in token.split(",")]
    return token


def console(proc, timeout):
    """Una terminal para que un humano le hable al kernel."""
    print(CONSOLE_HELP)
    ident = 1000

    while True:
        try:
            line = input("kornelia> ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            return 0
        if not line:
            continue
        if line in ("quit", "salir", "exit"):
            return 0
        if line in ("help", "ayuda", "?"):
            print(CONSOLE_HELP)
            continue

        words = line.split()
        name, rest = words[0], words[1:]

        # `send` no interpreta nada: verbo y argumentos tal como los escribieron.
        if name == "send":
            if not rest:
                print("  send <verbo> {json}")
                continue
            verb = rest[0]
            try:
                args = json.loads(" ".join(rest[1:])) if len(rest) > 1 else {}
            except ValueError as e:
                print(f"  ese JSON no se entiende: {e}")
                continue
        elif name in CONSOLE_VERBS:
            verb, positional = CONSOLE_VERBS[name]
            args, i = {}, 0
            for token in rest:
                if "=" in token and not token.startswith("0x"):
                    key, _, value = token.partition("=")
                    args[key] = console_value(value)
                elif i < len(positional):
                    args[positional[i]] = console_value(token)
                    i += 1
                elif verb == "mem.write" and "bytes" in args:
                    # Un volcado largo se copia con espacios en el medio. Se
                    # pegan en vez de rechazarlos: son bytes, no palabras.
                    args["bytes"] = f"{args['bytes']}{token}"
                else:
                    print(f"  no se donde va '{token}'; probá con clave=valor")
                    args = None
                    break
            if args is None:
                continue
            # `what` siempre es lista, aunque se pida una sola seccion.
            if verb == "describe" and "what" in args and not isinstance(args["what"], list):
                args["what"] = [args["what"]]
            # Los bytes que se suben van en hex: es codigo maquina, no texto.
            if verb == "mem.write" and isinstance(args.get("bytes"), (str, int)):
                try:
                    args["bytes"] = bytes.fromhex(str(args["bytes"]))
                except ValueError:
                    print("  los bytes de write van en hex, por ejemplo: write 1 c3")
                    continue
        else:
            print(f"  no conozco '{name}'. Probá 'help'.")
            continue

        ident += 1
        try:
            reply, _ = ask(proc, [ident, verb, args], timeout)
        except (TimeoutError, EOFError) as e:
            # No se sale: que la maquina no conteste es un resultado, y de los
            # interesantes. Si murio de verdad, el proximo pedido lo dice igual.
            print(f"  sin respuesta: {e}")
            continue

        _, ok, load = reply
        if not ok:
            print(f"  ERROR: {load}")
        else:
            show(load) if isinstance(load, dict) else print(f"  {load}")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--arch", default="x86_64", choices=["x86_64", "aarch64"])
    ap.add_argument("--what", help="secciones separadas por coma; sin esto pide el indice")
    ap.add_argument("--raw", action="store_true", help="mostrar los bytes que viajan")
    ap.add_argument("--timeout", type=float, default=90.0)
    ap.add_argument("--smp", type=int, default=4,
                    help="cuantos nucleos darle a QEMU")
    ap.add_argument("--no-acpi", action="store_true",
                    help="arranca sin ACPI, para que la maquina se describa por device tree")
    ap.add_argument("--kvm", action="store_true",
                    help="que el codigo lo ejecute el silicio de verdad, no la emulacion")
    ap.add_argument("--write-payload", metavar="RUTA",
                    help="escribe un payload de prueba para el disco y sale")
    ap.add_argument("--persist-check", action="store_true",
                    help="comprueba lo grabado por una corrida anterior, sin grabar")
    ap.add_argument("--persist", action="store_true",
                    help="graba un programa en el disco y comprueba que quedo")
    ap.add_argument("--nvme", action="store_true",
                    help="le habla al controlador NVMe con los once verbos")
    ap.add_argument("--lspci", action="store_true",
                    help="lista los aparatos del bus PCIe, que es el primer paso de un driver")
    ap.add_argument("--console", action="store_true",
                    help="una terminal para hablarle al kernel a mano")
    ap.add_argument("--connect", metavar="SOCKET", nargs="?", const=DEFAULT_SOCKET,
                    help="hablarle a una maquina que ya esta viva, en vez de arrancar una")
    ap.add_argument("--write-blob", metavar="RUTA",
                    help="escribe un blob.bin de prueba para esta arquitectura y sale")
    ap.add_argument("--msi", action="store_true",
                    help="un aparato dispara su interrupcion escribiendo en memoria")
    ap.add_argument("--deadline", action="store_true",
                    help="el agente declara cuanto puede tardar su codigo")
    ap.add_argument("--recover", action="store_true",
                    help="recupera un nucleo cuyo codigo no vuelve")
    ap.add_argument("--clock", action="store_true",
                    help="comprueba que el reloj de la maquina mida tiempo (deuda 17)")
    ap.add_argument("--cancel-blob", action="store_true",
                    help="manda un byte al arrancar, para caer en la ventana de rescate (D18)")
    ap.add_argument("--exec", action="store_true", dest="run_exec",
                    help="sube codigo maquina de verdad y lo corre")
    ap.add_argument("--permission", action="store_true",
                    help="pide memoria alcanzable sin privilegio y comprueba que el bit este")
    ap.add_argument("--dma", action="store_true",
                    help="el IOMMU hace cumplir lo que el agente declaro (D8)")
    ap.add_argument("--on-core", action="store_true", dest="on_core",
                    help="manda el codigo a correr a un nucleo reclamado")
    ap.add_argument("--supervised", action="store_true",
                    help="corre codigo sin privilegio y comprueba que no pueda colgar la maquina")
    ap.add_argument("--during", action="store_true",
                    help="el handler del agente corre durante un exec largo")
    ap.add_argument("--handler", action="store_true",
                    help="instala un handler de interrupcion del agente y lo hace sonar")
    ap.add_argument("--doorbell", action="store_true",
                    help="el agente toca el timbre del kernel con codigo propio")
    ap.add_argument("--mailbox", action="store_true",
                    help="arma el segundo canal y le habla por ahi")
    ap.add_argument("--cores", action="store_true",
                    help="reclama los otros nucleos y los arranca")
    ap.add_argument("--memory", action="store_true",
                    help="prueba el lazo completo: claim, write, read, release")
    args = ap.parse_args()

    # El blob de prueba no solo corre: **le pide memoria al kernel** por la
    # ventanilla que recibe al arrancar. Asi las dos mitades se pueden ver desde
    # afuera sin que el kernel mire lo que hace el blob (D20): el largo de la
    # respuesta lo deja en su registro, y el reclamo queda en la maquina.
    if args.write_payload:
        # El payload es codigo maquina de verdad: el mismo programa que prueba
        # `exec`, que deja 0xc0ffee y vuelve. Asi, cuando el cargador salta, se
        # puede comprobar desde afuera que corrio lo que estaba en el disco.
        body = PROGRAMS[args.arch]["ok"]
        with open(args.write_payload, "wb") as f:
            f.write(payload_image(body=body))
        print(f"payload de prueba para {args.arch}: {args.write_payload}"
              f" ({len(body)} bytes de codigo)")
        return 0

    if args.write_blob:
        with open(args.write_blob, "wb") as f:
            f.write(blob_image(args.arch))
        print(f"blob de prueba para {args.arch}: {args.write_blob}")
        return 0

    script = os.path.join(ROOT, "scripts", f"run-{args.arch}.sh")
    # bufsize=0 no es un detalle: con buffer, Python se trae un bloque entero a
    # su buffer interno y despues `select` sobre el descriptor dice "no hay
    # nada" mientras los bytes ya estan leidos. El cliente se cuelga esperando
    # datos que ya tiene.
    cmd = [script]
    if args.smp > 1:
        # Los scripts le pasan a QEMU cualquier argumento extra.
        cmd += ["-smp", str(args.smp)]
    # Con esto el codigo del kernel lo ejecuta el procesador de verdad en vez de
    # la emulacion. Importa por la misma razon que importan las dos
    # arquitecturas (D22): emulando, un modelo de memoria mas fuerte que el real
    # esconde barreras que faltan, y ademas el reloj y los bits de direccion
    # fisica dejan de ser los que invento QEMU y pasan a ser los del silicio.
    if args.kvm:
        cmd += ["-accel", "kvm"]
    # Sin ACPI el firmware le pasa al kernel un device tree en su lugar: es el
    # otro dialecto en el que una maquina se describe, y el kernel tiene que
    # poder averiguar lo mismo por los dos (P4).
    env = dict(os.environ, NO_ACPI="1") if args.no_acpi else None
    if args.connect:
        # La maquina ya arranco y ya paso el marcador, asi que no hay banner que
        # leer: se empieza hablando. Si del otro lado no hay nadie, el primer
        # pedido lo dice.
        try:
            proc = Attached(args.connect)
        except OSError as e:
            print(f"no hay una maquina en {args.connect}: {e}")
            print(f"  levantala con:  SOCKET={args.connect} ./scripts/run-{args.arch}.sh")
            return 1
        print(f"enganchado a la maquina en {args.connect}")
    else:
        proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=None if args.raw else subprocess.PIPE,
                                cwd=ROOT, bufsize=0, env=env)
    try:
        if not args.connect:
            print(f"arrancando {args.arch} en QEMU...")
            try:
                read_until_marker(proc, args.timeout, show=True,
                                  cancel_blob=args.cancel_blob)
            except (EOFError, TimeoutError) as e:
                # Sin esto, un script que muere por `set -e` —un `dd` que no
                # encuentra su archivo, por ejemplo— se ve como "QEMU se cerro"
                # y no dice por que. Costo un rato largo averiguarlo una vez.
                print(f"\nno arranco: {e}")
                if proc.stderr is not None:
                    said = proc.stderr.read().decode("utf-8", "replace").strip()
                    if said:
                        print("lo que dijo el script:")
                        for line in said.splitlines()[-8:]:
                            print(f"  {line}")
                return 1

        argumentos = {}
        if args.what:
            argumentos["what"] = [s.strip() for s in args.what.split(",")]
        request = [1, "describe", argumentos]

        if args.raw:
            print(f"\n  -> {enc(request).hex()}")

        reply, raw = ask(proc, request, args.timeout)
        if args.raw:
            print(f"  <- {raw.hex()}")

        ident, ok, load = reply
        print(f"\nrespuesta id={ident} ok={ok}")
        if not ok:
            print(f"  ERROR: {load}")
            return 1
        show(load)

        # El lazo de memoria va en el mismo arranque: cada booteo de QEMU son
        # quince segundos, y el porton hace esto por arquitectura.
        rc = 0
        if args.msi:
            print("\n== un aparato dispara su interrupcion por escritura ==")
            rc |= test_msi(proc, args.timeout, args.arch)
        if args.deadline:
            print("\n== el agente declara cuanto tarda su codigo ==")
            rc |= test_deadline(proc, args.timeout, args.arch)
        if args.recover:
            print("\n== recuperar un nucleo cuyo codigo no vuelve ==")
            rc |= test_recover(proc, args.timeout, args.arch)
        if args.clock:
            print("\n== el reloj de la maquina mide tiempo (deuda 17) ==")
            rc |= test_clock(proc, args.timeout)
        if args.memory:
            print()
            rc |= test_memory(proc, args.timeout)
        if args.run_exec:
            print()
            rc |= test_exec(proc, args.timeout, args.arch)
        if args.cores:
            print()
            rc |= test_cores(proc, args.timeout)
        if args.mailbox:
            print()
            rc |= test_mailbox(proc, args.timeout)
        if args.doorbell:
            print()
            rc |= test_doorbell(proc, args.timeout, args.arch)
        if args.handler:
            print()
            rc |= test_handler(proc, args.timeout, args.arch)
        if args.during:
            print()
            rc |= test_handler_during_exec(proc, args.timeout, args.arch)
        if args.dma:
            print("\n== el IOMMU hace cumplir lo declarado (D8) ==")
            rc |= test_dma(proc, args.timeout, args.arch)
        if args.on_core:
            print("\n== el agente elige en que nucleo corre (D13) ==")
            rc |= test_on_core(proc, args.timeout, args.arch)
        if args.supervised:
            print("\n== el agente declara con que privilegio corre (D27) ==")
            rc |= test_supervised(proc, args.timeout, args.arch)
        if args.permission:
            print()
            rc |= test_permission(proc, args.timeout, args.arch)
        if args.lspci:
            print("\n== lo que hay en el bus PCIe ==")
            rc |= test_lspci(proc, args.timeout)
        if args.nvme:
            print("\n== un driver de NVMe, escrito con los once verbos ==")
            rc |= test_nvme(proc, args.timeout, args.arch)
        if args.persist or args.persist_check:
            titulo = ("comprobar que lo grabado sobrevivio al reinicio"
                      if args.persist_check else "grabar un programa en el disco")
            print(f"\n== {titulo} (D18/D19) ==")
            rc |= test_persist(proc, args.timeout, args.arch,
                               only_check=args.persist_check)
        # La consola va ultima: se queda con la maquina hasta que la suelten, asi
        # que cualquier prueba pedida en la misma corrida ya paso por aca.
        if args.console:
            rc |= console(proc, args.timeout)
        return rc
    finally:
        proc.kill()
        proc.wait()


if __name__ == "__main__":
    sys.exit(main())
