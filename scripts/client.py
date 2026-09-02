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
import os
import select
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
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
RESCUE_PROMPT = b"mandar cualquier byte"


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

    # El blob de prueba son los mismos bytes que el programa que anda de
    # `--exec`: deja 0xc0ffee en el primer registro y vuelve. Asi el kernel puede
    # contar que corrio sin saber nada de lo que hace (D20: el blob es codigo del
    # agente, el kernel no lo mira).
    if args.write_blob:
        with open(args.write_blob, "wb") as f:
            f.write(PROGRAMS[args.arch]["ok"])
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
    # Sin ACPI el firmware le pasa al kernel un device tree en su lugar: es el
    # otro dialecto en el que una maquina se describe, y el kernel tiene que
    # poder averiguar lo mismo por los dos (P4).
    env = dict(os.environ, NO_ACPI="1") if args.no_acpi else None
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, cwd=ROOT, bufsize=0, env=env)
    try:
        print(f"arrancando {args.arch} en QEMU...")
        read_until_marker(proc, args.timeout, show=True, cancel_blob=args.cancel_blob)

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
        return rc
    finally:
        proc.kill()
        proc.wait()


if __name__ == "__main__":
    sys.exit(main())
