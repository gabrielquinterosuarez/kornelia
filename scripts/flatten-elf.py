#!/usr/bin/env python3
"""Saca de un ELF los bytes que hay que cargar, y nada mas.

Es lo que convierte lo que compila `blob/` en el `blob.bin` que el kernel copia
a memoria y ejecuta desde el byte cero (D19).

Se hace aca y no con `objcopy` por una razon boba y concreta: el `objcopy` que
viene con la maquina de desarrollo entiende **su** arquitectura, asi que no
puede leer el ELF de la otra. Leer el encabezado a mano son treinta lineas y
anda para las dos por igual.

Se recorren los segmentos de carga y no las secciones a proposito: un segmento
es justamente "esto va a memoria tal cual", que es la pregunta que hay que
contestar. Y se comprueba que empiecen en cero y sean contiguos, porque el
kernel no relocaliza nada — copia y salta.
"""
import struct
import sys

PT_LOAD = 1
# Los dos que puede haber, para poder decir cual vino si no es el que se espera.
MACHINES = {0x3E: "x86_64", 0xB7: "aarch64"}


def flatten(path):
    with open(path, "rb") as f:
        data = f.read()

    if data[:4] != b"\x7fELF":
        raise SystemExit(f"{path}: no es un ELF")
    if data[4] != 2:
        raise SystemExit(f"{path}: se esperaba un ELF de 64 bits")

    machine, = struct.unpack_from("<H", data, 0x12)
    entry, = struct.unpack_from("<Q", data, 0x18)
    phoff, = struct.unpack_from("<Q", data, 0x20)
    phentsize, phnum = struct.unpack_from("<HH", data, 0x36)

    # El kernel salta al byte cero de lo que carga, asi que la entrada tiene que
    # ser justo el principio. Si no lo es, el enlazador puso otra cosa adelante y
    # el blob saltaria al medio de una funcion — que no falla, hace cualquier
    # cosa.
    if entry != 0:
        raise SystemExit(
            f"{path}: la entrada esta en {entry:#x} y tiene que estar en 0.\n"
            f"  Lo pone `blob.ld`: la seccion .text.entry va primero.")

    image = bytearray()
    for i in range(phnum):
        off = phoff + i * phentsize
        p_type, = struct.unpack_from("<I", data, off)
        if p_type != PT_LOAD:
            continue
        p_offset, p_vaddr = struct.unpack_from("<QQ", data, off + 0x08)
        p_filesz, p_memsz = struct.unpack_from("<QQ", data, off + 0x20)

        # Un segmento puede empezar mas adelante que el anterior: entre el codigo
        # y los datos suele haber un hueco de alineacion. Ese hueco tiene que
        # **existir** en memoria, porque las direcciones que el compilador armo
        # lo dan por hecho, asi que se rellena con ceros en vez de cerrarlo.
        if p_vaddr < len(image):
            raise SystemExit(
                f"{path}: el segmento {i} quiere ir a {p_vaddr:#x}, que ya esta"
                f" ocupado (la imagen mide {len(image):#x}). Se pisarian.")
        image += bytes(p_vaddr - len(image))
        image += data[p_offset:p_offset + p_filesz]
        # Un segmento que ocupa mas en memoria que en el archivo es `.bss`, y en
        # un binario plano eso es basura: nadie escribe esos ceros.
        if p_memsz != p_filesz:
            raise SystemExit(
                f"{path}: el segmento {i} tiene {p_memsz - p_filesz} bytes sin"
                f" respaldo en el archivo (.bss). Sacá el `static` mutable.")

    if not image:
        raise SystemExit(f"{path}: no hay nada que cargar")
    return bytes(image), MACHINES.get(machine, f"maquina {machine:#x}")


def main():
    if len(sys.argv) != 3:
        raise SystemExit("uso: flatten-elf.py <elf> <salida.bin>")
    image, arch = flatten(sys.argv[1])
    with open(sys.argv[2], "wb") as f:
        f.write(image)


if __name__ == "__main__":
    main()
