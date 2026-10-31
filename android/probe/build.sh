#!/usr/bin/env bash
# Compila phantom-probe: binario statico arm64 per Android e uno x86_64 per i test locali.
# Richiede zig (pip install ziglang) oppure ZIG=/percorso/zig.
set -euo pipefail
cd "$(dirname "$0")"
ZIG="${ZIG:-python3 -m ziglang}"
OUT="${OUT:-build}"
mkdir -p "$OUT"

python3 ../../tools/embed_html.py webpage.html webpage.h PAGE_HTML

CFLAGS=(-std=c11 -O2 -Wall -Wextra -Werror -Wno-unused-parameter -static -s)
$ZIG cc -target aarch64-linux-musl "${CFLAGS[@]}" -o "$OUT/phantom-probe-arm64" phantom-probe.c
$ZIG cc -target x86_64-linux-musl "${CFLAGS[@]}" -o "$OUT/phantom-probe-x86_64" phantom-probe.c
ls -l "$OUT"
