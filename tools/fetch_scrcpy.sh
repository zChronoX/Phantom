#!/usr/bin/env bash
# Scarica il server di scrcpy (https://github.com/Genymobile/scrcpy, licenza Apache 2.0) dalla
# release ufficiale e lo mette nel modulo. Usa la versione indicata in SCRCPY_VERSION
# (es. 3.3.4) oppure l'ultima release. Stampa versione e SHA-256 per poterla fissare.
set -euo pipefail
cd "$(dirname "$0")/.."
DEST=android/module/bin
VER="${SCRCPY_VERSION:-}"
if [ -z "$VER" ]; then
  VER=$(gh api repos/Genymobile/scrcpy/releases/latest --jq .tag_name)
  VER="${VER#v}"
fi
URL="https://github.com/Genymobile/scrcpy/releases/download/v${VER}/scrcpy-server-v${VER}"
echo "scarico $URL"
curl -fsSL --retry 3 -o "$DEST/scrcpy-server" "$URL"
echo "$VER" > "$DEST/scrcpy-server.version"
SIZE=$(wc -c < "$DEST/scrcpy-server")
[ "$SIZE" -gt 10000 ] || { echo "file troppo piccolo ($SIZE byte)"; exit 1; }
unzip -l "$DEST/scrcpy-server" | grep -q "classes.dex" || { echo "il file non è un jar valido"; exit 1; }
echo "scrcpy-server v$VER · $SIZE byte · sha256 $(sha256sum "$DEST/scrcpy-server" | cut -d' ' -f1)"
