#!/system/bin/sh
# Phantom F0 · post-mortem: raccoglie le prove di un blocco o di un riavvio anomalo.
# Si usa in due modi: a mano (phantomctl postmortem) e in automatico all'avvio, quando il marker di una
# sessione scrcpy è rimasto (la sessione non è finita in modo pulito: di solito il telefono si è bloccato).
# Non raccoglie nomi di reti Wi-Fi, account o contenuti personali; il logcat è filtrato sulle parti di
# sistema che riguardano schermo, codec e watchdog.

DATA=${PHANTOM_DATA:-/data/adb/phantom-f0}
have() { command -v "$1" >/dev/null 2>&1; }
sec() { echo; echo "=== $1 ==="; }

echo "Phantom F0 · post-mortem"
echo "generato: $(date -u '+%Y-%m-%d %H:%M:%S') UTC · il telefono è acceso da $(cut -d' ' -f1 /proc/uptime 2>/dev/null) s"

sec "Perché il telefono si è riavviato"
if have getprop; then
    for k in sys.boot.reason sys.boot.reason.last ro.boot.bootreason persist.sys.boot.reason \
             sys.boot.reason.history persist.sys.boot.reason.history ro.boot.hardware.revision; do
        v=$(getprop "$k")
        [ -n "$v" ] && echo "$k=$v"
    done
    echo "(reboot,longkey / hard_reset = tenuto premuto l'accensione · watchdog = riavvio per blocco del sistema · kernel_panic = errore del kernel)"
else
    echo "getprop non disponibile"
fi

sec "Marker della sessione interrotta"
[ -f "$DATA/session.marker" ] && cat "$DATA/session.marker" || echo "(nessun marker: l'ultima sessione è finita in modo pulito)"

sec "Ultime righe del log della sonda (comprese quelle del server scrcpy)"
if [ -f "$DATA/probe.log" ]; then tail -n 90 "$DATA/probe.log"; else echo "(nessun log)"; fi

sec "Rapporti precedenti"
ls -l "$DATA"/crash-2*.txt 2>/dev/null || echo "(nessuno)"

sec "pstore: memoria dei crash del kernel (se la ROM la supporta)"
if [ -d /sys/fs/pstore ]; then
    ls -l /sys/fs/pstore 2>&1
    for f in /sys/fs/pstore/console-ramoops* /sys/fs/pstore/dmesg-ramoops*; do
        [ -f "$f" ] && { echo "-- $f (ultime 150 righe)"; tail -n 150 "$f"; }
    done
else
    echo "(non presente)"
fi
[ -f /proc/last_kmsg ] && { sec "Kernel: log del riavvio precedente"; tail -n 120 /proc/last_kmsg; }

sec "logcat prima del riavvio (solo parti di sistema rilevanti)"
if have logcat; then
    logcat -L -d 2>&1 | grep -E "SurfaceFlinger|surfaceflinger|c2\.|Codec2|codec2|MediaCodec|scrcpy|Watchdog|WATCHDOG|FATAL EXCEPTION|ANR in|kgsl|adreno|vidc|venus|HWC|hwcomposer|BufferQueue|VirtualDisplay|DisplayManager" | tail -n 150
    echo "(se qui non c'è nulla, la ROM non conserva il logcat dopo il riavvio)"
else
    echo "logcat non disponibile"
fi

sec "Tombstone recenti (crash di processi nativi)"
# shellcheck disable=SC2012  # elenco leggibile per una persona; su Android find non ha -printf
ls -lt /data/tombstones 2>/dev/null | head -n 8 || echo "(nessuno)"

sec "Dropbox di sistema (watchdog, crash, ANR)"
# shellcheck disable=SC2012
ls -lt /data/system/dropbox 2>/dev/null | head -n 15 || echo "(nessuno)"

sec "Processi di cattura e codec in questo momento"
# shellcheck disable=SC2009  # il pgrep di Android non permette di elencare più nomi
if have ps; then ps -A 2>/dev/null | grep -E "scrcpy|app_process|screenrecord|phantom|media.codec|c2@|surfaceflinger" | head -n 20; fi

sec "Temperatura e batteria"
if have dumpsys; then
    dumpsys thermalservice 2>/dev/null | grep -m 6 -E "Thermal Status|mName=(CPU|battery|skin)" | cut -c1-120
    dumpsys battery 2>/dev/null | grep -E "level:|temperature:|status:"
fi
