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

    Corre **sin privilegio** (D27), asi que no puede llamar a una funcion del
    kernel: pide por la puerta que `describe exec` publica como `service`, y
    vuelve por la que publica como `return`. Los dos son bytes, no nombres de
    instrucciones: el agente los pega y no tiene que saber sobre que silicio
    corre (D3).

    Recibe en el primer registro de argumento su propia direccion, que es lo
    unico que necesita — la puerta es una instruccion, no una direccion.
    """
    if arch == "x86_64":
        # rcx = base. **No rdi**: el kernel se compila para UEFI, donde la ABI
        # de C es la de Windows. Los cuatro argumentos del pedido van por rcx,
        # rdx, r8 y r9, que es lo que publica `describe exec arguments`.
        return (
            bytes([0x48, 0x89, 0xC8])                      # mov rax, rcx  (base)
            + bytes([0x48, 0x8D, 0x88]) + BLOB_REQUEST_AT.to_bytes(4, "little")   # lea rcx,[rax+..]
            + bytes([0xBA]) + request_len.to_bytes(4, "little")                   # mov edx, len
            + bytes([0x4C, 0x8D, 0x80]) + BLOB_REPLY_AT.to_bytes(4, "little")     # lea r8,[rax+..]
            + bytes([0x41, 0xB9]) + BLOB_REPLY_CAP.to_bytes(4, "little")          # mov r9d, cap
            + bytes([0xCD, 0x81])                          # int 0x81: atendeme
            + bytes([0xCD, 0x80])                          # int 0x80: termine
        )

    # x0 = base. Los cuatro argumentos van por x0-x3.
    def word(w):
        return w.to_bytes(4, "little")

    return (
        word(0xAA0003E8)                                   # mov x8, x0
        + word(0x91000100 | (BLOB_REQUEST_AT << 10))       # add x0, x8, #req
        + word(0xD2800001 | (request_len << 5))            # mov x1, #len
        + word(0x91000102 | (BLOB_REPLY_AT << 10))         # add x2, x8, #reply
        + word(0xD2800003 | (BLOB_REPLY_CAP << 5))         # mov x3, #cap
        + word(0xD4000021)                                 # svc #1: atendeme
        + word(0xD4000001)                                 # svc #0: termine
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


# --- Un ensamblador chiquito ------------------------------------------------
#
# `emit_writes` alcanzaba mientras el codigo del agente fuera "escribi esto
# aca". El transporte de D5 no entra ahi: tiene un bucle, condiciones y dos
# copias de largo variable, y escribirlo a mano en bytes dos veces —una por
# arquitectura— es la clase de cosa que sale mal en silencio.
#
# Asi que hay un ensamblador, con **solo** las instrucciones que ese programa
# usa y ni una mas. No es un ensamblador de verdad y no quiere serlo: es lo
# minimo para que el bucle se pueda leer como codigo en vez de como un hexa.
#
# Los registros se numeran 0..13 y cada arquitectura los mapea a los suyos. Se
# eligieron los que no obligan a casos especiales: en x86_64 quedan afuera `rsp`
# y `r12`, que como base de un acceso a memoria necesitan un byte extra que
# ninguna otra instruccion necesita.
X86_REGS = [0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 13, 14, 15]


class Asm:
    """Ensambla un programa con etiquetas, para x86_64 o aarch64.

    Dos pasadas: la primera anota donde cayo cada etiqueta y la segunda emite.
    Para que las dos pasadas coincidan, **todas las instrucciones miden lo
    mismo siempre** — los saltos se emiten con el desplazamiento mas grande
    aunque sobre. Un salto que cambia de tamano segun a donde va hace que las
    etiquetas se muevan mientras se las calcula.
    """

    def __init__(self, arch):
        self.arch = arch
        self.items = []       # (tamano, funcion que emite dado el mapa de etiquetas)
        self.labels = {}
        self.here = 0

    # -- lo que se usa para armar el programa --------------------------------

    def label(self, name):
        self.labels[name] = self.here

    def _put(self, size, fn):
        self.items.append((self.here, size, fn))
        self.here += size

    def assemble(self):
        out = b""
        for at, size, fn in self.items:
            chunk = fn(self.labels, at)
            assert len(chunk) == size, f"instruccion de tamano variable en {at}"
            out += chunk
        return out

    # -- las instrucciones ---------------------------------------------------

    def movi(self, r, value):
        """Un valor de 64 bits en un registro."""
        if self.arch == "x86_64":
            n = X86_REGS[r]
            code = bytes([0x48 | (n >> 3), 0xB8 + (n & 7)]) + (value & (2**64 - 1)).to_bytes(8, "little")
            self._put(len(code), lambda l, a, c=code: c)
        else:
            code = _a64_movi(r, value)
            self._put(len(code), lambda l, a, c=code: c)

    def mov(self, dst, src):
        if self.arch == "x86_64":
            d, s = X86_REGS[dst], X86_REGS[src]
            code = bytes([0x48 | ((s >> 3) << 2) | (d >> 3), 0x89,
                          0xC0 | ((s & 7) << 3) | (d & 7)])
        else:
            # `orr xd, xzr, xs` es como se escribe un `mov` de registro a
            # registro: no hay opcode propio.
            code = (0xAA0003E0 | (src << 16) | dst).to_bytes(4, "little")
        self._put(len(code), lambda l, a, c=code: c)

    def load(self, dst, base, off, width):
        """dst = [base + off], leyendo `width` bytes y rellenando con ceros."""
        if self.arch == "x86_64":
            d, b = X86_REGS[dst], X86_REGS[base]
            rex = 0x40 | ((d >> 3) << 2) | (b >> 3)
            modrm = bytes([0x80 | ((d & 7) << 3) | (b & 7)]) + _s32(off)
            if width == 8:
                code = bytes([rex | 8, 0x8B]) + modrm
            elif width == 4:
                code = bytes([rex, 0x8B]) + modrm
            elif width == 2:
                code = bytes([rex, 0x0F, 0xB7]) + modrm
            else:
                code = bytes([rex, 0x0F, 0xB6]) + modrm
        else:
            # Las formas "sin escalar" (LDUR) toman el offset tal cual, en vez
            # de dividirlo por el ancho. Con una sola forma alcanza para todos
            # los anchos y no hay que preocuparse por si el offset es multiplo.
            op = {1: 0x38400000, 2: 0x78400000, 4: 0xB8400000, 8: 0xF8400000}[width]
            code = (op | ((off & 0x1FF) << 12) | (base << 5) | dst).to_bytes(4, "little")
        self._put(len(code), lambda l, a, c=code: c)

    def store(self, base, off, src, width):
        """[base + off] = src, escribiendo `width` bytes."""
        if self.arch == "x86_64":
            s, b = X86_REGS[src], X86_REGS[base]
            rex = 0x40 | ((s >> 3) << 2) | (b >> 3)
            modrm = bytes([0x80 | ((s & 7) << 3) | (b & 7)]) + _s32(off)
            if width == 8:
                code = bytes([rex | 8, 0x89]) + modrm
            elif width == 4:
                code = bytes([rex, 0x89]) + modrm
            elif width == 2:
                code = bytes([0x66, rex, 0x89]) + modrm
            else:
                # El prefijo va **siempre** aunque no haga falta por el numero
                # de registro: sin el, guardar un byte desde rsi/rdi/rbp no
                # escribe ese registro sino la mitad de arriba de otro. Es un
                # rincon de x86 que viene de 1978 y no perdona.
                code = bytes([rex, 0x88]) + modrm
        else:
            op = {1: 0x38000000, 2: 0x78000000, 4: 0xB8000000, 8: 0xF8000000}[width]
            code = (op | ((off & 0x1FF) << 12) | (base << 5) | src).to_bytes(4, "little")
        self._put(len(code), lambda l, a, c=code: c)

    def addi(self, r, imm):
        if self.arch == "x86_64":
            n = X86_REGS[r]
            code = bytes([0x48 | (n >> 3), 0x81, 0xC0 | (n & 7)]) + _s32(imm)
        else:
            code = (0x91000000 | (imm << 10) | (r << 5) | r).to_bytes(4, "little")
        self._put(len(code), lambda l, a, c=code: c)

    def andi(self, r, bits):
        """r &= (2**bits - 1). Solo mascaras de unos seguidos desde abajo.

        No es una limitacion que moleste: todas las mascaras de este programa
        son "dar la vuelta a un anillo", que es exactamente esa forma. Y en
        aarch64 las mascaras arbitrarias se codifican de una manera que no
        vale la pena escribir para no usarla.
        """
        if self.arch == "x86_64":
            n = X86_REGS[r]
            code = bytes([0x48 | (n >> 3), 0x81, 0xE0 | (n & 7)]) + _s32((1 << bits) - 1)
        else:
            code = (0x92000000 | (1 << 22) | ((bits - 1) << 10)
                    | (r << 5) | r).to_bytes(4, "little")
        self._put(len(code), lambda l, a, c=code: c)

    def add(self, dst, src):
        if self.arch == "x86_64":
            d, s = X86_REGS[dst], X86_REGS[src]
            code = bytes([0x48 | ((s >> 3) << 2) | (d >> 3), 0x01,
                          0xC0 | ((s & 7) << 3) | (d & 7)])
        else:
            code = (0x8B000000 | (src << 16) | (dst << 5) | dst).to_bytes(4, "little")
        self._put(len(code), lambda l, a, c=code: c)

    def sub(self, dst, src):
        if self.arch == "x86_64":
            d, s = X86_REGS[dst], X86_REGS[src]
            code = bytes([0x48 | ((s >> 3) << 2) | (d >> 3), 0x29,
                          0xC0 | ((s & 7) << 3) | (d & 7)])
        else:
            code = (0xCB000000 | (src << 16) | (dst << 5) | dst).to_bytes(4, "little")
        self._put(len(code), lambda l, a, c=code: c)

    def trunc32(self, r):
        """Se queda con los 32 bits de abajo del registro.

        Hace falta despues de restar dos indices del buzon: son contadores de 32
        bits que **dan la vuelta** —el kernel los resta con `wrapping_sub`—, y
        restarlos como si fueran de 64 daria, justo despues de la vuelta, un
        numero enorme en vez de una diferencia chica. Pasaria una vez cada 4 GiB
        de trafico, que es la clase de bug que aparece en produccion y nunca en
        una prueba.
        """
        if self.arch == "x86_64":
            n = X86_REGS[r]
            # Escribir la mitad de abajo de un registro pone en cero la de
            # arriba: es la unica regla de x86_64 que hace algo asi, y aca viene
            # bien. Enmascarar con una constante no serviria — el inmediato de
            # 32 bits se extiende con signo y 0xFFFFFFFF se volveria todo unos.
            code = bytes([0x40 | ((n >> 3) << 2) | (n >> 3), 0x89,
                          0xC0 | ((n & 7) << 3) | (n & 7)])
        else:
            code = (0x2A0003E0 | (r << 16) | r).to_bytes(4, "little")
        self._put(len(code), lambda l, a, c=code: c)

    def cmpi(self, r, imm):
        if self.arch == "x86_64":
            n = X86_REGS[r]
            code = bytes([0x48 | (n >> 3), 0x81, 0xF8 | (n & 7)]) + _s32(imm)
        else:
            code = (0xF100001F | (imm << 10) | (r << 5)).to_bytes(4, "little")
        self._put(len(code), lambda l, a, c=code: c)

    def cmp(self, a, b):
        if self.arch == "x86_64":
            d, s = X86_REGS[a], X86_REGS[b]
            code = bytes([0x48 | ((s >> 3) << 2) | (d >> 3), 0x39,
                          0xC0 | ((s & 7) << 3) | (d & 7)])
        else:
            code = (0xEB00001F | (b << 16) | (a << 5)).to_bytes(4, "little")
        self._put(len(code), lambda l, a_=None, c=code: c)

    def _branch(self, kind, target):
        if self.arch == "x86_64":
            head = {"eq": b"\x0f\x84", "ne": b"\x0f\x85",
                    "hs": b"\x0f\x83", "": b"\xe9"}[kind]
            size = len(head) + 4

            def emit(labels, at, head=head, size=size, target=target):
                return head + _s32(labels[target] - (at + size))
        else:
            size = 4

            def emit(labels, at, kind=kind, target=target):
                delta = (labels[target] - at) // 4
                if kind == "":
                    return (0x14000000 | (delta & 0x3FFFFFF)).to_bytes(4, "little")
                cond = {"eq": 0, "ne": 1, "hs": 2}[kind]
                return (0x54000000 | ((delta & 0x7FFFF) << 5) | cond).to_bytes(4, "little")
        self._put(size, emit)

    def beq(self, target):
        self._branch("eq", target)

    def bne(self, target):
        self._branch("ne", target)

    def bhs(self, target):
        """Salta si el ultimo `cmp` dio mayor o igual, sin signo."""
        self._branch("hs", target)

    def b(self, target):
        self._branch("", target)

    def barrier(self):
        """Que lo escrito antes se vea antes que lo que se escriba despues.

        No es adorno. El nucleo que corre esto y el que atiende el protocolo son
        **distintos**, y el buzon dice explicitamente que los indices se leen con
        orden de memoria: si el kernel viera el indice nuevo y los bytes viejos,
        leeria basura. x86 lo da casi siempre y ARM no, que es justo la razon
        por la que este proyecto compila las dos (D22).
        """
        code = b"\x0f\xae\xf0" if self.arch == "x86_64" else (0xD5033BBF).to_bytes(4, "little")
        self._put(len(code), lambda l, a, c=code: c)

    def ret(self):
        code = b"\xc3" if self.arch == "x86_64" else (0xD65F03C0).to_bytes(4, "little")
        self._put(len(code), lambda l, a, c=code: c)

    def raw(self, code):
        """Bytes ya armados, para pegar lo que genera `emit_writes`.

        El timbre del kernel se publica como una lista de escrituras y ya habia
        quien las convirtiera en codigo: se reusa en vez de repetirlo. Usa los
        dos primeros registros, que en este programa son de descarte.
        """
        self._put(len(code), lambda l, a, c=code: c)


def _s32(value):
    """Un entero de 32 bits con signo, como lo quiere x86."""
    return (value & 0xFFFFFFFF).to_bytes(4, "little")


def _a64_movi(reg, value):
    """movz/movk hasta armar un valor de 64 bits, siempre con las cuatro partes.

    Se emiten las cuatro aunque alguna sea cero: el ensamblador necesita que
    cada instruccion mida lo mismo en las dos pasadas, y saltear las partes en
    cero haria que el tamano dependa del valor.
    """
    out = (0xD2800000 | ((value & 0xFFFF) << 5) | reg).to_bytes(4, "little")
    for hw in range(1, 4):
        chunk = (value >> (16 * hw)) & 0xFFFF
        out += (0xF2800000 | (hw << 21) | (chunk << 5) | reg).to_bytes(4, "little")
    return out


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


# --- La placa de red --------------------------------------------------------
#
# El segundo driver del agente, y el que D5 estaba esperando: mientras el unico
# transporte sea el cordon umbilical, subir algo grande es imposible fuera de
# QEMU. A 115200 baudios el cable da unos 11 KB/s, que es exactamente el
# problema que D20 quiere evitar.
#
# Hay una diferencia con el NVMe que conviene ver **antes** de leer el codigo.
# Al NVMe se lo encuentra por lo que **hace**: la clase 01.08.02 quiere decir
# "cualquier NVMe", y un solo driver maneja el de QEMU y el de una maquina de
# verdad. Con una placa de red eso no existe. La clase 02.00.00 solo quiere
# decir "ethernet", y abajo de ella cada modelo tiene registros que no se
# parecen en nada a los del vecino. Asi que aca **hay que elegir modelo**, y no
# es una comodidad de la prueba: es lo que D20 ya anticipaba al decir que el
# blob trae "drivers para una lista conocida". La lista existe porque no hay
# forma de no tenerla.
#
# El modelo es la Intel 82540EM (`8086:100e`), que es la placa real mas simple
# que hay: registros por MMIO, dos anillos de descriptores, y nada mas. Los
# numeros salen del manual del fabricante, que es publico. Va la misma en las
# dos maquinas de prueba justamente para que este archivo tenga **un** driver y
# no dos (D22).
E1000_ID = 0x100E8086         # dispositivo y fabricante, como vienen juntos
NET_CLASS = (0x02, 0x00, 0x00)

# Sus registros, dentro de la ventana de memoria que publica el BAR 0.
E1000_CTRL = 0x0000       # control general: aca se lo resetea
E1000_STATUS = 0x0008     # estado: aca dice si el cable esta enchufado
E1000_IMC = 0x00D8        # apagar interrupciones (se escribe 1 para apagar)
E1000_RCTL = 0x0100       # control de recepcion
E1000_TCTL = 0x0400       # control de transmision
E1000_TIPG = 0x0410       # el hueco entre paquetes
E1000_RDBAL = 0x2800      # donde esta el anillo de recepcion (abajo)
E1000_RDBAH = 0x2804      # y arriba
E1000_RDLEN = 0x2808      # cuanto mide, en bytes
E1000_RDH = 0x2810        # por donde va el aparato
E1000_RDT = 0x2818        # hasta donde le dejamos escribir
E1000_TDBAL = 0x3800      # lo mismo para transmision
E1000_TDBAH = 0x3804
E1000_TDLEN = 0x3808
E1000_TDH = 0x3810
E1000_TDT = 0x3818
E1000_MTA = 0x5200        # filtro de multicast, 128 palabras
E1000_RAL = 0x5400        # su direccion MAC, abajo
E1000_RAH = 0x5404        # y arriba, con el bit de "esta entrada vale"

E1000_CTRL_SLU = 1 << 6       # subir el enlace
E1000_CTRL_RST = 1 << 26      # resetear
E1000_STATUS_LU = 1 << 1      # el enlace esta arriba
E1000_RAH_AV = 1 << 31        # esta entrada de direccion vale

E1000_RCTL_EN = 1 << 1        # recibir
E1000_RCTL_BAM = 1 << 15      # aceptar broadcast (sin esto no llega un ARP)
E1000_RCTL_SECRC = 1 << 26    # que la placa saque el CRC del final

E1000_TCTL_EN = 1 << 1        # transmitir
E1000_TCTL_PSP = 1 << 3       # rellenar los paquetes cortos hasta 60 bytes

# En el descriptor de transmision: fin de paquete, poneme el CRC, y avisame
# cuando lo hayas mandado.
E1000_TXD_EOP = 1 << 0
E1000_TXD_IFCS = 1 << 1
E1000_TXD_RS = 1 << 3
E1000_TXD_DD = 1 << 0         # en el estado: "listo"
E1000_RXD_DD = 1 << 0
E1000_RXD_EOP = 1 << 1

# Cuantos descriptores tiene cada anillo. Ocho es el minimo util y no es un
# numero redondo por gusto: el aparato exige que el anillo mida un multiplo de
# 128 bytes, y un descriptor son 16.
RING_SLOTS = 8
DESC_BYTES = 16
# Lo mas grande que puede entrar en un buffer de recepcion. 2048 es el valor por
# omision de la placa, y alcanza para un paquete de ethernet entero.
RX_BUFFER = 2048


class E1000:
    """Le habla a una placa Intel 82540EM usando solo los verbos del kernel.

    Igual que el driver de NVMe: no hay un verbo `red`, hay memoria, permiso de
    DMA y registros. El kernel no sabe que esta clase existe (D4).
    """

    def __init__(self, ask_verb, ident):
        self.ask = ask_verb
        self.n = ident
        self.window = None      # el reclamo de su ventana de registros
        self.claims = []        # todo lo reclamado, para poder devolverlo
        self.bdf = None
        self.mac = None
        self.tx_next = 0        # el proximo descriptor de transmision a usar
        self.rx_next = 0        # el proximo a mirar por si llego algo

    def _id(self):
        self.n += 1
        return self.n

    def reg_read(self, off):
        # Todos sus registros son de 32 bits y **solo** aceptan accesos de 32
        # bits. Leerlos de a un byte devuelve ceros en x86_64 y mata el bus en
        # aarch64: la misma moneda que ya se pago con el NVMe.
        ok, r = self.ask(self._id(), "mem.read",
                         {"handle": self.window["handle"], "off": off,
                          "len": 4, "width": 4})
        if not ok:
            return None
        return int.from_bytes(r["bytes"], "little")

    def reg_write(self, off, value):
        ok, _ = self.ask(self._id(), "mem.write",
                         {"handle": self.window["handle"], "off": off,
                          "bytes": int(value & 0xFFFFFFFF).to_bytes(4, "little"),
                          "width": 4})
        return ok

    def shared(self, bytes_wanted):
        """Memoria que la placa tambien toca: pedida, limpia y declarada.

        Los tres pasos van juntos siempre, por lo mismo que en el NVMe: sin
        limpiar se leen descriptores viejos que parecen listos, y sin declarar
        el IOMMU la bloquea — que se ve igual que una placa que no contesta.
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

    def find(self, ecam):
        """Busca la placa en el bus y reclama su ventana de registros.

        Se busca por clase **y** por modelo, y las dos cosas hacen falta: la
        clase descarta todo lo que no sea una placa de red, y el modelo es lo
        unico que autoriza a escribirle estos registros y no otros.
        """
        cfg, devices = pcie_scan(self.ask, self._id(), ecam)
        if cfg is None:
            return f"no se pudo mirar el bus: {devices}"
        mine = [d for d in devices
                if d["class"] == NET_CLASS and d["id"] == E1000_ID]
        if not mine:
            otras = [d for d in devices if d["class"] == NET_CLASS]
            self.ask(self._id(), "release", {"handle": cfg["handle"]})
            if otras:
                # Decir cual es la placa que hay es mucho mas util que "no hay
                # red": el que lee esto sabe que driver le falta (P4).
                tiene = ", ".join(f"{d['id'] & 0xFFFF:04x}:{d['id'] >> 16:04x}"
                                  for d in otras)
                return (f"hay placa de red pero no es la que este driver sabe"
                        f" manejar: {tiene}, y este driver es para 8086:100e")
            return "no hay ninguna placa de red en este bus"
        dev = mine[0]

        # Que responda a accesos de memoria y que pueda ser maestro del bus: sin
        # lo segundo no puede leer sus propios anillos, que viven en RAM.
        self.ask(self._id(), "mem.write",
                 {"handle": cfg["handle"], "off": dev["off"] + 4,
                  "bytes": bytes([0x06, 0x00])})
        self.ask(self._id(), "release", {"handle": cfg["handle"]})

        self.bdf = dev["bdf"]
        # Su ventana mide 128 KiB. Se reclama entera aunque los registros que
        # usamos entren en 32: el rango es de la placa, no del driver.
        ok, win = self.ask(self._id(), "mem.claim",
                           {"at": dev["window"], "bytes": 0x20000})
        if not ok:
            return f"no se pudo alcanzar su ventana: {win}"
        self.window = win
        return None

    def reset(self):
        """La apaga, la resetea y le sube el enlace.

        Resetear primero no es prolijidad: el firmware pudo haberla dejado a
        medio configurar, y los anillos que le demos despues los lee **una sola
        vez**, cuando se la enciende.
        """
        # Callarle las interrupciones antes que nada: todavia no hay handler, y
        # una placa que interrumpe sin que nadie la atienda no es un problema
        # aca, pero una que interrumpe **durante** el reset si.
        self.reg_write(E1000_IMC, 0xFFFFFFFF)
        self.reg_write(E1000_CTRL, self.reg_read(E1000_CTRL) | E1000_CTRL_RST)

        # El reset se anuncia solo: el bit se limpia cuando termino. Esperarlo
        # es mejor que dormir un numero inventado.
        deadline = time.time() + 2.0
        while time.time() < deadline:
            ctrl = self.reg_read(E1000_CTRL)
            if ctrl is None:
                return "la placa dejo de contestar durante el reset"
            if not (ctrl & E1000_CTRL_RST):
                break
        else:
            return "no termino de resetearse en 2 s"

        # Y otra vez despues del reset: el reset las vuelve a habilitar.
        self.reg_write(E1000_IMC, 0xFFFFFFFF)
        self.reg_write(E1000_CTRL, self.reg_read(E1000_CTRL) | E1000_CTRL_SLU)

        # La MAC sale de la propia placa. No se la inventa ni se la hornea: es
        # un dato de la maquina, y preguntarselo es P4 aplicado a un aparato.
        low = self.reg_read(E1000_RAL)
        high = self.reg_read(E1000_RAH)
        if not (high & E1000_RAH_AV):
            # Si esa entrada no vale, lo que leimos no es una direccion. Vale la
            # pena distinguirlo: seis bytes de basura se ven como una MAC.
            return "la placa no tiene direccion valida en su primera ranura"
        self.mac = bytes([low & 0xFF, (low >> 8) & 0xFF, (low >> 16) & 0xFF,
                          (low >> 24) & 0xFF, high & 0xFF, (high >> 8) & 0xFF])

        # El filtro de multicast, vacio. Viene con basura despues del reset en
        # algunas placas, y una entrada suelta hace entrar trafico ajeno.
        for i in range(128):
            self.reg_write(E1000_MTA + i * 4, 0)
        return None

    def rings(self):
        """Le arma los dos anillos y la enciende.

        Un anillo es un arreglo de descriptores en RAM: cada uno dice donde esta
        un buffer y cuanto se uso. La placa y el driver se persiguen por el
        anillo con dos indices —una cabeza y una cola— y nadie interrumpe a
        nadie: se avisan moviendo la cola, que es una escritura a un registro.
        """
        ring_bytes = RING_SLOTS * DESC_BYTES

        # Los dos anillos, y los buffers de cada lado.
        self.tx_ring, why = self.shared(4096)
        if why:
            return f"sin memoria para el anillo de transmision: {why}"
        self.rx_ring, why = self.shared(4096)
        if why:
            return f"sin memoria para el anillo de recepcion: {why}"
        self.tx_buf, why = self.shared(4096)
        if why:
            return f"sin memoria para el buffer de transmision: {why}"
        self.rx_buf, why = self.shared(RING_SLOTS * RX_BUFFER)
        if why:
            return f"sin memoria para los buffers de recepcion: {why}"

        # Los descriptores de recepcion: cada uno apunta a su buffer y va con el
        # estado en cero, que es como se dice "este es tuyo".
        desc = bytearray(ring_bytes)
        for i in range(RING_SLOTS):
            addr = self.rx_buf["start"] + i * RX_BUFFER
            desc[i * DESC_BYTES:i * DESC_BYTES + 8] = addr.to_bytes(8, "little")
        ok, _ = self.ask(self._id(), "mem.write",
                         {"handle": self.rx_ring["handle"], "off": 0,
                          "bytes": bytes(desc)})
        if not ok:
            return "no se pudieron escribir los descriptores de recepcion"

        self.reg_write(E1000_RDBAL, self.rx_ring["start"] & 0xFFFFFFFF)
        self.reg_write(E1000_RDBAH, self.rx_ring["start"] >> 32)
        self.reg_write(E1000_RDLEN, ring_bytes)
        self.reg_write(E1000_RDH, 0)
        # La cola va **una atras** de la cabeza, y por eso se sacrifica una
        # ranura: cabeza igual a cola quiere decir "no hay lugar", asi que
        # apuntarla al mismo lado dejaria a la placa sin donde escribir.
        self.rx_next = 0
        self.reg_write(E1000_RDT, RING_SLOTS - 1)

        self.reg_write(E1000_TDBAL, self.tx_ring["start"] & 0xFFFFFFFF)
        self.reg_write(E1000_TDBAH, self.tx_ring["start"] >> 32)
        self.reg_write(E1000_TDLEN, ring_bytes)
        self.reg_write(E1000_TDH, 0)
        self.reg_write(E1000_TDT, 0)
        self.tx_next = 0

        # El hueco entre paquetes que manda el manual para ethernet de cobre.
        self.reg_write(E1000_TIPG, 10 | (8 << 10) | (6 << 20))
        self.reg_write(E1000_TCTL, E1000_TCTL_EN | E1000_TCTL_PSP
                       | (0x0F << 4) | (0x40 << 12))
        # BAM es lo que deja entrar el broadcast. Sin ese bit un ARP no llega
        # nunca, y la falla se ve como "la red no anda" en vez de como un filtro.
        self.reg_write(E1000_RCTL, E1000_RCTL_EN | E1000_RCTL_BAM
                       | E1000_RCTL_SECRC)
        return None

    def rearm(self):
        """Deja los dos anillos como recien armados, en la ranura cero.

        Hace falta antes de entregarle la placa al bucle del transporte: ese
        codigo empieza a contar desde la ranura cero, y para entonces Python ya
        mando y recibio unos cuantos paquetes. Si los indices no coincidieran,
        el bucle miraria una ranura que la placa no esta usando — y eso se ve
        como "la red no anda", que no se parece en nada a su causa.

        Se apaga la placa para tocar los indices: mover la cabeza de un anillo
        con el aparato recibiendo es pedirle que escriba en cualquier lado.
        """
        self.reg_write(E1000_RCTL, 0)
        self.reg_write(E1000_TCTL, 0)

        desc = bytearray(RING_SLOTS * DESC_BYTES)
        for i in range(RING_SLOTS):
            addr = self.rx_buf["start"] + i * RX_BUFFER
            desc[i * DESC_BYTES:i * DESC_BYTES + 8] = addr.to_bytes(8, "little")
        self.ask(self._id(), "mem.write",
                 {"handle": self.rx_ring["handle"], "off": 0, "bytes": bytes(desc)})
        self.ask(self._id(), "mem.write",
                 {"handle": self.tx_ring["handle"], "off": 0,
                  "bytes": bytes(RING_SLOTS * DESC_BYTES)})

        self.reg_write(E1000_RDH, 0)
        self.reg_write(E1000_RDT, RING_SLOTS - 1)
        self.reg_write(E1000_TDH, 0)
        self.reg_write(E1000_TDT, 0)
        self.rx_next = 0
        self.tx_next = 0

        self.reg_write(E1000_TCTL, E1000_TCTL_EN | E1000_TCTL_PSP
                       | (0x0F << 4) | (0x40 << 12))
        self.reg_write(E1000_RCTL, E1000_RCTL_EN | E1000_RCTL_BAM | E1000_RCTL_SECRC)

    def link_up(self, timeout_s=3.0):
        """Espera a que la placa diga que el cable esta enchufado."""
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            status = self.reg_read(E1000_STATUS)
            if status is None:
                return "la placa dejo de contestar"
            if status & E1000_STATUS_LU:
                return None
        return "el enlace no subio"

    def send(self, frame):
        """Manda un paquete y espera a que la placa confirme que salio."""
        if len(frame) > 4096:
            return f"{len(frame)} bytes no entran en el buffer de transmision"
        ok, _ = self.ask(self._id(), "mem.write",
                         {"handle": self.tx_buf["handle"], "off": 0, "bytes": frame})
        if not ok:
            return "no se pudo dejar el paquete en memoria"

        slot = self.tx_next
        desc = bytearray(DESC_BYTES)
        desc[0:8] = self.tx_buf["start"].to_bytes(8, "little")
        desc[8:10] = len(frame).to_bytes(2, "little")
        desc[11] = E1000_TXD_EOP | E1000_TXD_IFCS | E1000_TXD_RS
        ok, _ = self.ask(self._id(), "mem.write",
                         {"handle": self.tx_ring["handle"],
                          "off": slot * DESC_BYTES, "bytes": bytes(desc)})
        if not ok:
            return "no se pudo dejar el descriptor en el anillo"

        # Mover la cola es lo que le avisa. Hasta esta escritura la placa no
        # miro nada de lo anterior.
        self.tx_next = (slot + 1) % RING_SLOTS
        self.reg_write(E1000_TDT, self.tx_next)

        # Y esperar a que lo confirme. Sin esto "se mando" querria decir "se
        # dejo escrito", que es otra cosa.
        deadline = time.time() + 3.0
        while time.time() < deadline:
            ok, r = self.ask(self._id(), "mem.read",
                             {"handle": self.tx_ring["handle"],
                              "off": slot * DESC_BYTES + 12, "len": 1})
            if ok and (r["bytes"][0] & E1000_TXD_DD):
                return None
        return "la placa no confirmo haber mandado el paquete"

    def poll(self, timeout_s=3.0):
        """Devuelve el proximo paquete que haya llegado, o None si no llego.

        Recibir es al reves de mandar y por el otro anillo: la placa escribe el
        paquete en el buffer por DMA y marca el descriptor. Nadie avisa nada —
        aca se mira, que para una prueba alcanza; el transporte de verdad usa el
        timbre de la placa.
        """
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            slot = self.rx_next
            ok, r = self.ask(self._id(), "mem.read",
                             {"handle": self.rx_ring["handle"],
                              "off": slot * DESC_BYTES + 8, "len": 8})
            if not ok:
                return None, "no se pudo mirar el anillo de recepcion"
            length = int.from_bytes(r["bytes"][0:2], "little")
            status = r["bytes"][4]
            if not (status & E1000_RXD_DD):
                continue
            if not (status & E1000_RXD_EOP):
                return None, "llego un paquete partido, y este driver no los junta"

            ok, r = self.ask(self._id(), "mem.read",
                             {"handle": self.rx_buf["handle"],
                              "off": slot * RX_BUFFER, "len": length})
            if not ok:
                return None, "no se pudo leer el paquete que llego"
            frame = r["bytes"]

            # Devolver la ranura: primero se limpia el estado y despues se mueve
            # la cola. Al reves, la placa podria escribir encima de un
            # descriptor que todavia dice "listo" y el proximo poll leeria un
            # largo viejo con un paquete nuevo.
            self.ask(self._id(), "mem.write",
                     {"handle": self.rx_ring["handle"],
                      "off": slot * DESC_BYTES + 8, "bytes": bytes(8)})
            self.rx_next = (slot + 1) % RING_SLOTS
            self.reg_write(E1000_RDT, (self.rx_next - 1) % RING_SLOTS)
            return frame, None
        return None, None

    def release(self):
        """Suelta lo reclamado. Lo que el agente toma, el agente devuelve."""
        # Y antes de soltar, apagarla: si quedara recibiendo, seguiria
        # escribiendo por DMA en memoria que ya no es nuestra. El IOMMU lo
        # frenaria —para eso esta— pero dejar a un aparato apuntando a memoria
        # ajena y confiar en que lo bloqueen es al reves de como se hace.
        if self.window:
            self.reg_write(E1000_RCTL, 0)
            self.reg_write(E1000_TCTL, 0)
        for claim in self.claims + ([self.window] if self.window else []):
            self.ask(self._id(), "release", {"handle": claim["handle"]})
        self.claims = []


# La red que QEMU arma del otro lado del cable. No es una eleccion nuestra: son
# los numeros que usa su red de usuario, y estan aca para poder comprobar contra
# **algo que no somos nosotros**.
QEMU_GUEST_IP = bytes([10, 0, 2, 15])     # la direccion que nos toca
QEMU_GATEWAY_IP = bytes([10, 0, 2, 2])    # el que contesta del otro lado
BROADCAST = b"\xff" * 6
ETHERTYPE_ARP = 0x0806


def arp_request(mac, target_ip, sender_ip=QEMU_GUEST_IP):
    """Arma la pregunta "quien tiene esta IP", que es el paquete mas simple util.

    Se elige ARP y no otra cosa porque la respuesta **no se puede fabricar
    desde aca**: trae una MAC que no conocemos y viene dirigida a la nuestra,
    que la leimos de la placa. Un paquete que nos contestaramos solos probaria
    mucho menos.
    """
    frame = bytearray()
    frame += BROADCAST + mac + ETHERTYPE_ARP.to_bytes(2, "big")
    frame += (1).to_bytes(2, "big")       # sobre ethernet
    frame += (0x0800).to_bytes(2, "big")  # preguntando por una direccion IPv4
    frame += bytes([6, 4])                # cuanto mide cada una
    frame += (1).to_bytes(2, "big")       # es una pregunta
    frame += mac + sender_ip              # quien pregunta
    frame += bytes(6) + target_ip         # y por quien
    # El minimo de ethernet son 60 bytes sin el CRC. La placa rellena sola con
    # TCTL.PSP, pero se rellena aca igual: asi el largo que se manda es el largo
    # que se ve del otro lado, y una prueba no deberia depender de eso.
    frame += bytes(60 - len(frame))
    return bytes(frame)


def parse_arp_reply(frame):
    """Devuelve (mac, ip) del que contesto, o None si esto no es una respuesta."""
    if len(frame) < 42:
        return None
    if int.from_bytes(frame[12:14], "big") != ETHERTYPE_ARP:
        return None
    if int.from_bytes(frame[20:22], "big") != 2:   # 2 = es una respuesta
        return None
    return frame[22:28], frame[28:32]


def net_bring_up(ask_verb, ident, ecam):
    """Encuentra la placa, la resetea y la deja andando. Devuelve (placa, motivo)."""
    nic = E1000(ask_verb, ident)
    if (why := nic.find(ecam)):
        return None, why
    if (why := nic.reset()):
        nic.release()
        return None, why
    if (why := nic.rings()):
        nic.release()
        return None, why
    if (why := nic.link_up()):
        nic.release()
        return None, why
    return nic, None


def test_net(proc, timeout, arch):
    """Le habla a una placa de red de verdad, con los once verbos y nada mas.

    Es lo que le faltaba al agente para dejar de depender del cordon (D5). La
    prueba es un ARP de ida y vuelta contra el otro extremo del cable: no puede
    pasar por casualidad, porque la respuesta trae una MAC que no teniamos y
    viene dirigida a la nuestra, que la leimos de la placa.
    """
    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, d = ask_verb(600, "describe", {"what": ["pcie"]})
    if not ok or not d["pcie"]:
        print("  esta maquina no informa PCIe")
        return 1

    nic, why = net_bring_up(ask_verb, 601, d["pcie"]["base"])
    if nic is None:
        print(f"  FALLA: {why}")
        return 1
    print(f"  placa encontrada: el bus la llama {nic.bdf:#x}")
    print(f"  y dice que su direccion es {':'.join(f'{b:02x}' for b in nic.mac)}")
    print("  reseteada, con sus dos anillos, y el enlace arriba")

    # Y ahora la prueba: preguntar quien tiene la IP del otro extremo.
    if (why := nic.send(arp_request(nic.mac, QEMU_GATEWAY_IP))):
        print(f"  FALLA al mandar: {why}")
        nic.release()
        return 1
    print(f"  mandado un ARP preguntando por {'.'.join(str(b) for b in QEMU_GATEWAY_IP)}")

    while True:
        frame, why = nic.poll()
        if why:
            print(f"  FALLA al recibir: {why}")
            nic.release()
            return 1
        if frame is None:
            print("  FALLA: nadie contesto el ARP")
            nic.release()
            return 1
        answer = parse_arp_reply(frame)
        if answer:
            break
        # Puede entrar cualquier otra cosa antes: la red de QEMU manda lo suyo.
        # Se descarta y se sigue mirando, que es lo que hace un driver de verdad.

    their_mac, their_ip = answer
    print(f"  y contesto {'.'.join(str(b) for b in their_ip)}:"
          f" soy {':'.join(f'{b:02x}' for b in their_mac)}")

    failures = []
    # Las tres cosas que hacen que esto no pueda pasar por casualidad.
    if their_ip != QEMU_GATEWAY_IP:
        failures.append(f"contesto otro: preguntamos por"
                        f" {QEMU_GATEWAY_IP.hex()} y contesto {their_ip.hex()}")
    if frame[0:6] != nic.mac:
        failures.append("la respuesta no venia dirigida a nuestra direccion")
    if their_mac == nic.mac or their_mac == BROADCAST:
        failures.append(f"la MAC que contesto no es de nadie: {their_mac.hex()}")

    nic.release()
    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  red: ok")
    return 0


# --- Lo minimo de IP y UDP --------------------------------------------------
#
# Esto no es un stack: es lo mas chico que permite que por la placa viaje algo
# con destinatario. Y es del agente, no del kernel — D5 dice justamente que el
# kernel nunca necesita stack de red.
#
# Se elige UDP y no TCP a proposito. El protocolo del kernel ya es pedido y
# respuesta con un identificador en cada uno (D6), asi que reintentar es
# volver a mandar el mismo pedido: todo lo que TCP agrega —ventanas, orden,
# reensamblado— seria repetir en el transporte algo que el protocolo ya tiene.
ETHERTYPE_IPV4 = 0x0800
IP_PROTO_UDP = 17
# El puerto donde escucha el agente. Es el que las maquinas de prueba mandan
# adentro con `hostfwd`, asi que del lado del host se llega por otro numero.
AGENT_PORT = 5555


def pick_udp_port():
    """Un puerto libre del host, para el hostfwd con el que se le habla al agente.

    Se lo pide al sistema en vez de elegir un numero: dos maquinas de prueba a
    la vez con el mismo puerto hacen que la segunda **no arranque**, y eso no se
    parece en nada a su causa.
    """
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]
    finally:
        s.close()


def ones_complement(data):
    """La suma con la que se chequean las cabeceras de IP.

    Se suman de a 16 bits, el acarreo vuelve a entrar por abajo, y al final se
    invierte. Es de 1981 y sigue igual.
    """
    if len(data) % 2:
        data += b"\x00"
    total = 0
    for i in range(0, len(data), 2):
        total += int.from_bytes(data[i:i + 2], "big")
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return (~total) & 0xFFFF


def udp_frame(src_mac, dst_mac, src_ip, dst_ip, src_port, dst_port, payload):
    """Un datagrama UDP entero, desde la cabecera de ethernet."""
    udp = (src_port.to_bytes(2, "big") + dst_port.to_bytes(2, "big")
           + (8 + len(payload)).to_bytes(2, "big")
           # El checksum de UDP es opcional sobre IPv4, y cero quiere decir
           # "no lo calcule". No es pereza: abajo hay ethernet, que ya trae su
           # propio CRC y lo pone la placa.
           + bytes(2) + payload)

    ip = bytearray(20)
    ip[0] = 0x45                                    # IPv4, cabecera de 20 bytes
    ip[2:4] = (20 + len(udp)).to_bytes(2, "big")    # cuanto mide todo
    ip[6:8] = (0x4000).to_bytes(2, "big")           # no lo fragmenten
    ip[8] = 64                                      # cuantos saltos aguanta
    ip[9] = IP_PROTO_UDP
    ip[12:16] = src_ip
    ip[16:20] = dst_ip
    # El de IP **no** es opcional, y se calcula sobre la cabecera con el campo
    # del checksum en cero.
    ip[10:12] = ones_complement(bytes(ip)).to_bytes(2, "big")

    frame = dst_mac + src_mac + ETHERTYPE_IPV4.to_bytes(2, "big") + bytes(ip) + udp
    # El minimo de ethernet, otra vez.
    return frame + bytes(max(0, 60 - len(frame)))


def parse_udp(frame, our_ip, our_port):
    """Saca el contenido de un datagrama dirigido a nosotros, o None.

    Devolver None ante cualquier cosa rara es lo correcto para un driver: por
    el cable entra lo que sea, y nada de lo que entra es de fiar.
    """
    if len(frame) < 42 or int.from_bytes(frame[12:14], "big") != ETHERTYPE_IPV4:
        return None
    ip = frame[14:]
    if (ip[0] >> 4) != 4:
        return None
    # La cabecera de IP puede traer opciones, asi que su largo se lee, no se
    # supone. Suponer 20 es el bug clasico de los stacks de juguete.
    head = (ip[0] & 0xF) * 4
    if head < 20 or ip[9] != IP_PROTO_UDP or ip[16:20] != our_ip:
        return None
    total = int.from_bytes(ip[2:4], "big")
    udp = ip[head:total]
    if len(udp) < 8:
        return None
    if int.from_bytes(udp[2:4], "big") != our_port:
        return None
    length = int.from_bytes(udp[4:6], "big")
    return {
        "mac": frame[6:12],
        "ip": bytes(ip[12:16]),
        "port": int.from_bytes(udp[0:2], "big"),
        "payload": bytes(udp[8:length]),
    }


def parse_arp_request(frame, our_ip):
    """Devuelve (mac, ip) del que pregunta por nuestra IP, o None.

    Hace falta responder esto: el otro extremo no puede entregarnos nada
    mientras no sepa que direccion tenemos.
    """
    if len(frame) < 42 or int.from_bytes(frame[12:14], "big") != ETHERTYPE_ARP:
        return None
    if int.from_bytes(frame[20:22], "big") != 1:   # 1 = es una pregunta
        return None
    if frame[38:42] != our_ip:
        return None
    return frame[22:28], frame[28:32]


def arp_reply(our_mac, our_ip, their_mac, their_ip):
    """La respuesta: "esa IP es mia, y esta es mi direccion"."""
    frame = bytearray()
    frame += their_mac + our_mac + ETHERTYPE_ARP.to_bytes(2, "big")
    frame += (1).to_bytes(2, "big") + (0x0800).to_bytes(2, "big")
    frame += bytes([6, 4]) + (2).to_bytes(2, "big")   # 2 = es una respuesta
    frame += our_mac + our_ip
    frame += their_mac + their_ip
    return bytes(frame) + bytes(60 - len(frame))


def serve_udp(nic, our_ip, our_port, timeout_s):
    """Mira lo que entra hasta que llegue un datagrama para nosotros.

    Por el camino contesta los ARP que pregunten por nuestra direccion, que es
    lo que hace que el otro lado pueda entregarnos algo. Todo lo demas se
    descarta: por una placa de red entra el ruido de toda la red.
    """
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        frame, why = nic.poll(timeout_s=0.4)
        if why:
            return None, why
        if frame is None:
            continue
        if (who := parse_arp_request(frame, our_ip)):
            nic.send(arp_reply(nic.mac, our_ip, who[0], who[1]))
            continue
        if (datagram := parse_udp(frame, our_ip, our_port)):
            return datagram, None
    return None, None


def test_udp(proc, timeout, arch, port):
    """Un datagrama del host al agente y la respuesta de vuelta (D5).

    Esta prueba tiene un par de verdad del otro lado: los bytes salen de un
    socket de esta misma maquina, cruzan la red de QEMU, los levanta el driver
    del agente **de la placa**, y la respuesta hace el camino inverso hasta el
    socket. Nada de eso pasa si el driver no anda.

    Y lo que vuelve no es lo que se mando: el agente contesta otra cosa. Un eco
    podria venir de cualquier lado del camino; una respuesta distinta sólo la
    puede haber armado el codigo que corre adentro.
    """
    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, d = ask_verb(700, "describe", {"what": ["pcie"]})
    if not ok or not d["pcie"]:
        print("  esta maquina no informa PCIe")
        return 1

    nic, why = net_bring_up(ask_verb, 701, d["pcie"]["base"])
    if nic is None:
        print(f"  FALLA: {why}")
        return 1
    print(f"  placa lista, direccion {':'.join(f'{b:02x}' for b in nic.mac)}")

    # Un ARP de salida antes que nada. Ademas de comprobar el enlace, le ensena
    # al otro extremo que direccion tenemos: sin eso, lo primero que nos manden
    # se pierde mientras nos busca.
    nic.send(arp_request(nic.mac, QEMU_GATEWAY_IP))

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(1.0)
    # Un numero distinto en cada corrida: si la respuesta trajera uno viejo,
    # seria un datagrama que quedo dando vueltas y no el que acabamos de mandar.
    nonce = os.urandom(4).hex()
    probe = f"PING {nonce}".encode()

    datagram = None
    for intento in range(4):
        sock.sendto(probe, ("127.0.0.1", port))
        datagram, why = serve_udp(nic, QEMU_GUEST_IP, AGENT_PORT, 4.0)
        if why:
            print(f"  FALLA al recibir: {why}")
            nic.release()
            return 1
        if datagram:
            break
        print(f"  (no llego nada; reintento {intento + 1})")

    if datagram is None:
        print("  FALLA: el datagrama del host nunca llego a la placa")
        nic.release()
        return 1
    print(f"  del host llego {datagram['payload']!r}"
          f" desde {'.'.join(str(b) for b in datagram['ip'])}:{datagram['port']}")

    if datagram["payload"] != probe:
        print(f"  FALLA: llego algo que no es lo que se mando: {datagram['payload']!r}")
        nic.release()
        return 1

    # Y la vuelta. Se contesta al que pregunto, con lo que el diga: la direccion
    # y el puerto salen del datagrama que llego, no de un numero horneado.
    answer = f"PONG {nonce}".encode()
    why = nic.send(udp_frame(nic.mac, datagram["mac"], QEMU_GUEST_IP,
                             datagram["ip"], AGENT_PORT, datagram["port"], answer))
    if why:
        print(f"  FALLA al contestar: {why}")
        nic.release()
        return 1

    try:
        back, _ = sock.recvfrom(2048)
    except socket.timeout:
        print("  FALLA: el agente contesto pero la respuesta no llego al host")
        nic.release()
        return 1
    finally:
        sock.close()
        nic.release()

    print(f"  y al host le volvio {back!r}")
    print()
    if back != answer:
        print(f"  FALLA: volvio otra cosa que lo que el agente contesto")
        return 1
    print("  udp: ok")
    return 0


# --- El transporte de D5 ----------------------------------------------------
#
# Hasta aca el driver lo manejaba Python **por el cable**, que para probar el
# driver esta bien y para el transporte no prueba nada: usar el cordon para
# correr la red es exactamente lo que D5 dice que hay que dejar de hacer.
#
# Asi que esto es codigo maquina del agente, corriendo en un nucleo que reclamo
# (D13, D29: ahi manda el, asi que puede correr `raw` y quedarse girando). El
# bucle mueve bytes entre la placa y el buzon de `listen`, y el kernel contesta
# por el buzon sin enterarse de que del otro lado hay una red (D4).
#
# Por que un bucle que gira y no una interrupcion: esta placa no tiene MSI —los
# aparatos viejos avisan por un cable, y para saber que cable hay que interpretar
# AML, un lenguaje entero adentro de ACPI—, asi que no hay a quien instalarle un
# handler. Girar en un nucleo propio es justamente para lo que sirve un nucleo
# propio.
#
# **El buzon es un flujo de bytes, no una cola de mensajes**, y eso decide la
# forma del transporte. El kernel sabe donde termina un pedido porque va
# escaneando el CBOR a medida que entra (`cbor::scan`); del lado del agente
# hacer lo mismo seria un decodificador de CBOR en codigo maquina. Asi que el
# bucle **no interpreta nada**: es un cano. Manda los bytes que haya cuando los
# haya, y quien arma los mensajes es el que esta en las puntas —el kernel de un
# lado, el cliente del otro, que decodifica CBOR y sabe cuando esta completo.
# Un pedido o una respuesta pueden venir en varios datagramas.
#
# Y cada datagrama lleva **un largo adelante** porque las respuestas van de
# tamano fijo. Eso no es capricho: con el datagrama de tamano fijo, la cabecera
# de IP entera —incluido su checksum, que es lo unico que este transporte
# tendria que calcular— es **constante**, y la arma Python una sola vez. El
# bucle solo copia bytes.
TRANSPORT_PAYLOAD = 512      # cuantos bytes de respuesta lleva cada datagrama
TRANSPORT_MAX_REQUEST = 1024  # el pedido mas grande que acepta de una
TRANSPORT_CAPACITY = 4096    # cuanto mide cada anillo del buzon
TRANSPORT_CAP_BITS = 12      # ...y en bits, porque dar la vuelta es una mascara
# Donde vive la senal de "pará", dentro del mismo reclamo que el codigo.
TRANSPORT_STOP_AT = 2048
# Y justo despues, lo que el bucle va anotando de lo que le pasa.
#
# Un transporte que no se puede mirar no se puede arreglar: no imprime nada
# —no tiene por donde, y escribir por el cable desde adentro cambiaria los
# tiempos, que en este proyecto ya costo caro— asi que deja numeros en su
# memoria y se leen despues con `mem.read`. Cada uno separa una hipotesis de
# la siguiente: si `vistos` es cero el paquete no llego a la placa, si
# `aceptados` es cero llego pero no era para nosotros, y asi.
TRANSPORT_COUNTERS_AT = 2064
TRANSPORT_COUNTERS = ["vueltas", "vistos", "aceptados", "al_buzon", "contestados"]

# Los registros que el bucle se guarda de una vuelta a la otra.
_R_RXOFF, _R_RXBUF, _R_RXIDX, _R_TXOFF, _R_TXIDX = 9, 10, 11, 12, 13


def transport_program(arch, where, bell):
    """El bucle del transporte, en codigo maquina.

    `where` trae las direcciones de todo lo que toca — la placa, sus anillos, el
    buzon— porque un programa que las tuviera horneadas no serviria en otra
    maquina. Se las pasa el que lo arma, que las supo por el protocolo (P4).
    """
    a = Asm(arch)
    s = {n: n for n in range(9)}   # los registros de descarte, 0..8

    def anotar(cual):
        """Suma uno a un contador. Usa los dos primeros registros de descarte,
        asi que solo se llama donde esos no tienen nada vivo."""
        a.movi(s[0], where["counters"] + TRANSPORT_COUNTERS.index(cual) * 4)
        a.load(s[1], s[0], 0, 4)
        a.addi(s[1], 1)
        a.store(s[0], 0, s[1], 4)

    a.movi(_R_RXOFF, 0)
    a.movi(_R_RXBUF, 0)
    a.movi(_R_RXIDX, 0)
    a.movi(_R_TXOFF, 0)
    a.movi(_R_TXIDX, 0)

    a.label("vuelta")
    anotar("vueltas")
    # ¿Le dijeron que pare? Es lo unico que lo saca del bucle, y existe para que
    # la prueba pueda terminar: un transporte de verdad no para nunca.
    a.movi(s[0], where["stop"])
    a.load(s[1], s[0], 0, 4)
    a.cmpi(s[1], 0)
    a.bne("listo")

    # --- ¿Llego un paquete? El descriptor lo dice en su byte de estado.
    a.movi(s[0], where["rx_ring"])
    a.add(s[0], _R_RXOFF)
    a.load(s[1], s[0], 12, 1)
    a.andi(s[1], 1)                      # el bit de "esto ya esta"
    a.cmpi(s[1], 0)
    a.beq("respuesta")
    anotar("vistos")

    # El paquete lo escribio la placa por DMA: hay que asegurarse de ver los
    # bytes y no lo que hubiera antes.
    a.barrier()
    a.movi(s[2], where["rx_buf"])
    a.add(s[2], _R_RXBUF)

    # Tres preguntas antes de creerle a un paquete. Por una placa de red entra
    # el ruido de toda la red, y meter cualquier cosa en el buzon seria darle
    # al kernel un CBOR que no lo es.
    a.load(s[1], s[2], 12, 2)            # ¿es IPv4?
    a.cmpi(s[1], ETHERTYPE_IPV4 >> 8 | (ETHERTYPE_IPV4 & 0xFF) << 8)
    a.bne("soltar")
    a.load(s[1], s[2], 26, 4)            # ¿viene de quien esperamos?
    a.movi(s[3], where["peer_ip"])
    a.cmp(s[1], s[3])
    a.bne("soltar")
    a.load(s[1], s[2], 36, 2)            # ¿es para nuestro puerto?
    a.movi(s[3], where["port_be"])
    a.cmp(s[1], s[3])
    a.bne("soltar")

    anotar("aceptados")
    # Guardarse a quien hay que contestarle. La direccion IP no se copia porque
    # es siempre la misma —y por eso el checksum de la cabecera puede ser
    # constante—, pero la MAC y el puerto salen del paquete que llego.
    a.movi(s[4], where["tx_buf"])
    for off in (0, 2, 4):
        a.load(s[1], s[2], 6 + off, 2)
        a.store(s[4], off, s[1], 2)
    a.load(s[1], s[2], 34, 2)
    a.store(s[4], 36, s[1], 2)

    # Cuantos bytes trae. Si no entran, se descarta: mejor perder un pedido que
    # escribir fuera del anillo.
    a.load(s[5], s[2], 42, 2)
    a.cmpi(s[5], TRANSPORT_MAX_REQUEST)
    a.bhs("soltar")

    # --- Del paquete al anillo de pedidos, byte por byte.
    a.movi(s[6], where["mailbox"])
    a.load(s[7], s[6], where["off_req_head"], 4)
    a.movi(s[8], 0)
    a.label("copia_entra")
    a.cmp(s[8], s[5])
    a.bhs("copia_entra_fin")
    a.mov(s[0], s[2])
    a.add(s[0], s[8])
    a.load(s[1], s[0], 44, 1)
    a.mov(s[3], s[7])
    a.add(s[3], s[8])
    a.andi(s[3], TRANSPORT_CAP_BITS)     # dar la vuelta al anillo
    a.movi(s[0], where["req_ring"])
    a.add(s[0], s[3])
    a.store(s[0], 0, s[1], 1)
    a.addi(s[8], 1)
    a.b("copia_entra")
    a.label("copia_entra_fin")
    a.add(s[7], s[5])
    # **Los bytes antes que el indice.** Si el kernel viera el indice nuevo y los
    # bytes viejos leeria basura, y es un nucleo distinto el que mira.
    a.barrier()
    a.store(s[6], where["off_req_head"], s[7], 4)

    anotar("al_buzon")
    # Y avisarle, que es lo unico que lo despierta.
    a.raw(emit_writes(arch, bell["writes"], con_ret=False))

    a.label("soltar")
    # Devolver la ranura: primero limpiarla, despues avisar que esta libre.
    a.movi(s[0], where["rx_ring"])
    a.add(s[0], _R_RXOFF)
    a.movi(s[1], 0)
    a.store(s[0], 8, s[1], 4)
    a.store(s[0], 12, s[1], 4)
    a.barrier()
    a.addi(_R_RXOFF, DESC_BYTES)
    a.andi(_R_RXOFF, 7)                  # 8 descriptores de 16 bytes
    a.addi(_R_RXBUF, RX_BUFFER)
    a.andi(_R_RXBUF, 14)                 # 8 buffers de 2048
    a.addi(_R_RXIDX, 1)
    a.andi(_R_RXIDX, 3)
    a.mov(s[1], _R_RXIDX)                # la cola va una atras de la cabeza
    a.addi(s[1], RING_SLOTS - 1)
    a.andi(s[1], 3)
    a.movi(s[0], where["rdt"])
    a.store(s[0], 0, s[1], 4)

    a.label("respuesta")
    # --- ¿El kernel dejo algo? Sin interpretarlo: lo que haya, sale.
    a.movi(s[6], where["mailbox"])
    a.load(s[0], s[6], where["off_resp_head"], 4)
    a.load(s[1], s[6], where["off_resp_tail"], 4)
    a.cmp(s[0], s[1])
    a.beq("vuelta")
    a.barrier()

    a.mov(s[5], s[0])
    a.sub(s[5], s[1])                    # cuantos bytes hay
    a.trunc32(s[5])                      # ...y los indices dan la vuelta
    a.cmpi(s[5], TRANSPORT_PAYLOAD + 1)
    a.bhs("recortar")
    a.b("largo_listo")
    a.label("recortar")
    a.movi(s[5], TRANSPORT_PAYLOAD)      # lo que sobre va en el proximo
    a.label("largo_listo")

    a.movi(s[4], where["tx_buf"])
    a.store(s[4], 42, s[5], 2)           # el largo, adelante del contenido

    a.movi(s[8], 0)
    a.label("copia_sale")
    a.cmp(s[8], s[5])
    a.bhs("copia_sale_fin")
    a.mov(s[3], s[1])
    a.add(s[3], s[8])
    a.andi(s[3], TRANSPORT_CAP_BITS)
    a.movi(s[0], where["resp_ring"])
    a.add(s[0], s[3])
    a.load(s[2], s[0], 0, 1)
    a.mov(s[0], s[4])
    a.add(s[0], s[8])
    a.store(s[0], 44, s[2], 1)
    a.addi(s[8], 1)
    a.b("copia_sale")
    a.label("copia_sale_fin")
    a.add(s[1], s[5])
    a.barrier()
    a.store(s[6], where["off_resp_tail"], s[1], 4)

    # --- Y mandarlo. El descriptor es casi todo constante porque el datagrama
    #     es de tamano fijo: solo hay que rearmarlo y mover la cola.
    a.movi(s[0], where["tx_ring"])
    a.add(s[0], _R_TXOFF)
    a.movi(s[1], where["tx_buf"])
    a.store(s[0], 0, s[1], 8)
    a.movi(s[1], where["tx_cmd"])
    a.store(s[0], 8, s[1], 4)
    a.movi(s[1], 0)
    a.store(s[0], 12, s[1], 4)
    a.barrier()
    a.addi(_R_TXOFF, DESC_BYTES)
    a.andi(_R_TXOFF, 7)
    a.addi(_R_TXIDX, 1)
    a.andi(_R_TXIDX, 3)
    a.movi(s[0], where["tdt"])
    a.store(s[0], 0, _R_TXIDX, 4)
    anotar("contestados")
    a.b("vuelta")

    a.label("listo")
    a.ret()
    return a.assemble()


def transport_header(our_mac, peer_ip=QEMU_GATEWAY_IP, our_ip=QEMU_GUEST_IP):
    """La cabecera fija de cada datagrama de vuelta: 42 bytes que nunca cambian.

    Que sea fija es lo que le saca al bucle el unico calculo que tendria: el
    checksum de IP depende del largo total y de las direcciones, y con el
    datagrama de tamano fijo y un solo destinatario los tres son constantes. Se
    calcula aca, una vez, en Python.

    Los unicos huecos son la MAC y el puerto del destinatario, que el bucle
    copia del paquete que llego.
    """
    payload = 2 + TRANSPORT_PAYLOAD       # el largo va adelante del contenido
    ip = bytearray(20)
    ip[0] = 0x45
    ip[2:4] = (20 + 8 + payload).to_bytes(2, "big")
    ip[6:8] = (0x4000).to_bytes(2, "big")
    ip[8] = 64
    ip[9] = IP_PROTO_UDP
    ip[12:16] = our_ip
    ip[16:20] = peer_ip
    ip[10:12] = ones_complement(bytes(ip)).to_bytes(2, "big")

    udp = (AGENT_PORT.to_bytes(2, "big") + bytes(2)      # el puerto lo pone el bucle
           + (8 + payload).to_bytes(2, "big") + bytes(2))
    return (bytes(6) + our_mac + ETHERTYPE_IPV4.to_bytes(2, "big")
            + bytes(ip) + udp)


def transport_frames(data, buffer):
    """Saca de los datagramas que llegaron los bytes del flujo, en orden.

    Cada uno trae su largo adelante porque el datagrama es de tamano fijo: sin
    eso no habria como distinguir el contenido del relleno.
    """
    if len(data) < 2:
        return buffer
    count = int.from_bytes(data[0:2], "little")
    return buffer + data[2:2 + count]


def test_transport(proc, timeout, arch, port):
    """El kernel contesta el protocolo **por la red** (D5, D17, D28).

    Es lo que el proyecto venia prometiendo desde el primer commit y no tenia:
    un transporte que escribio el agente, entregado al kernel con `listen`, por
    el que viaja el mismo CBOR que por el cable.

    La prueba esta armada para que el cable no pueda estar ayudando. El pedido
    **no sale por el cable**: sale de un socket del host, entra por la placa, y
    lo mueve al buzon un bucle de codigo maquina del agente que corre en su
    propio nucleo. El cable se usa solo para armar todo y para preguntar
    despues, que es justo lo que D17 dice que tiene que seguir andando.
    """
    import struct
    failures = []

    def ask_verb(n, verb, args):
        resp, _ = ask(proc, [n, verb, args], timeout)
        _, ok, load = resp
        return ok, load

    ok, d = ask_verb(800, "describe", {"what": ["pcie", "channel"]})
    if not ok or not d["pcie"]:
        print("  esta maquina no informa PCIe")
        return 1
    ch = d["channel"]
    bell = ch.get("doorbell")
    if not bell:
        print("  el kernel no publica timbre del buzon")
        return 1
    L = ch["layout"]

    # El nucleo primero, **antes de reclamar memoria**: en x86_64 el trampolin
    # que arranca un nucleo pasa por una pagina baja y fija, y si el driver la
    # reclama antes, el kernel se queda sin poder arrancarlo.
    core = core_for_raw(ask_verb, 801)
    if core is None:
        print("  no hay un nucleo libre donde correr el transporte")
        return 1

    nic, why = net_bring_up(ask_verb, 810, d["pcie"]["base"])
    if nic is None:
        print(f"  FALLA: {why}")
        return 1
    print(f"  placa lista, direccion {':'.join(f'{b:02x}' for b in nic.mac)}")

    # El buzon, armado con los offsets que publica el kernel (D28: nada horneado).
    ok, mb = ask_verb(830, "mem.claim", {"bytes": 16384, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar el buzon: {mb}")
        nic.release()
        return 1
    head = bytearray(L["rings"])
    struct.pack_into("<I", head, L["magic"], ch["magic"])
    struct.pack_into("<I", head, L["version"], ch["version"])
    struct.pack_into("<I", head, L["capacity"], TRANSPORT_CAPACITY)
    ask_verb(831, "mem.write", {"handle": mb["handle"], "bytes": bytes(16384)})
    ask_verb(832, "mem.write", {"handle": mb["handle"], "off": 0, "bytes": bytes(head)})
    ok, r = ask_verb(833, "listen", {"handle": mb["handle"]})
    if not ok:
        print(f"  el kernel no adopto el buzon: {r}")
        nic.release()
        return 1
    print(f"  buzon entregado al kernel con listen, anillos de"
          f" {TRANSPORT_CAPACITY} bytes")

    # La cabecera fija de las respuestas, ya con su checksum.
    ok, code_mem = ask_verb(834, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar memoria para el codigo: {code_mem}")
        nic.release()
        return 1
    where = {
        "stop": code_mem["start"] + TRANSPORT_STOP_AT,
        "counters": code_mem["start"] + TRANSPORT_COUNTERS_AT,
        "rx_ring": nic.rx_ring["start"],
        "rx_buf": nic.rx_buf["start"],
        "tx_ring": nic.tx_ring["start"],
        "tx_buf": nic.tx_buf["start"],
        "mailbox": mb["start"],
        "req_ring": mb["start"] + L["rings"],
        "resp_ring": mb["start"] + L["rings"] + TRANSPORT_CAPACITY,
        "off_req_head": L["request_head"],
        "off_resp_head": L["response_head"],
        "off_resp_tail": L["response_tail"],
        "rdt": nic.window["start"] + E1000_RDT,
        "tdt": nic.window["start"] + E1000_TDT,
        "peer_ip": int.from_bytes(QEMU_GATEWAY_IP, "little"),
        "port_be": int.from_bytes(AGENT_PORT.to_bytes(2, "big"), "little"),
        "tx_cmd": (44 + TRANSPORT_PAYLOAD)
                  | ((E1000_TXD_EOP | E1000_TXD_IFCS | E1000_TXD_RS) << 24),
    }
    code = transport_program(arch, where, bell)
    print(f"  el transporte son {len(code)} bytes de codigo maquina del agente")
    ask_verb(836, "mem.write", {"handle": code_mem["handle"], "off": 0, "bytes": code})

    # Un ARP antes de soltar el bucle: le ensena al otro extremo nuestra
    # direccion, para que lo primero que mandemos desde el host se pueda
    # entregar. Despues de esto Python no toca mas la placa — es del bucle.
    nic.send(arp_request(nic.mac, QEMU_GATEWAY_IP))
    while True:
        frame, _ = nic.poll(timeout_s=1.0)
        if frame is None:
            break
    # Y los anillos vuelven a cero, porque el bucle empieza a contar de ahi.
    nic.rearm()

    # La cabecera fija de las respuestas va **ultima**, y esto costo encontrarlo:
    # `send` arma cada paquete en este mismo buffer, asi que el ARP de recien la
    # pisaba entera. El sintoma no se parecia a la causa — el bucle contaba
    # respuestas mandadas, la placa las mandaba de verdad, y del otro lado no
    # llegaba nada, porque lo que salia tenia la cabecera de un ARP.
    ask_verb(835, "mem.write", {"handle": nic.tx_buf["handle"], "off": 0,
                                "bytes": transport_header(nic.mac)})

    ok, r = ask_verb(837, "exec", {"handle": code_mem["handle"], "mode": "raw",
                                   "core": core, "wait": False,
                                   "deadline_ms": 60000})
    if not ok:
        print(f"  no se pudo arrancar el transporte: {r}")
        nic.release()
        return 1
    print(f"  corriendo en un nucleo del agente; de aca en mas la placa es suya")

    # --- Y ahora la prueba: hablarle al kernel **sin el cable**.
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(2.0)
    want_id = 4242
    request = enc([want_id, "describe", {"what": ["clock"]}])
    datagram = bytes([len(request) & 0xFF, len(request) >> 8]) + request

    answer, raw_back = None, b""
    for intento in range(5):
        sock.sendto(datagram, ("127.0.0.1", port))
        deadline = time.time() + 3.0
        while time.time() < deadline:
            try:
                back, _ = sock.recvfrom(4096)
            except socket.timeout:
                break
            raw_back = transport_frames(back, raw_back)
            try:
                # CBOR se sabe terminar solo: por eso el bucle puede ser un cano
                # que no interpreta nada y el que arma los mensajes es este lado.
                answer, _ = dec(raw_back)
                break
            except Exception:
                continue          # todavia no llego entero; viene otro datagrama
        if answer is not None:
            break
        print(f"  (sin respuesta todavia; reintento {intento + 1})")

    # Lo que el bucle fue anotando. Se lee siempre, no solo cuando falla: es la
    # unica ventana a un codigo que corre en otro nucleo y no habla por el cable.
    ok, r = ask_verb(844, "mem.read", {"handle": code_mem["handle"],
                                       "off": TRANSPORT_COUNTERS_AT,
                                       "len": 4 * len(TRANSPORT_COUNTERS)})
    if ok:
        cuenta = {name: int.from_bytes(r["bytes"][i * 4:i * 4 + 4], "little")
                  for i, name in enumerate(TRANSPORT_COUNTERS)}
        print("  lo que anoto el bucle: "
              + ", ".join(f"{k}={v}" for k, v in cuenta.items()))

    if answer is None:
        failures.append("el kernel no contesto por la red")
    else:
        print(f"  por la RED contesto: id={answer[0]} ok={answer[1]}")
        if answer[0] != want_id:
            failures.append(f"contesto con otro id: {answer[0]} en vez de {want_id}")
        if not answer[1]:
            failures.append(f"contesto un error: {answer[2]}")
        elif not isinstance(answer[2], dict) or "clock" not in answer[2]:
            failures.append(f"la respuesta no es la que se pidio: {answer[2]}")
        else:
            print(f"  y lo que contesto es la maquina de verdad:"
                  f" clock={answer[2]['clock']}")
    sock.close()

    # Pararlo, y comprobar que el cable siguio vivo todo el tiempo (D17).
    ask_verb(838, "mem.write", {"handle": code_mem["handle"],
                                "off": TRANSPORT_STOP_AT, "bytes": (1).to_bytes(4, "little")})
    # Y esperar a que **de verdad** haya parado antes de soltar nada. Soltar la
    # placa con el bucle todavia girando seria dejarlo escribiendo en memoria
    # que ya no es nuestra: el IOMMU lo frenaria, pero confiar en eso es al
    # reves de como se hace.
    detenido = False
    deadline = time.time() + 5.0
    while time.time() < deadline:
        ok, d = ask_verb(839, "describe", {"what": ["cores"]})
        if ok and any(c["handle"] == core and c["state"] == "idle"
                      for c in (d.get("cores") or [])):
            detenido = True
            break
    if not detenido:
        failures.append("el transporte no paro cuando se le dijo que parara")
    else:
        print("  y para cuando se le dice: el nucleo volvio a quedar libre")

    ok, d = ask_verb(842, "describe", {"what": ["cable"]})
    if ok:
        dropped = (d.get("cable") or {}).get("dropped")
        print(f"  el cable nunca dejo de atender, y no perdio bytes: dropped={dropped}")
        if dropped:
            failures.append(f"se perdieron {dropped} bytes del cable")

    nic.release()
    ask_verb(840, "release", {"handle": mb["handle"]})
    ask_verb(841, "release", {"handle": code_mem["handle"]})

    print()
    if failures:
        for f in failures:
            print(f"  FALLA: {f}")
        return 1
    print("  transporte: ok")
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
    ap.add_argument("--net", action="store_true",
                    help="le habla a la placa de red con los once verbos (D5)")
    ap.add_argument("--udp", action="store_true",
                    help="manda un datagrama del host al agente y espera la vuelta")
    ap.add_argument("--transport", action="store_true",
                    help="el kernel contesta el protocolo por la red, sin el cable (D5)")
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
    env = dict(os.environ)
    if args.no_acpi:
        env["NO_ACPI"] = "1"
    # Por que puerto del host se le habla a la placa del agente. Se elige uno
    # libre en vez de horneado para que dos maquinas puedan correr a la vez: si
    # el puerto estuviera tomado, QEMU no arrancaria — y la falla se veria como
    # "la maquina no bootea", que no se parece en nada a su causa.
    netport = int(env.get("NETPORT") or pick_udp_port())
    env["NETPORT"] = str(netport)
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
        if args.net:
            print("\n== un driver de red, escrito con los once verbos (D5) ==")
            rc |= test_net(proc, args.timeout, args.arch)
        if args.udp:
            print(f"\n== un datagrama del host al agente y la vuelta"
                  f" (puerto {netport}) ==")
            rc |= test_udp(proc, args.timeout, args.arch, netport)
        if args.transport:
            print(f"\n== el kernel contesta el protocolo POR LA RED"
                  f" (puerto {netport}) ==")
            rc |= test_transport(proc, args.timeout, args.arch, netport)
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
