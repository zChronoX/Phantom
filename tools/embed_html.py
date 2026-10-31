#!/usr/bin/env python3
"""Converte un file (es. la pagina web della sonda) in un header C con una stringa costante.

Uso: embed_html.py <input> <output.h> <NOME_SIMBOLO>
"""
import sys


def main() -> None:
    src, dst, name = sys.argv[1:4]
    data = open(src, "rb").read()
    lines = []
    for i in range(0, len(data), 80):
        chunk = data[i : i + 80]
        esc = "".join(
            c if 32 <= ord(c) < 127 and c not in '"\\?' else f"\\{ord(c):03o}"
            for c in chunk.decode("latin-1")
        )
        lines.append(f'    "{esc}"')
    body = "\n".join(lines)
    with open(dst, "w") as f:
        f.write(f"/* Generato da tools/embed_html.py a partire da {src}: non modificare a mano. */\n")
        f.write(f"static const char {name}[] =\n{body};\n")


if __name__ == "__main__":
    main()
