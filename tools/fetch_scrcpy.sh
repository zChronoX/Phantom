#!/usr/bin/env bash
# Scarica il server di scrcpy (https://github.com/Genymobile/scrcpy, licenza Apache 2.0) dalla
# release ufficiale e lo mette nel modulo.
#
# La versione è FISSATA (PINNED_VERSION) e il file viene controllato con la sua impronta SHA-256: una release nuova
# di scrcpy non deve cambiare in silenzio il comportamento che stiamo diagnosticando sul POCO. Per provare un'altra
# versione: SCRCPY_VERSION=5.1.0 tools/fetch_scrcpy.sh (senza controllo dell'impronta, che viene stampata: se la
# versione va bene, si aggiornano PINNED_VERSION e PINNED_SHA256 qui sotto). SCRCPY_VERSION=latest scarica l'ultima.
set -euo pipefail
cd "$(dirname "$0")/.."
DEST=android/module/bin

PINNED_VERSION="5.0.1"
PINNED_SHA256="764eb6f79811d5211fe9df341120882ba9994c7a61b897d7bf3fb662e53bc536"

VER="${SCRCPY_VERSION:-$PINNED_VERSION}"
if [ "$VER" = "latest" ]; then
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
if [ "$VER" = "$PINNED_VERSION" ]; then
  ACTUAL=$(sha256sum "$DEST/scrcpy-server" | cut -d' ' -f1)
  [ "$ACTUAL" = "$PINNED_SHA256" ] || { echo "IMPRONTA SBAGLIATA: attesa $PINNED_SHA256, trovata $ACTUAL"; rm -f "$DEST/scrcpy-server"; exit 1; }
  echo "impronta verificata"
else
  echo "ATTENZIONE: versione diversa da quella fissata ($PINNED_VERSION): impronta non verificata"
fi
