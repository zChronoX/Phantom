#!/usr/bin/env bash
# Compila ed esegue i test del parser scrcpy. Serve swiftc (macOS con Xcode, oppure Swift su Linux).
set -euo pipefail
cd "$(dirname "$0")"
OUT="${TMPDIR:-/tmp}/phantom-ios-tests"
mkdir -p "$OUT"
# main.swift è il punto d'ingresso: i test hanno codice a livello di file.
cp ScrcpyStreamTests.swift "$OUT/main.swift"
swiftc -O -o "$OUT/run" ../PhantomF0/ScrcpyStream.swift "$OUT/main.swift"
"$OUT/run"
