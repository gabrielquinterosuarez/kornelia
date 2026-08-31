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
    ./scripts/client.py --memoria                # el lazo: claim, write, read, release
"""

import argparse
import os
import select
import subprocess
import sys

RAIZ = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MARCA = b"-- CBOR --"


# --------------------------------------------------------------------------
# CBOR minimo
# --------------------------------------------------------------------------

class Incompleto(Exception):
    """Todavia no llegaron todos los bytes."""


def enc_cabeza(mayor, valor):
    if valor < 24:
        return bytes([mayor << 5 | valor])
    for limite, ai, largo in ((0x100, 24, 1), (0x10000, 25, 2),
                              (0x100000000, 26, 4), (1 << 64, 27, 8)):
        if valor < limite:
            return bytes([mayor << 5 | ai]) + valor.to_bytes(largo, "big")
    raise ValueError("valor fuera de rango")


def enc(v):
    if isinstance(v, bool):
        return b"\xf5" if v else b"\xf4"
    if v is None:
        return b"\xf6"
    if isinstance(v, int):
        return enc_cabeza(0, v)
    if isinstance(v, bytes):
        return enc_cabeza(2, len(v)) + v
    if isinstance(v, str):
        b = v.encode()
        return enc_cabeza(3, len(b)) + b
    if isinstance(v, list):
        return enc_cabeza(4, len(v)) + b"".join(enc(x) for x in v)
    if isinstance(v, dict):
        return enc_cabeza(5, len(v)) + b"".join(enc(k) + enc(x) for k, x in v.items())
    raise TypeError(f"no se como codificar {type(v)}")


def dec(b, i=0):
    """Devuelve (valor, posicion_siguiente). Levanta Incompleto si falta."""
    if i >= len(b):
        raise Incompleto
    ib = b[i]
    mayor, ai = ib >> 5, ib & 0x1F
    i += 1

    if ai < 24:
        valor = ai
    elif ai in (24, 25, 26, 27):
        largo = {24: 1, 25: 2, 26: 4, 27: 8}[ai]
        if i + largo > len(b):
            raise Incompleto
        valor = int.from_bytes(b[i:i + largo], "big")
        i += largo
    else:
        raise ValueError(f"cbor invalido: 0x{ib:02x}")

    if mayor == 0:
        return valor, i
    if mayor == 1:
        return -1 - valor, i
    if mayor in (2, 3):
        if i + valor > len(b):
            raise Incompleto
        crudo = b[i:i + valor]
        return (crudo if mayor == 2 else crudo.decode()), i + valor
    if mayor == 4:
        salida = []
        for _ in range(valor):
            x, i = dec(b, i)
            salida.append(x)
        return salida, i
    if mayor == 5:
        salida = {}
        for _ in range(valor):
            k, i = dec(b, i)
            v, i = dec(b, i)
            salida[k] = v
        return salida, i
    if mayor == 7:
        return {20: False, 21: True, 22: None}.get(valor, f"simple({valor})"), i
    raise ValueError(f"tipo mayor no soportado: {mayor}")


# --------------------------------------------------------------------------
# Hablar con el kernel
# --------------------------------------------------------------------------

def leer_hasta_la_marca(proc, timeout, mostrar):
    """Consume la salida de texto del arranque hasta que empieza el binario."""
    buf = b""
    while MARCA not in buf:
        listo, _, _ = select.select([proc.stdout], [], [], timeout)
        if not listo:
            raise TimeoutError("el kernel nunca llego a la marca del protocolo")
        c = proc.stdout.read(1)
        if not c:
            raise EOFError("QEMU se cerro antes de arrancar el protocolo")
        buf += c
    # El resto del renglon de la marca.
    while not buf.endswith(b"\n"):
        buf += proc.stdout.read(1)

    if mostrar:
        texto = buf.decode(errors="replace").replace("\r", "")
        for linea in texto.splitlines():
            if linea.strip():
                print(f"  {linea}")
    return buf


def pedir(proc, mensaje, timeout):
    """Manda un pedido y espera una respuesta completa."""
    proc.stdin.write(enc(mensaje))
    proc.stdin.flush()

    buf = b""
    while True:
        listo, _, _ = select.select([proc.stdout], [], [], timeout)
        if not listo:
            raise TimeoutError(f"sin respuesta (llegaron {len(buf)} bytes: {buf.hex()})")
        trozo = proc.stdout.read(1)
        if not trozo:
            raise EOFError("QEMU se cerro")
        buf += trozo
        try:
            valor, fin = dec(buf)
            return valor, buf[:fin]
        except Incompleto:
            continue


# --------------------------------------------------------------------------
# Presentacion
# --------------------------------------------------------------------------

def humano(n):
    for unidad, tam in (("GiB", 1 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)):
        if n >= tam and n % tam == 0:
            return f"{n // tam} {unidad}"
    return f"{n} B"


def mostrar(carga):
    if "memory" in carga and isinstance(carga["memory"], list):
        regiones = carga["memory"]
        print(f"\n  memory: {len(regiones)} regiones")
        for r in regiones:
            crudo = f"  (tipo crudo {r[3]})" if len(r) > 3 else ""
            print(f"    {r[0]:#018x}  {humano(r[1]):>10}  {r[2]}{crudo}")
        carga = {k: v for k, v in carga.items() if k != "memory"}

    for clave, valor in carga.items():
        print(f"\n  {clave}: {valor}")


def prueba_de_memoria(proc, timeout):
    """El lazo completo: reclamar, escribir, leer de vuelta, soltar.

    Es la primera vez que el agente no solo mira la maquina sino que la usa.
    """
    fallas = []

    def pedir_verbo(n, verbo, args):
        resp, _ = pedir(proc, [n, verbo, args], timeout)
        _, ok, carga = resp
        print(f"  {verbo:<10} {'ok ' if ok else 'ERROR'} {carga}")
        return ok, carga

    # 1. Reclamar 4 KiB alineados a 4 KiB.
    ok, c = pedir_verbo(1, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        return 1
    h, start = c["handle"], c["start"]
    if start % 4096 != 0:
        fallas.append(f"no respeto la alineacion: {start:#x}")
    if c["kind"] != "free":
        fallas.append(f"entrego memoria que no es libre: {c['kind']}")

    # 2. Escribir un patron reconocible.
    patron = bytes([0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x11, 0x22, 0x33])
    ok, w = pedir_verbo(2, "mem.write", {"handle": h, "off": 16, "bytes": patron})
    if not ok or w.get("written") != len(patron):
        fallas.append("la escritura no informo lo que se escribio")

    # 3. Leerlo de vuelta del mismo lugar.
    ok, rd = pedir_verbo(3, "mem.read", {"handle": h, "off": 16, "len": len(patron)})
    if not ok or rd.get("bytes") != patron:
        fallas.append(f"lo leido no es lo escrito: {rd}")

    # 4. Fuera del reclamo tiene que fallar, no leer memoria ajena.
    ok, e = pedir_verbo(4, "mem.read", {"handle": h, "off": 4090, "len": 16})
    if ok or e.get("error") != "out-of-bounds":
        fallas.append("dejo leer fuera del reclamo")

    # 5. La memoria del kernel no se entrega.
    ok, e = pedir_verbo(5, "mem.claim", {"at": start, "bytes": 4096})
    if ok or e.get("error") != "already-claimed":
        fallas.append("dejo reclamar dos veces lo mismo")

    # 6. Lo reclamado se ve en describe (D14).
    ok, d = pedir_verbo(6, "describe", {"what": ["claims"]})
    if not ok or not any(x["handle"] == h for x in d.get("claims", [])):
        fallas.append("el reclamo no aparece en describe")

    # 7. Soltarlo, y que deje de existir.
    ok, _ = pedir_verbo(7, "release", {"handle": h})
    if not ok:
        fallas.append("no se pudo soltar")
    ok, e = pedir_verbo(8, "mem.read", {"handle": h, "len": 4})
    if ok or e.get("error") != "no-such-handle":
        fallas.append("el handle sigue vivo despues de soltarlo")

    print()
    if fallas:
        for f in fallas:
            print(f"  FALLA: {f}")
        return 1
    print("  lazo de memoria completo: ok")
    return 0


# Codigo maquina escrito a mano. Son los dos programas mas chicos que sirven
# para probar las dos salidas de `exec`: volver bien y fallar.
PROGRAMAS = {
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


def prueba_de_exec(proc, timeout, arch):
    """Sube codigo maquina de verdad, lo corre, y comprueba las dos salidas.

    Esta es la tesis del proyecto: el agente escribe codigo, lo corre, y si
    esta mal el fault vuelve como un dato en vez de matar la maquina.
    """
    prog = PROGRAMAS[arch]
    fallas = []

    def pedir_verbo(n, verbo, args):
        resp, _ = pedir(proc, [n, verbo, args], timeout)
        _, ok, carga = resp
        return ok, carga

    for nombre, codigo, espera_fault in (
        ("un programa que anda", prog["ok"], False),
        ("un programa que falla", prog["falla"], True),
        ("un programa que rompe la pila y falla", prog["pila_rota"], True),
    ):
        ok, c = pedir_verbo(10, "mem.claim", {"bytes": 4096, "align": 4096})
        if not ok:
            fallas.append(f"{nombre}: no se pudo reclamar")
            continue
        h = c["handle"]

        ok, _ = pedir_verbo(11, "mem.write", {"handle": h, "bytes": codigo})
        if not ok:
            fallas.append(f"{nombre}: no se pudo subir")
            continue

        print(f"  {nombre}: {codigo.hex()}")
        ok, r = pedir_verbo(12, "exec", {"handle": h})
        if not ok:
            fallas.append(f"{nombre}: exec fallo: {r}")
            continue

        print(f"    faulted={r['faulted']}")
        if r["faulted"] != espera_fault:
            fallas.append(f"{nombre}: faulted={r['faulted']}, se esperaba {espera_fault}")

        if espera_fault:
            f = r["fault"]
            print(f"    fault: {f}")
            if not f or f.get("cause") != "page-fault":
                fallas.append(f"{nombre}: la causa no es page-fault: {f}")
            if f and f.get("address") != 0x400000000000:
                fallas.append(f"{nombre}: la direccion no es la que se toco: {f}")
        else:
            reg = prog["registro"]
            valor = r["registers"].get(reg)
            print(f"    {reg}={valor:#x}")
            if valor != 0xC0FFEE:
                fallas.append(f"{nombre}: {reg}={valor:#x}, se esperaba 0xc0ffee")

        pedir_verbo(13, "release", {"handle": h})

    # Y lo mas importante: la maquina sigue contestando despues del fault.
    ok, _ = pedir_verbo(14, "describe", {})
    if not ok:
        fallas.append("la maquina dejo de contestar despues del fault")
    else:
        print("  la maquina sigue viva despues del fault")

    print()
    if fallas:
        for f in fallas:
            print(f"  FALLA: {f}")
        return 1
    print("  exec: ok")
    return 0


def prueba_de_nucleos(proc, timeout):
    """Reclama todos los nucleos menos el que atiende, y los arranca."""
    fallas = []

    def pedir_verbo(n, verbo, args):
        resp, _ = pedir(proc, [n, verbo, args], timeout)
        _, ok, carga = resp
        return ok, carga

    ok, d = pedir_verbo(20, "describe", {"what": ["cpus"]})
    if not ok:
        print("  no se pudo listar los nucleos")
        return 1
    cpus = d["cpus"]
    print(f"  la maquina informa {len(cpus)} nucleos: {[c['id'] for c in cpus]}")

    arrancados = []
    for c in cpus:
        ok, r = pedir_verbo(21, "core.claim", {"id": c["id"]})
        if ok:
            print(f"    id={c['id']} -> handle {r['handle']}, {r['state']}")
            arrancados.append(c["id"])
            if r["state"] != "idle":
                fallas.append(f"el nucleo {c['id']} quedo en {r['state']}")
        else:
            # Uno tiene que fallar: el que esta contestando.
            print(f"    id={c['id']} -> {r['error']}")
            if r["error"] not in ("is-boot-core", "core-not-usable"):
                fallas.append(f"el nucleo {c['id']} fallo con {r['error']}")

    if not arrancados and len(cpus) > 1:
        fallas.append("no se pudo arrancar ni un nucleo")

    # Reclamarlo dos veces tiene que fallar.
    if arrancados:
        ok, e = pedir_verbo(22, "core.claim", {"id": arrancados[0]})
        if ok or e.get("error") != "already-claimed":
            fallas.append("dejo reclamar dos veces el mismo nucleo")

    # Y uno que no existe, tambien.
    ok, e = pedir_verbo(23, "core.claim", {"id": 9999})
    if ok or e.get("error") != "no-such-core":
        fallas.append("dejo reclamar un nucleo inexistente")

    # Los reclamados se ven en describe.
    ok, d = pedir_verbo(24, "describe", {"what": ["cores"]})
    if not ok or len(d.get("cores", [])) != len(arrancados):
        fallas.append(f"describe no informa los nucleos reclamados: {d}")
    else:
        print(f"  describe informa {len(d['cores'])} reclamados")

    # Y la maquina sigue contestando con los otros nucleos corriendo.
    ok, _ = pedir_verbo(25, "describe", {})
    if not ok:
        fallas.append("la maquina dejo de contestar")

    print()
    if fallas:
        for f in fallas:
            print(f"  FALLA: {f}")
        return 1
    print(f"  nucleos: ok ({len(arrancados)} arrancados)")
    return 0


def mov_reg_imm64(reg, v):
    """movz/movk para dejar un inmediato de 64 bits en xN (aarch64)."""
    out = b""
    primero = True
    for hw in range(4):
        trozo = (v >> (16 * hw)) & 0xFFFF
        if trozo == 0 and not primero:
            continue
        base = 0xD2800000 if primero else 0xF2800000
        out += (base | (hw << 21) | (trozo << 5) | reg).to_bytes(4, "little")
        primero = False
    return out or (0xD2800000 | reg).to_bytes(4, "little")


def emitir_escrituras(arch, writes, con_ret=True):
    """Codigo maquina que hace esas escrituras de 32 bits y vuelve.

    Es lo que haria el driver de red del agente para tocar el timbre. Se genera
    a mano porque el kernel publica direcciones, no codigo.
    """
    if arch == "x86_64":
        code = b""
        for addr, val, _ancho in writes:
            code += b"\x48\xb8" + addr.to_bytes(8, "little")   # mov rax, addr
            code += b"\xc7\x00" + (val & 0xFFFFFFFF).to_bytes(4, "little")  # mov [rax], val
        return code + (b"\xc3" if con_ret else b"")            # ret

    # aarch64: armar la direccion en x0 y el valor en w1, y guardar.
    def mov_x0(v):
        out = b""
        primero = True
        for hw in range(4):
            trozo = (v >> (16 * hw)) & 0xFFFF
            if trozo == 0 and not primero:
                continue
            base = 0xD2800000 if primero else 0xF2800000
            out += (base | (hw << 21) | (trozo << 5) | 0).to_bytes(4, "little")
            primero = False
        return out or (0xD2800000).to_bytes(4, "little")

    def mov_w1(v):
        out = b""
        primero = True
        for hw in range(2):
            trozo = (v >> (16 * hw)) & 0xFFFF
            if trozo == 0 and not primero:
                continue
            base = 0x52800000 if primero else 0x72800000
            out += (base | (hw << 21) | (trozo << 5) | 1).to_bytes(4, "little")
            primero = False
        return out or (0x52800000 | 1).to_bytes(4, "little")

    code = b""
    for addr, val, _ancho in writes:
        code += mov_x0(addr) + mov_w1(val & 0xFFFFFFFF)
        code += (0xB9000001).to_bytes(4, "little")   # str w1, [x0]
    return code + ((0xD65F03C0).to_bytes(4, "little") if con_ret else b"")  # ret


def prueba_de_timbre(proc, timeout, arch):
    """El agente toca el timbre del kernel con codigo maquina propio.

    Es lo que va a hacer su driver de red: dejar el pedido en el buzon y avisar.
    """
    fallas = []

    def pedir_verbo(n, verbo, args):
        resp, _ = pedir(proc, [n, verbo, args], timeout)
        _, ok, carga = resp
        return ok, carga

    ok, d = pedir_verbo(40, "describe", {"what": ["channel"]})
    if not ok:
        print("  no se pudo leer el acuerdo del canal")
        return 1
    ch = d["channel"]
    campana = ch.get("doorbell")
    if not campana:
        print("  el kernel no publica timbre del buzon")
        return 1

    antes = ch["rings"]
    print(f"  el timbre es el {campana['id']}, sono {antes} veces hasta ahora")
    for a, v, w in campana["writes"]:
        print(f"    escribir {v:#x} ({w} bytes) en {a:#x}")

    # Codigo maquina que hace esas escrituras: exactamente lo que haria el
    # driver del agente.
    codigo = emitir_escrituras(arch, campana["writes"])
    ok, c = pedir_verbo(41, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar memoria: {c}")
        return 1
    h = c["handle"]
    pedir_verbo(42, "mem.write", {"handle": h, "bytes": codigo})

    print(f"  el agente toca el timbre: {codigo.hex()}")
    ok, r = pedir_verbo(43, "exec", {"handle": h})
    if not ok or r.get("faulted"):
        fallas.append(f"el codigo del timbre fallo: {r}")

    ok, d = pedir_verbo(44, "describe", {"what": ["channel"]})
    despues = d["channel"]["rings"] if ok else -1
    print(f"  y ahora sono {despues} veces")

    if despues <= antes:
        fallas.append("el timbre no sono: la interrupcion del agente no llego")

    pedir_verbo(45, "release", {"handle": h})

    print()
    if fallas:
        for f in fallas:
            print(f"  FALLA: {f}")
        return 1
    print("  timbre: ok")
    return 0


def prueba_de_permiso(proc, timeout, arch):
    """Memoria pedida para el agente, y la prueba de que el bit esta puesto.

    La comprobacion no es mirar lo que dice el kernel: es que una memoria
    marcada para el agente **deja de ser ejecutable con privilegio**. Asi que si
    el bit se puso de verdad, correr codigo ahi con `exec` tiene que fallar con
    un fault de permiso al buscar la instruccion.
    """
    fallas = []

    def pedir_verbo(n, verbo, args):
        resp, _ = pedir(proc, [n, verbo, args], timeout)
        _, ok, carga = resp
        return ok, carga

    ret = (0xD65F03C0).to_bytes(4, "little") if arch == "aarch64" else b"\xc3"

    # Primero, memoria comun: correr ahi tiene que andar.
    ok, c = pedir_verbo(80, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar: {c}")
        return 1
    print(f"  memoria comun:      user={c['user']}, {c['bytes']} bytes")
    if c["user"]:
        fallas.append("una memoria que no se pidio para el agente vino marcada")
    pedir_verbo(81, "mem.write", {"handle": c["handle"], "bytes": ret})
    ok, r = pedir_verbo(82, "exec", {"handle": c["handle"]})
    if not ok or r.get("faulted"):
        fallas.append(f"no se pudo correr codigo en memoria comun: {r}")
    else:
        print("    y el kernel puede correr codigo ahi")

    # Ahora memoria para el agente.
    ok, u = pedir_verbo(83, "mem.claim", {"bytes": 4096, "user": True})
    if not ok:
        print(f"  no se pudo reclamar para el agente: {u}")
        return 1
    print(f"  para el agente:     user={u['user']}, {u['bytes']} bytes, en {u['start']:#x}")
    if not u["user"]:
        fallas.append("se pidio para el agente y no quedo marcada")
    # Pedirlo redondea al bloque de la tabla: el kernel informa lo que quedo.
    if u["bytes"] < 2 * 1024 * 1024:
        fallas.append(f"no se redondeo al bloque: {u['bytes']} bytes")
    if u["start"] % (2 * 1024 * 1024) != 0:
        fallas.append(f"no quedo alineada al bloque: {u['start']:#x}")

    pedir_verbo(84, "mem.write", {"handle": u["handle"], "bytes": ret})
    ok, r = pedir_verbo(85, "exec", {"handle": u["handle"]})
    if ok and not r.get("faulted"):
        fallas.append("el kernel pudo correr codigo en memoria del agente: el bit NO se puso")
    else:
        # Cada arquitectura lo cuenta a su manera: aarch64 dice que no pudo
        # buscar la instruccion, x86 lo reporta como fault de pagina.
        causa = r.get("fault", {}).get("cause") if ok else "?"
        print(f"    y el kernel YA NO puede correr codigo ahi: {causa}")

    # Y al soltarla, el permiso se saca: si quedara, seria un agujero silencioso.
    pedir_verbo(86, "release", {"handle": u["handle"]})
    ok, v = pedir_verbo(87, "mem.claim", {"at": u["start"], "bytes": 4096})
    if ok:
        pedir_verbo(88, "mem.write", {"handle": v["handle"], "bytes": ret})
        ok2, r2 = pedir_verbo(89, "exec", {"handle": v["handle"]})
        if not ok2 or r2.get("faulted"):
            fallas.append("al soltarla no se le saco el permiso")
        else:
            print("    y al soltarla vuelve a ser del kernel")

    print()
    if fallas:
        for f in fallas:
            print(f"  FALLA: {f}")
        return 1
    print("  permiso: ok")
    return 0


def prueba_durante_exec(proc, timeout, arch):
    """El handler del agente corre DURANTE un exec largo (D9, D29).

    La prueba es de las que no admiten interpretacion: el codigo del agente
    dispara su interrupcion y despues se queda esperando a que su propio handler
    le escriba una bandera. Si las interrupciones estuvieran cerradas durante el
    exec, esa espera no terminaria nunca y esto colgaria.
    """
    fallas = []

    def pedir_verbo(n, verbo, args):
        resp, _ = pedir(proc, [n, verbo, args], timeout)
        _, ok, carga = resp
        return ok, carga

    # Otra interrupcion que la que usa --handler: son dos pruebas que pueden
    # correr en el mismo arranque, y un handler ya instalado no se reemplaza.
    INT = 35 if arch == "aarch64" else 6
    FLAG_OFF = 2048

    ok, c = pedir_verbo(70, "mem.claim", {"bytes": 4096, "align": 4096})
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
    pedir_verbo(71, "mem.write", {"handle": h, "off": 0, "bytes": handler})
    pedir_verbo(72, "mem.write", {"handle": h, "off": FLAG_OFF, "bytes": b"\x00"})

    ok, r = pedir_verbo(73, "irq.install", {"handle": h, "interrupt": INT})
    if not ok:
        print(f"  no se pudo instalar el handler: {r}")
        return 1

    ok, d = pedir_verbo(74, "describe", {"what": ["handlers"]})
    # El de esta prueba, que puede no ser el unico instalado.
    hh = next(x for x in d["handlers"] if x["interrupt"] == INT)
    antes = hh["served"]

    # El codigo del agente: disparar y esperar la bandera. Sin el `ret` del
    # disparo, porque despues viene la espera — y el largo del `ret` no es el
    # mismo en las dos arquitecturas, asi que se pide sin el en vez de recortarlo.
    disparo = emitir_escrituras(arch, hh["trigger"], con_ret=False)
    if arch == "x86_64":
        espera = b"\x48\xb8" + flag.to_bytes(8, "little")   # mov rax, flag
        espera += b"\x80\x38\x00"                          # cmp byte [rax], 0
        espera += b"\x74\xfb"                               # je -5
        espera += b"\xc3"                                    # ret
    else:
        espera = mov_reg_imm64(0, flag)
        espera += (0x39400001).to_bytes(4, "little")          # ldrb w1, [x0]
        espera += (0x34FFFFC1).to_bytes(4, "little")          # cbz w1, -8
        espera += (0xD65F03C0).to_bytes(4, "little")          # ret

    ok, c2 = pedir_verbo(75, "mem.claim", {"bytes": 4096, "align": 4096})
    h2 = c2["handle"]
    pedir_verbo(76, "mem.write", {"handle": h2, "bytes": disparo + espera})

    print("  el agente dispara su interrupcion y espera a su propio handler...")
    try:
        ok, r = pedir_verbo(77, "exec", {"handle": h2})
    except TimeoutError:
        print("  FALLA: el exec no volvio — el handler no corrio durante el exec")
        return 1

    if not ok or r.get("faulted"):
        fallas.append(f"el exec fallo: {r}")
    else:
        print("  volvio: el handler corrio mientras el exec seguia")

    ok, d = pedir_verbo(78, "describe", {"what": ["handlers"]})
    despues = -1
    if ok:
        for x in d.get("handlers", []):
            if x["interrupt"] == INT:
                despues = x["served"]
    if despues <= antes:
        fallas.append(f"la cuenta no subio: {antes} -> {despues}")

    print()
    if fallas:
        for f in fallas:
            print(f"  FALLA: {f}")
        return 1
    print("  handler durante exec: ok")
    return 0


def prueba_de_handler(proc, timeout, arch):
    """El agente pone su codigo a atender una interrupcion (D9)."""
    fallas = []

    def pedir_verbo(n, verbo, args):
        resp, _ = pedir(proc, [n, verbo, args], timeout)
        _, ok, carga = resp
        return ok, carga

    # Un numero de interrupcion que la maquina tenga y que no sea del kernel.
    # 34 en ARM es el reloj de tiempo real; 5 en x86 es un cable libre.
    INT = 34 if arch == "aarch64" else 5

    # El handler: un `ret` pelado. Lo unico que se prueba es que el kernel lo
    # llame — lo que haga adentro es asunto del agente (P2).
    ret = (0xD65F03C0).to_bytes(4, "little") if arch == "aarch64" else b"\xc3"

    ok, c = pedir_verbo(50, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar: {c}")
        return 1
    h = c["handle"]
    pedir_verbo(51, "mem.write", {"handle": h, "bytes": ret})

    ok, r = pedir_verbo(52, "irq.install", {"handle": h, "interrupt": INT})
    print(f"  irq.install (interrupcion {INT})  {'ok ' if ok else 'ERROR'} {r}")
    if not ok:
        fallas.append(f"no se pudo instalar: {r}")
        return 1

    # El cable del kernel NO se entrega: seria quedarse sin cordon.
    ok, d = pedir_verbo(53, "describe", {"what": ["interrupts"]})
    cable = d["interrupts"].get("serial")
    if cable and cable.get("gsi"):
        ok, e = pedir_verbo(54, "irq.install", {"handle": h, "interrupt": cable["gsi"]})
        print(f"  y el cable del kernel:            {'ok ' if ok else 'ERROR'} {e}")
        if ok or e.get("error") != "is-kernel-interrupt":
            fallas.append("dejo instalar un handler sobre el cable del kernel")

    # Dos veces la misma tampoco.
    ok, e = pedir_verbo(55, "irq.install", {"handle": h, "interrupt": INT})
    if ok or e.get("error") != "already-installed":
        fallas.append("dejo instalar dos veces la misma interrupcion")

    # Ahora hacerla sonar con codigo del agente, y ver si el kernel la atendio.
    ok, d = pedir_verbo(56, "describe", {"what": ["handlers"]})
    if not ok or not d.get("handlers"):
        fallas.append("el handler no aparece en describe")
        return 1
    hh = d["handlers"][0]
    antes = hh["served"]
    print(f"  atendida {antes} veces hasta ahora")

    if not hh["trigger"]:
        fallas.append("el kernel no publica como hacerla sonar")
        return 1

    codigo = emitir_escrituras(arch, hh["trigger"])
    ok, c2 = pedir_verbo(57, "mem.claim", {"bytes": 4096, "align": 4096})
    h2 = c2["handle"]
    pedir_verbo(58, "mem.write", {"handle": h2, "bytes": codigo})
    print(f"  el agente la hace sonar: {codigo.hex()}")
    ok, r = pedir_verbo(59, "exec", {"handle": h2})
    if not ok or r.get("faulted"):
        fallas.append(f"el codigo que la hace sonar fallo: {r}")

    ok, d = pedir_verbo(60, "describe", {"what": ["handlers"]})
    despues = d["handlers"][0]["served"] if ok and d.get("handlers") else -1
    print(f"  y ahora {despues} veces")
    if despues <= antes:
        fallas.append("el kernel nunca llamo al handler del agente")

    print()
    if fallas:
        for f in fallas:
            print(f"  FALLA: {f}")
        return 1
    print("  handler: ok")
    return 0


def prueba_de_buzon(proc, timeout):
    """Arma el segundo canal en memoria y le manda un pedido por ahi (D17).

    El agente de verdad va a llenar ese buzon desde su driver de red. Aca lo
    llenamos con mem.write, que para el kernel es indistinguible.
    """
    import struct
    fallas = []

    def pedir_verbo(n, verbo, args):
        resp, _ = pedir(proc, [n, verbo, args], timeout)
        _, ok, carga = resp
        return ok, carga

    # El acuerdo lo publica el kernel: no se hornea nada de esto.
    ok, ch = pedir_verbo(30, "describe", {"what": ["channel"]})
    if not ok:
        print("  no se pudo leer el acuerdo del canal")
        return 1
    ch = ch["channel"]
    L = ch["layout"]
    print(f"  el kernel pide magic={ch['magic']:#x} version={ch['version']}")

    CAP = 1024
    ok, c = pedir_verbo(31, "mem.claim", {"bytes": 4096, "align": 4096})
    if not ok:
        print(f"  no se pudo reclamar memoria: {c}")
        return 1
    h = c["handle"]

    # El pedido que va a viajar por el buzon.
    pedido = enc([777, "describe", {"what": ["channel"]}])

    # El encabezado, armado con los offsets que publico el kernel.
    cab = bytearray(L["rings"])
    struct.pack_into("<I", cab, L["magic"], ch["magic"])
    struct.pack_into("<I", cab, L["version"], ch["version"])
    struct.pack_into("<I", cab, L["capacity"], CAP)
    struct.pack_into("<I", cab, L["request_head"], len(pedido))

    ok, _ = pedir_verbo(32, "mem.write", {"handle": h, "off": 0, "bytes": bytes(cab)})
    if not ok:
        fallas.append("no se pudo escribir el encabezado")
    ok, _ = pedir_verbo(33, "mem.write",
                        {"handle": h, "off": L["rings"], "bytes": pedido})
    if not ok:
        fallas.append("no se pudo escribir el pedido en el anillo")

    # Y el kernel lo adopta.
    ok, r = pedir_verbo(34, "listen", {"handle": h})
    print(f"  listen     {'ok ' if ok else 'ERROR'} {r}")
    if not ok:
        fallas.append(f"listen fallo: {r}")
        return 1

    # Un pedido por el cable, para despertar al nucleo. Todavia no hay timbre
    # propio del buzon: eso es lo que sigue.
    pedir_verbo(35, "describe", {})

    # Y ahora la pregunta: ¿contesto por el buzon?
    ok, hdr = pedir_verbo(36, "mem.read", {"handle": h, "off": 0, "len": L["rings"]})
    if not ok:
        fallas.append("no se pudo leer el encabezado de vuelta")
        return 1
    cab = hdr["bytes"]
    res_cabeza = struct.unpack_from("<I", cab, L["response_head"])[0]
    ped_cola = struct.unpack_from("<I", cab, L["request_tail"])[0]

    print(f"  el kernel leyo {ped_cola} de {len(pedido)} bytes del pedido")
    print(f"  y dejo {res_cabeza} bytes de respuesta en el buzon")

    if ped_cola != len(pedido):
        fallas.append(f"no consumio el pedido entero: {ped_cola}/{len(pedido)}")
    if res_cabeza == 0:
        fallas.append("no contesto por el buzon")
    else:
        ok, rd = pedir_verbo(37, "mem.read",
                             {"handle": h, "off": L["rings"] + CAP, "len": res_cabeza})
        if ok:
            try:
                valor, _ = dec(rd["bytes"])
                print(f"  respuesta por el buzon: id={valor[0]} ok={valor[1]}")
                if valor[0] != 777:
                    fallas.append(f"el id no es el del pedido del buzon: {valor[0]}")
            except Exception as e:
                fallas.append(f"la respuesta del buzon no decodifica: {e}")
        else:
            fallas.append("no se pudo leer la respuesta del buzon")

    print()
    if fallas:
        for f in fallas:
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
    ap.add_argument("--smp", type=int, default=1,
                    help="cuantos nucleos darle a QEMU")
    ap.add_argument("--exec", action="store_true", dest="ejecutar",
                    help="sube codigo maquina de verdad y lo corre")
    ap.add_argument("--permiso", action="store_true",
                    help="pide memoria alcanzable sin privilegio y comprueba que el bit este")
    ap.add_argument("--durante", action="store_true",
                    help="el handler del agente corre durante un exec largo")
    ap.add_argument("--handler", action="store_true",
                    help="instala un handler de interrupcion del agente y lo hace sonar")
    ap.add_argument("--timbre", action="store_true",
                    help="el agente toca el timbre del kernel con codigo propio")
    ap.add_argument("--buzon", action="store_true",
                    help="arma el segundo canal y le habla por ahi")
    ap.add_argument("--nucleos", action="store_true",
                    help="reclama los otros nucleos y los arranca")
    ap.add_argument("--memoria", action="store_true",
                    help="prueba el lazo completo: claim, write, read, release")
    args = ap.parse_args()

    guion = os.path.join(RAIZ, "scripts", f"run-{args.arch}.sh")
    # bufsize=0 no es un detalle: con buffer, Python se trae un bloque entero a
    # su buffer interno y despues `select` sobre el descriptor dice "no hay
    # nada" mientras los bytes ya estan leidos. El cliente se cuelga esperando
    # datos que ya tiene.
    cmd = [guion]
    if args.smp > 1:
        # Los scripts le pasan a QEMU cualquier argumento extra.
        cmd += ["-smp", str(args.smp)]
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, cwd=RAIZ, bufsize=0)
    try:
        print(f"arrancando {args.arch} en QEMU...")
        leer_hasta_la_marca(proc, args.timeout, mostrar=True)

        argumentos = {}
        if args.what:
            argumentos["what"] = [s.strip() for s in args.what.split(",")]
        pedido = [1, "describe", argumentos]

        if args.raw:
            print(f"\n  -> {enc(pedido).hex()}")

        respuesta, crudo = pedir(proc, pedido, args.timeout)
        if args.raw:
            print(f"  <- {crudo.hex()}")

        ident, ok, carga = respuesta
        print(f"\nrespuesta id={ident} ok={ok}")
        if not ok:
            print(f"  ERROR: {carga}")
            return 1
        mostrar(carga)

        # El lazo de memoria va en el mismo arranque: cada booteo de QEMU son
        # quince segundos, y el porton hace esto por arquitectura.
        rc = 0
        if args.memoria:
            print()
            rc |= prueba_de_memoria(proc, args.timeout)
        if args.ejecutar:
            print()
            rc |= prueba_de_exec(proc, args.timeout, args.arch)
        if args.nucleos:
            print()
            rc |= prueba_de_nucleos(proc, args.timeout)
        if args.buzon:
            print()
            rc |= prueba_de_buzon(proc, args.timeout)
        if args.timbre:
            print()
            rc |= prueba_de_timbre(proc, args.timeout, args.arch)
        if args.handler:
            print()
            rc |= prueba_de_handler(proc, args.timeout, args.arch)
        if args.durante:
            print()
            rc |= prueba_durante_exec(proc, args.timeout, args.arch)
        if args.permiso:
            print()
            rc |= prueba_de_permiso(proc, args.timeout, args.arch)
        return rc
    finally:
        proc.kill()
        proc.wait()


if __name__ == "__main__":
    sys.exit(main())
