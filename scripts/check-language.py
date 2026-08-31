#!/usr/bin/env python3
"""El idioma del codigo (regla 6): los identificadores van en ingles.

Los comentarios, la documentacion y los textos que salen por el UART van en
espanol, y este chequeo no los mira. Lo que mira son las **posiciones de
codigo**: nombres de tipos, campos, funciones, constantes, variables,
etiquetas de ensamblador y nombres de archivo.

Existe porque la regla ya se rompio dos veces estando escrita en CLAUDE.md.
Una regla que no se comprueba es una intencion, no una regla — la misma razon
por la que la frontera de portabilidad tiene su propio chequeo (D23).

Es una lista de palabras, asi que no es completa por construccion: si una
palabra en espanol se cuela sin estar en la lista, se agrega abajo y deja de
poder volver. Lo que si garantiza es que lo ya corregido no se repita.
"""
import re
import sys
from pathlib import Path

# Palabras en espanol que NO son tambien palabras en ingles. Las que se
# escriben igual en los dos idiomas (base, serial, cable, error, total, final,
# normal, real, no, id) quedan afuera a proposito: marcarlas seria ruido.
FORBIDDEN = set("""
abajo abrir acepta actualizar acuerdo adelante adentro afuera agente agregar
cadena cadenas carga humano incompleta incompleto linea listo mayor mensaje
patron pedazo pedazos texto trozo unidad volver
ahora algo alguien alineacion alineado alto ancho anillo anotar antes anterior
apagar aparato aparatos apilar aqui arrancar arreglar arriba asi atender
atras aunque avisar bajo bandera banderas bloque bloques borrar buscar buzon
cabeza cada calcular cambiar campana campo campos candado candidato cargar
causa cerca cerrar chico cima clave claves codigo cola colgar comprobar
contador contar copiar corto crear crudo cual cuales cuando cuantas cuantos
cuerpo datos deber decir dejar demorar dentro desde despertar despues destino
destruir devolver direccion dispositivo donde dormir encabezado encender
encontrar entero enteros entonces entrada entradas entrega enviar escribe
escribir espera esperar espuria estado fallar falla fallas fallo firma firmas
fija fijas fin flujo formato frontera fuga fugas fuera
funcion grande guardado guardar
hacer hasta hueco huecos imprimir indice indices inicio instalar interrupcion
largo lee leer lejos limite llamar llegada llegado llenar luego maquina marca
marco medir mejorar memoria mia mismo mostrar mover muchos muy nada nadie
necesario nivel niveles nombre nombres nuestra nuestro nucleo nucleos nuevo
numero nunca ocupado origen otro pagina paginas parar partidos paso pedido
pedir pegar pensar perdidos permiso pila pilas poner porque prender primera
primero probar procesar prueba pruebas puesto punteros punto quitar raiz
ranura ranuras recibir reclamo reclamos recuperacion registro registros
respuesta responder revisar romper sacar salida salidas saltar saludar seguir
serie siempre siguiente subir sumar tabla tablas tamano terminar timbre tipo
tipos todavia todas todos tope trampolin ultima ultimo usadas usados usar
vacia vacio valor valores verbo verificar vistos volver vueltas
""".split())


def words(name):
    """Parte un identificador en palabras: por `_` y por mayuscula."""
    for chunk in name.split('_'):
        for w in re.findall(r'[A-Z]+(?![a-z])|[A-Z][a-z]*|[a-z]+|[0-9]+', chunk):
            yield w.lower()


def rust_code(src):
    """Los tramos de codigo de un `.rs`, con su numero de linea.

    Los raw strings llevan ensamblador adentro, asi que **son** codigo; los
    comentarios `//` de ese ensamblador, no.
    """
    out = []
    i, n, line, start = 0, len(src), 1, 0
    while i < n:
        c = src[i]
        if c == '\n':
            line += 1
        if c == '/' and i + 1 < n and src[i + 1] == '/':
            out.append((line, src[start:i]))
            while i < n and src[i] != '\n':
                i += 1
            start = i
            continue
        if c == '/' and i + 1 < n and src[i + 1] == '*':
            out.append((line, src[start:i]))
            depth, i = 1, i + 2
            while i < n and depth:
                if src[i] == '\n':
                    line += 1
                if src.startswith('/*', i):
                    depth, i = depth + 1, i + 2
                elif src.startswith('*/', i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            start = i
            continue
        m = re.match(r'r(#*)"', src[i:])
        if m:
            out.append((line, src[start:i]))
            close = '"' + m.group(1)
            end = src.find(close, i + m.end())
            end = n if end < 0 else end
            first = line + src.count('\n', i, i + m.end())
            for k, piece in enumerate(src[i + m.end():end].split('\n')):
                out.append((first + k, re.sub(r'//.*', '', piece)))
            line += src.count('\n', i, end + len(close))
            i = start = end + len(close)
            continue
        if c == '"':
            out.append((line, src[start:i]))
            i += 1
            while i < n:
                if src[i] == '\\':
                    i += 2
                    continue
                if src[i] == '\n':
                    line += 1
                if src[i] == '"':
                    i += 1
                    break
                i += 1
            start = i
            continue
        i += 1
    out.append((line, src[start:]))
    return out


def shell_code(src):
    """En shell solo son codigo los nombres: asignaciones, `$var` y funciones.

    Los textos van entre comillas y estan en espanol a proposito, pero adentro
    de ellos puede haber `$variables`, que si son codigo.
    """
    out = []
    for i, line in enumerate(src.split('\n'), 1):
        line = re.sub(r'(^|\s)#.*', r'\1', line)
        found = re.findall(r'^\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?:\(\)|=)', line)
        found += re.findall(r'\$\{?([A-Za-z_][A-Za-z0-9_]*)', line)
        found += re.findall(r'\b(?:local|for)\s+([A-Za-z_][A-Za-z0-9_]*)', line)
        if found:
            out.append((i, ' '.join(found)))
    return out


def python_code(src):
    """Codigo de Python: sin comentarios y sin textos, incluidos los de tres
    comillas, que es donde vive la lista de arriba."""
    src = re.sub(r'"""[\s\S]*?"""|\'\'\'[\s\S]*?\'\'\'',
                 lambda m: '\n' * m.group(0).count('\n'), src)
    out = []
    for i, line in enumerate(src.split('\n'), 1):
        # Los textos primero: un `#` adentro de uno —`{x:#018x}` es de lo mas
        # comun— no abre un comentario, y sacarlo antes cortaria la cadena al
        # medio y dejaria su contenido a la vista como si fuera codigo.
        line = re.sub(r'\'[^\']*\'|"[^"]*"', ' ', line)
        line = re.sub(r'#.*', '', line)
        out.append((i, line))
    return out


NAME = re.compile(r'[A-Za-z_][A-Za-z0-9_]*')
READERS = {'.rs': rust_code, '.sh': shell_code, '.py': python_code}


def check(path):
    reader = READERS.get(path.suffix)
    if reader is None:
        return []
    bad = []
    for line, text in reader(path.read_text()):
        for name in NAME.findall(text):
            for word in words(name):
                if word in FORBIDDEN:
                    bad.append((line, name, word))
                    break
    return bad


def main():
    root = Path(__file__).resolve().parent.parent
    files = sorted(
        p for p in root.rglob('*')
        if p.suffix in READERS and 'target' not in p.parts
        and not any(part.startswith('.') for part in p.parts)
    )

    total = 0
    for p in files:
        rel = p.relative_to(root)
        for line, name, word in check(p):
            print(f"{rel}:{line}: `{name}` lleva `{word}`, que es espanol")
            total += 1
        # El nombre del archivo tambien es codigo.
        for word in words(p.stem.replace('-', '_')):
            if word in FORBIDDEN:
                print(f"{rel}: el nombre del archivo lleva `{word}`, "
                      f"que es espanol")
                total += 1

    if total:
        print(f"\n{total} identificadores en espanol. La regla 6: el codigo va "
              f"en ingles;\nlos comentarios y los textos del UART siguen en "
              f"espanol.")
        return 1
    print("idioma: todo el codigo en ingles")
    return 0


if __name__ == '__main__':
    sys.exit(main())
