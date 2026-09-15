#!/usr/bin/env python3
"""Saca de una fuente PCF los glifos ASCII y los deja como codigo Rust.

Genera `kernel-core/src/font.rs`, que es lo que el kernel usa para escribir en
la pantalla cuando la maquina no tiene puerto serie.

**Se corre a mano, no en cada compilacion.** La salida esta commiteada a
proposito: si el build dependiera de que exista una fuente instalada en el
sistema, el kernel no compilaria en una maquina limpia — y eso es exactamente la
clase de dependencia escondida que el proyecto evita.

    ./scripts/make-font.py /usr/share/fonts/X11/misc/8x16.pcf.gz

La fuente por omision es la `misc-fixed` 8x16 de X11, cuya licencia entera dice:

    "Public domain font.  Share and enjoy."

O sea que se puede embeber en un proyecto MIT/Apache sin condiciones. Queda
anotado igual en el archivo generado, porque de donde salio un dato es parte del
dato.
"""
import gzip
import struct
import sys

# Los tipos de tabla que hacen falta. Una PCF trae varias mas —aceleradores,
# propiedades, anchos— que para esto no aportan nada.
PCF_METRICS = 0x04
PCF_BITMAPS = 0x08
PCF_BDF_ENCODINGS = 0x20
# Si esta puesto, cada metrica entra en cinco bytes en vez de seis enteros de 16.
PCF_COMPRESSED_METRICS = 0x100

# El rango que se embebe: de espacio a tilde. Es todo lo que el kernel escribe,
# porque lo que sale por el cable es ASCII puro (regla 4).
FIRST = 0x20
LAST = 0x7E


def ints(data, off, count, msb):
    """`count` enteros de 32 bits, en el orden que diga la tabla."""
    order = ">" if msb else "<"
    return struct.unpack_from(f"{order}{count}i", data, off)


def shorts(data, off, count, msb):
    order = ">" if msb else "<"
    return struct.unpack_from(f"{order}{count}h", data, off)


def tables(data):
    """El indice del archivo: que tablas hay y donde empieza cada una."""
    if data[:4] != b"\x01fcp":
        raise SystemExit("no parece un archivo PCF")
    count, = struct.unpack_from("<i", data, 4)
    found = {}
    for i in range(count):
        kind, fmt, size, off = struct.unpack_from("<4i", data, 8 + i * 16)
        found[kind] = (fmt, size, off)
    return found


def glyphs(data):
    """Devuelve (codigo -> filas de bits) y cuanto mide cada glifo."""
    index = tables(data)
    for needed, name in ((PCF_BITMAPS, "bitmaps"), (PCF_BDF_ENCODINGS, "codigos"),
                         (PCF_METRICS, "medidas")):
        if needed not in index:
            raise SystemExit(f"a la fuente le falta la tabla de {name}")

    # --- Cuanto mide un caracter -------------------------------------------
    #
    # El ancho hay que leerlo, no deducirlo del relleno: el relleno es a cuantos
    # bytes se redondea cada fila, y en esta fuente son cuatro para glifos de
    # ocho pixeles. Confundirlos da un ancho de 32.
    _, _, off = index[PCF_METRICS]
    fmt, = struct.unpack_from("<i", data, off)
    msb = (fmt >> 2) & 1
    order = ">" if msb else "<"
    if fmt & PCF_COMPRESSED_METRICS:
        # Comprimidas: cinco bytes por glifo, cada uno corrido en 0x80.
        count, = struct.unpack_from(f"{order}h", data, off + 4)
        first = struct.unpack_from("5B", data, off + 6)
        cell_width = first[2] - 0x80
        ascent, descent = first[3] - 0x80, first[4] - 0x80
    else:
        count, = struct.unpack_from(f"{order}i", data, off + 4)
        first = struct.unpack_from(f"{order}6h", data, off + 8)
        cell_width = first[2]
        ascent, descent = first[3], first[4]
    cell_height = ascent + descent

    # --- Los bitmaps -------------------------------------------------------
    _, _, off = index[PCF_BITMAPS]
    fmt, = struct.unpack_from("<i", data, off)
    # El bit 2 del formato dice si los enteros de ESTA tabla van al reves.
    msb = (fmt >> 2) & 1
    # Y los dos de abajo, a cuantos bytes se redondea cada fila.
    padding = 1 << (fmt & 3)

    total, = ints(data, off + 4, 1, msb)
    offsets = ints(data, off + 8, total, msb)
    sizes = ints(data, off + 8 + total * 4, 4, msb)
    bitmaps_at = off + 8 + total * 4 + 16
    bitmaps = data[bitmaps_at:bitmaps_at + sizes[fmt & 3]]

    # --- Que codigo corresponde a que glifo --------------------------------
    _, _, off = index[PCF_BDF_ENCODINGS]
    fmt, = struct.unpack_from("<i", data, off)
    msb = (fmt >> 2) & 1
    lo, hi, byte1_lo, byte1_hi, _default = shorts(data, off + 4, 5, msb)
    span = (hi - lo + 1) * (byte1_hi - byte1_lo + 1)
    mapping = shorts(data, off + 14, span, msb)

    # Cuanto ocupa un glifo: la diferencia entre dos arranques consecutivos. Se
    # deduce en vez de suponerse, que es lo mismo que hace el kernel con la
    # maquina (P4).
    per_glyph = sorted(set(b - a for a, b in zip(offsets, offsets[1:]) if b > a))
    if len(per_glyph) != 1:
        raise SystemExit(f"los glifos no miden todos igual: {per_glyph[:5]}")
    per_glyph = per_glyph[0]
    height = cell_height
    width = cell_width
    if per_glyph != height * padding:
        raise SystemExit(
            f"un glifo ocupa {per_glyph} bytes pero {height} filas de {padding}"
            f" darian {height * padding}")
    # El bit mas alto de cada byte es el pixel de la izquierda, salvo que la
    # tabla diga lo contrario.
    lsb_first = not ((fmt >> 3) & 1)

    out = {}
    for code in range(FIRST, LAST + 1):
        if not (lo <= code <= hi):
            raise SystemExit(f"la fuente no trae el codigo {code:#x}")
        glyph = mapping[code - lo]
        if glyph == 0xFFFF or glyph < 0:
            raise SystemExit(f"la fuente no define el codigo {code:#x}")
        at = offsets[glyph]
        rows = []
        for row in range(height):
            # Solo el primer byte de cada fila: la fuente es de 8 de ancho, asi
            # que el resto del relleno es cero.
            b = bitmaps[at + row * padding]
            if lsb_first:
                # Al reves: el pixel de la izquierda es el bit de mas abajo.
                b = int(f"{b:08b}"[::-1], 2)
            rows.append(b)
        out[code] = rows
    return out, width, height


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "/usr/share/fonts/X11/misc/8x16.pcf.gz"
    opener = gzip.open if path.endswith(".gz") else open
    with opener(path, "rb") as f:
        data = f.read()

    table, width, height = glyphs(data)
    if width != 8:
        raise SystemExit(f"esto espera una fuente de 8 de ancho, y esa mide {width}")

    lines = [
        "//! La tipografia con la que el kernel escribe en pantalla.",
        "//!",
        "//! **Generado por `./scripts/make-font.py`. No se edita a mano.**",
        "//!",
        f"//! Sale de la `misc-fixed` {width}x{height} de X11, cuya licencia entera dice:",
        '//! *\"Public domain font.  Share and enjoy.\"* — o sea que se puede embeber',
        "//! sin condiciones, que es justo lo que hace falta en un kernel que no",
        "//! carga archivos.",
        "//!",
        "//! Solo van los codigos imprimibles de ASCII, porque es lo unico que el",
        "//! kernel escribe: lo que sale por el cable va en ASCII puro (regla 4), y",
        "//! la pantalla dice exactamente lo mismo que el cable.",
        "",
        "/// Cuantos pixeles de ancho tiene un caracter.",
        f"pub const WIDTH: usize = {width};",
        "/// Y cuantos de alto.",
        f"pub const HEIGHT: usize = {height};",
        "/// El primer codigo que la tabla trae.",
        f"pub const FIRST: u8 = {FIRST:#04x};",
        "/// Y el ultimo.",
        f"pub const LAST: u8 = {LAST:#04x};",
        "",
        "/// Un byte por fila, el bit mas alto a la izquierda.",
        f"pub static GLYPHS: [[u8; HEIGHT]; {LAST - FIRST + 1}] = [",
    ]
    for code in range(FIRST, LAST + 1):
        shown = chr(code).replace("\\", "\\\\")
        body = ", ".join(f"0x{b:02x}" for b in table[code])
        lines.append(f"    [{body}], // {shown!r}")
    lines.append("];")

    with open("kernel-core/src/font.rs", "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    print(f"kernel-core/src/font.rs: {LAST - FIRST + 1} glifos de {width}x{height}")

    # Una muestra, para poder ver con los ojos que no salio cualquier cosa.
    for code in (ord("A"), ord("k")):
        print(f"\n  {chr(code)!r}:")
        for row in table[code]:
            print("    " + "".join("#" if row & (0x80 >> i) else "." for i in range(8)))


if __name__ == "__main__":
    main()
