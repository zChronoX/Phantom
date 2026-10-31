#!/system/bin/sh
# Disinstallazione: ferma la sonda e cancella i suoi dati.
DATA=/data/adb/phantom-f0
[ -f "$DATA/probe.pid" ] && kill "$(cat "$DATA/probe.pid")" 2>/dev/null
rm -rf "$DATA"
