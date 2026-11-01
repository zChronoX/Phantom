#!/usr/bin/env bash
# Crea phantom-f0.zip installabile dal manager SukiSU/KernelSU/Magisk.
set -euo pipefail
cd "$(dirname "$0")"
../probe/build.sh >/dev/null
OUT="${OUT:-../../dist}"
mkdir -p "$OUT" stage
rm -rf stage/* "$OUT/phantom-f0.zip"
cp -r META-INF webroot module.prop customize.sh service.sh action.sh uninstall.sh stage/
mkdir -p stage/bin
cp bin/phantomctl bin/phantom-report.sh bin/phantom-postmortem.sh bin/phantom-deep.sh bin/phantom-captest.sh stage/bin/
cp ../probe/build/phantom-probe-arm64 stage/bin/phantom-probe
# Il server di scrcpy è scaricato da tools/fetch_scrcpy.sh (in CI); se manca, il modulo funziona senza.
if [ -f bin/scrcpy-server ] && [ -f bin/scrcpy-server.version ]; then
  cp bin/scrcpy-server bin/scrcpy-server.version stage/bin/
  echo "incluso scrcpy-server v$(cat bin/scrcpy-server.version)"
else
  echo "scrcpy-server non presente: modulo senza video a bassa latenza"
fi
chmod 755 stage/bin/* stage/*.sh stage/META-INF/com/google/android/update-binary
(cd stage && zip -qr9 -X "../$OUT/phantom-f0.zip" .)
rm -rf stage
ls -l "$OUT/phantom-f0.zip"
