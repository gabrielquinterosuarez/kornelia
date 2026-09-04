#!/usr/bin/env python3
"""Las citas del libro al codigo tienen que seguir apuntando a algo.

El libro cita el kernel constantemente y el kernel se mueve. Una cita vieja no
se ve rota: se lee igual de bien y dice algo falso, que es peor que un enlace
muerto. Este chequeo existe por la misma razon que `check-boundary.sh` y
`check-language.py`: una regla que no se comprueba es una intencion (D23).

La forma de una cita, escrita como codigo en linea dentro del markdown:

    `kernel-core/src/platform.rs:69#pub trait Platform`
     ^ruta relativa a la raiz     ^linea ^ancla

El numero de linea es informativo. **El ancla es lo que manda**: un texto que
tiene que seguir apareciendo en esa linea. Cuando el codigo se corre, el numero
queda viejo pero el ancla sigue encontrando el lugar, y `--fix` reescribe el
numero. Cuando el ancla desaparece, la cita esta rota de verdad y hay que
mirarla a mano: puede que lo que el libro explica ya no exista.

Una cita sin ancla se avisa pero no falla: solo se puede comprobar que el
archivo exista y que tenga esa linea. Conviene ponerle ancla a todas.

    ./libro/scripts/check-citas.py           # informa
    ./libro/scripts/check-citas.py --fix     # corrige los numeros corridos
    ./libro/scripts/check-citas.py --strict  # y falla tambien por las citas sin ancla
"""

import argparse
import re
import sys
from pathlib import Path

VAULT = Path(__file__).resolve().parent.parent
ROOT = VAULT.parent

# Las plantillas citan a proposito rutas que no existen (`crate/src/archivo.rs`):
# son el molde, no una cita.
SKIP = {"plantillas"}

# Extensiones que se citan. La lista es cerrada para que un `foo.rs:12` dentro
# de una frase en prosa no se confunda con una cita.
SUFFIXES = "rs|py|sh|toml|md|txt|json|yml"

CITATION = re.compile(
    r"`(?P<path>[A-Za-z0-9_./-]+\.(?:" + SUFFIXES + r")):(?P<line>\d+)(?:#(?P<anchor>[^`\n]+))?`"
)

OK, NO_ANCHOR, MOVED, BROKEN, NO_FILE, NO_LINE = range(6)


def markdown_files():
    for path in sorted(VAULT.rglob("*.md")):
        if any(part in SKIP for part in path.relative_to(VAULT).parts):
            continue
        yield path


def check(citation, cache):
    """Devuelve (estado, linea_correcta, detalle) para una cita."""
    target = ROOT / citation["path"]
    if not target.is_file():
        return NO_FILE, None, "el archivo no existe"

    if citation["path"] not in cache:
        cache[citation["path"]] = target.read_text(errors="replace").splitlines()
    lines = cache[citation["path"]]

    cited = int(citation["line"])
    if cited < 1 or cited > len(lines):
        return NO_LINE, None, f"el archivo tiene {len(lines)} lineas"

    anchor = citation["anchor"]
    if anchor is None:
        return NO_ANCHOR, cited, "sin ancla: no se puede comprobar que siga diciendo lo mismo"

    if anchor in lines[cited - 1]:
        return OK, cited, ""

    # El ancla no esta donde decia la cita. Si aparece en otro lado, el codigo
    # se corrio y el numero se puede arreglar. Con varias coincidencias se
    # elige la mas cercana a la que decia la cita, que es casi siempre la
    # misma linea que se movio unas pocas.
    found = [i + 1 for i, text in enumerate(lines) if anchor in text]
    if not found:
        return BROKEN, None, "el ancla ya no esta en el archivo"
    best = min(found, key=lambda n: abs(n - cited))
    extra = f" ({len(found)} coincidencias)" if len(found) > 1 else ""
    return MOVED, best, f"se corrio de {cited} a {best}{extra}"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--fix", action="store_true", help="reescribir los numeros de linea corridos")
    parser.add_argument("--strict", action="store_true", help="fallar tambien por las citas sin ancla")
    args = parser.parse_args()

    cache = {}
    counts = {state: 0 for state in range(6)}
    problems = []

    for path in markdown_files():
        text = path.read_text()
        rewritten = text
        for match in CITATION.finditer(text):
            citation = match.groupdict()
            state, correct, detail = check(citation, cache)
            counts[state] += 1
            where = f"{path.relative_to(ROOT)}"

            if state == OK:
                continue

            problems.append((state, where, match.group(0), detail))

            if state == MOVED and args.fix:
                anchor = citation["anchor"]
                old = match.group(0)
                new = f"`{citation['path']}:{correct}#{anchor}`"
                rewritten = rewritten.replace(old, new)

        if args.fix and rewritten != text:
            path.write_text(rewritten)

    label = {
        NO_ANCHOR: "SIN ANCLA",
        MOVED: "CORRIDA  ",
        BROKEN: "ROTA     ",
        NO_FILE: "NO EXISTE",
        NO_LINE: "FUERA     ",
    }
    for state, where, citation, detail in problems:
        print(f"{label[state]}  {where}\n            {citation}\n            {detail}")

    total = sum(counts.values())
    print(
        f"\n{total} citas: {counts[OK]} bien, {counts[MOVED]} corridas, "
        f"{counts[BROKEN] + counts[NO_FILE] + counts[NO_LINE]} rotas, "
        f"{counts[NO_ANCHOR]} sin ancla."
    )

    if args.fix and counts[MOVED]:
        print(f"Se corrigieron {counts[MOVED]} numeros de linea.")
        return 0

    bad = counts[MOVED] + counts[BROKEN] + counts[NO_FILE] + counts[NO_LINE]
    if args.strict:
        bad += counts[NO_ANCHOR]
    if bad:
        print("\nCorre con --fix si solo son numeros corridos.")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
