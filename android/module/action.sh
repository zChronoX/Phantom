#!/system/bin/sh
# Pulsante "Azione" nel manager: avvia la sonda, mostra lo stato e genera il report.
MODDIR=${0%/*}
export PHANTOM_MODDIR="$MODDIR"
sh "$MODDIR/bin/phantomctl" start
echo
sh "$MODDIR/bin/phantomctl" status
echo
echo "Genero il report del dispositivo (circa 10 secondi, tieni lo schermo acceso)…"
sh "$MODDIR/bin/phantom-report.sh" > /dev/null 2>&1
echo "Fatto: il report è in Download/phantom-f0-report.txt"
