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
        "registro": "rax",
    },
    "aarch64": {
        # movz x0, #0xFFEE ; movk x0, #0xC0, lsl #16 ; ret
        "ok": bytes([0xC0, 0xFD, 0x9F, 0xD2, 0x00, 0x18, 0xA0, 0xF2,
                     0xC0, 0x03, 0x5F, 0xD6]),
        # movz x9, #0x4000, lsl #32 ; str x9, [x9] ; ret
        "falla": bytes([0x09, 0x00, 0xC8, 0xD2, 0x29, 0x01, 0x00, 0xF9,
                        0xC0, 0x03, 0x5F, 0xD6]),
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


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--arch", default="x86_64", choices=["x86_64", "aarch64"])
    ap.add_argument("--what", help="secciones separadas por coma; sin esto pide el indice")
    ap.add_argument("--raw", action="store_true", help="mostrar los bytes que viajan")
    ap.add_argument("--timeout", type=float, default=90.0)
    ap.add_argument("--exec", action="store_true", dest="ejecutar",
                    help="sube codigo maquina de verdad y lo corre")
    ap.add_argument("--memoria", action="store_true",
                    help="prueba el lazo completo: claim, write, read, release")
    args = ap.parse_args()

    guion = os.path.join(RAIZ, "scripts", f"run-{args.arch}.sh")
    # bufsize=0 no es un detalle: con buffer, Python se trae un bloque entero a
    # su buffer interno y despues `select` sobre el descriptor dice "no hay
    # nada" mientras los bytes ya estan leidos. El cliente se cuelga esperando
    # datos que ya tiene.
    proc = subprocess.Popen([guion], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
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
        return rc
    finally:
        proc.kill()
        proc.wait()


if __name__ == "__main__":
    sys.exit(main())
