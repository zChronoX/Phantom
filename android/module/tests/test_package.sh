#!/usr/bin/env bash
# Controlla il pacchetto FINALE (quello che si installa sul telefono), non i sorgenti:
# contiene tutti i file, gli script sono eseguibili e senza errori di sintassi, la sonda è un binario arm64.
set -u
ZIP="${1:-$(cd "$(dirname "$0")/../../.." && pwd)/dist/phantom-f0.zip}"
FAIL=0
ok()  { echo "  ok   $1"; }
bad() { echo "  FAIL $1"; FAIL=$((FAIL + 1)); }
[ -f "$ZIP" ] || { echo "manca $ZIP"; exit 1; }
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
unzip -q "$ZIP" -d "$T"

for f in module.prop customize.sh service.sh action.sh uninstall.sh webroot/index.html META-INF/com/google/android/update-binary \
         bin/phantom-probe bin/phantomctl bin/phantom-report.sh bin/phantom-postmortem.sh; do
    [ -f "$T/$f" ] && ok "contiene $f" || bad "manca $f"
done
for f in service.sh action.sh customize.sh uninstall.sh bin/phantomctl bin/phantom-report.sh bin/phantom-postmortem.sh bin/phantom-probe; do
    [ -x "$T/$f" ] && ok "$f è eseguibile" || bad "$f non è eseguibile"
done
for f in service.sh action.sh customize.sh uninstall.sh bin/phantomctl bin/phantom-report.sh bin/phantom-postmortem.sh; do
    sh -n "$T/$f" 2>/dev/null && ok "$f: sintassi corretta" || bad "$f: errore di sintassi"
done
file "$T/bin/phantom-probe" | grep -q "ARM aarch64" && ok "la sonda è un binario arm64" || bad "la sonda non è arm64"
file "$T/bin/phantom-probe" | grep -q "statically linked" && ok "la sonda è statica" || bad "la sonda non è statica"
# Ogni script richiamato da un altro deve esistere nel pacchetto.
for ref in $(grep -ho 'bin/phantom[a-z-]*\.sh' "$T"/*.sh "$T"/bin/* 2>/dev/null | sort -u); do
    [ -f "$T/$ref" ] && ok "$ref (richiamato) è presente" || bad "$ref è richiamato ma manca"
done
ver=$(grep '^version=' "$T/module.prop" | cut -d= -f2)
echo "  versione del modulo: $ver"
if [ -f "$T/bin/scrcpy-server" ]; then
    unzip -l "$T/bin/scrcpy-server" | grep -q classes.dex && ok "scrcpy-server è un jar valido" || bad "scrcpy-server non è un jar valido"
    [ -s "$T/bin/scrcpy-server.version" ] && ok "versione di scrcpy-server: $(cat "$T/bin/scrcpy-server.version")" || bad "manca scrcpy-server.version"
else
    echo "  (scrcpy-server non incluso in questa build locale)"
fi
[ $FAIL -eq 0 ] && { echo; echo "Pacchetto valido"; } || { echo; echo "$FAIL controlli falliti"; exit 1; }
