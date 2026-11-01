#!/system/bin/sh
# Phantom F0 · post-mortem approfondito.
# Legge i dump che Android ha già salvato da solo dopo un blocco: quelli del watchdog di system_server
# (contengono gli stack dei thread bloccati e dei processi nativi come surfaceflinger), i tombstone dei crash
# nativi e gli ANR. Non avvia nessuna cattura dello schermo: è sicuro a ogni ora.
# Non raccoglie nomi di reti Wi-Fi, account o contenuti personali.
#
# Variabili per i test: PHANTOM_DATA, PHANTOM_DROPBOX, PHANTOM_TOMBSTONES, PHANTOM_ANR, PHANTOM_NDUMPS, PHANTOM_DUMP_MAXKB.

DATA=${PHANTOM_DATA:-/data/adb/phantom-f0}
DROP=${PHANTOM_DROPBOX:-/data/system/dropbox}
TOMB=${PHANTOM_TOMBSTONES:-/data/tombstones}
ANRD=${PHANTOM_ANR:-/data/anr}
NDUMPS=${PHANTOM_NDUMPS:-2}          # quanti dump del watchdog riportare per intero in appendice
MAXKB=${PHANTOM_DUMP_MAXKB:-700}     # tetto di dimensione per ciascuno
WORK=$DATA/deep-work

have() { command -v "$1" >/dev/null 2>&1; }
sec() { echo; echo "=== $1 ==="; }
unz() { zcat "$1" 2>/dev/null || gzip -dc "$1" 2>/dev/null || gunzip -c "$1" 2>/dev/null; }
# testo di un dump, compresso o no
dump_text() { case "$1" in *.gz) unz "$1" ;; *) cat "$1" ;; esac; }
# data (UTC) da un nome come system_server_pre_watchdog@1793545722653.txt.gz
utc_of() {
    ms=${1##*@}; ms=${ms%%.*}
    case "$ms" in *[!0-9]* | "") echo "?"; return ;; esac
    s=$((ms / 1000))
    d=$(date -u -d "@$s" '+%Y-%m-%d %H:%M:%S' 2>/dev/null) && echo "$d UTC" || echo "epoch $s"
}

# Filtro dei thread interessanti in un dump (stack Java dell'ART o stack nativi di debuggerd).
# Un blocco comincia con «----- pid N», oppure con una riga che inizia per virgolette (un thread).
# Si tengono: i thread «Blocked», quelli che citano blocchi/schermo/codec/disco, e tutti i thread
# dei processi nativi che interessano (surfaceflinger, composer, codec, ...).
# shellcheck disable=SC2016  # è un programma awk: i $ non vanno espansi dalla shell
AWKPROG='
function flush(   i, m) {
    if (started && n > 0 && keep && blocks < maxb) {
        m = (n < maxl) ? n : maxl
        for (i = 1; i <= m; i++) print buf[i]
        if (n > maxl) print "    ... (altre " (n - maxl) " righe)"
        print ""
        blocks++
    }
    n = 0; keep = 0
}
/^----- pid / { flush(); proc = ""; started = 1 }
/^Cmd line: / { proc = substr($0, 11) }
/^"/ { flush(); started = 1 }
{
    if ($0 == "") next
    buf[++n] = substr($0, 1, 200)
    if (pat != "" && $0 ~ pat) keep = 1
    if (n == 1 && $0 ~ /^"/) {
        if (head_re != "" && $0 ~ head_re) keep = 1
        if (proc != "" && proc_re != "" && proc ~ proc_re) keep = 1
    }
}
END { flush() }
'
PAT='waiting to lock|held by|waiting on <|kgsl|dequeueBuffer|waitForFence|sync_file|fsync|f2fs|DisplayManager|createVirtualDisplay|VirtualDisplay|SurfaceControl|SurfaceFlinger|surfaceflinger|MediaCodec|Codec2|vidc|BufferQueue|HWC|composer|Cmd line'
PROC_RE='surfaceflinger|composer|media[.]|codec|vidc|cameraserver|display'

echo "Phantom F0 · post-mortem approfondito"
echo "generato: $(date -u '+%Y-%m-%d %H:%M:%S') UTC · il telefono è acceso da $(cut -d' ' -f1 /proc/uptime 2>/dev/null) s"
echo "kernel: $(uname -r 2>/dev/null)"

rm -rf "$WORK"; mkdir -p "$WORK"

sec "Voci del dropbox di sistema (data in UTC ricavata dal nome del file)"
found=0
for f in "$DROP"/system_server_* "$DROP"/system_app_* "$DROP"/SYSTEM_TOMBSTONE* "$DROP"/SYSTEM_LAST_KMSG* "$DROP"/SYSTEM_RECOVERY* "$DROP"/data_app_*; do
    [ -f "$f" ] || continue
    b=${f##*/}
    echo "$b · $(wc -c < "$f") byte · $(utc_of "$b")"
    found=1
done
[ "$found" = 1 ] || echo "(nessuna voce di watchdog, crash o ANR)"

sec "Dump del watchdog di system_server: riassunto"
# I più recenti per primi; si considerano sia «pre_watchdog» (blocco da 30 s) sia «watchdog» (blocco da 60 s, riavvio del sistema).
# shellcheck disable=SC2012  # elenco ordinato per data; nomi generati da Android
LIST=$(ls -t "$DROP"/system_server_pre_watchdog@* "$DROP"/system_server_watchdog@* 2>/dev/null | head -n "$NDUMPS")
if [ -z "$LIST" ]; then
    echo "(nessun dump del watchdog: il sistema non ha rilevato blocchi di system_server, oppure sono stati cancellati)"
fi
i=0
for f in $LIST; do
    i=$((i + 1))
    b=${f##*/}
    T=$WORK/dump$i.txt
    dump_text "$f" > "$T" 2>/dev/null
    echo
    echo "--- dump $i: $b · $(utc_of "$b") · $(wc -c < "$T") byte non compressi"
    echo "[intestazione]"
    sed -n '1,28p' "$T" | cut -c1-220
    echo "[pressione di memoria e CPU al momento del dump]"
    grep -m 8 -E '^(some|full) avg10=' "$T"
    grep -m 3 -E 'CPU usage from|^Load: ' "$T" | cut -c1-200
    grep -m 25 -E '^ +[0-9]+([.][0-9]+)?% [0-9]+/' "$T" | cut -c1-200
    echo "[thread bloccati e stack che toccano schermo, codec, disco, blocchi]"
    awk -v pat="$PAT" -v head_re="Blocked" -v proc_re="$PROC_RE" -v maxb=60 -v maxl=18 "$AWKPROG" "$T"
done

sec "Tombstone recenti: intestazione e stack"
n=0
# shellcheck disable=SC2012  # elenco ordinato per data; i nomi sono generati da Android (tombstone_NN)
for f in $(ls -t "$TOMB"/tombstone_?? 2>/dev/null | head -n 3); do
    n=$((n + 1))
    echo
    echo "--- $f · $(wc -c < "$f") byte · $(ls -l "$f" | awk '{ print $6, $7 }')"
    sed -n '1,80p' "$f" | cut -c1-200
done
[ "$n" = 0 ] && echo "(nessun tombstone)"

sec "ANR e tracce (cartella /data/anr)"
# shellcheck disable=SC2012
ls -lt "$ANRD" 2>/dev/null | head -n 8 || echo "(vuota o non leggibile)"

sec "Messaggi del kernel di QUESTO avvio su schermo, GPU, disco, memoria (utili come riferimento)"
if have dmesg; then
    dmesg 2>/dev/null | grep -i -E 'kgsl|adreno|gpu|msm_drm|sde|dsi|smmu|iommu|ufs|f2fs|watchdog|bite|oom|lowmem|hung|stall|lockup|fault|panic|vidc|venus|thermal' | tail -n 60 | cut -c1-220
else
    echo "dmesg non disponibile"
fi

sec "Memoria e carico adesso"
head -n 4 /proc/meminfo 2>/dev/null
grep -E 'SwapTotal|SwapFree' /proc/meminfo 2>/dev/null
for p in cpu memory io; do [ -f "/proc/pressure/$p" ] && { echo "pressure/$p:"; cat "/proc/pressure/$p"; }; done
cat /proc/loadavg 2>/dev/null

sec "Proprietà grafiche e di sistema che contano per la cattura dello schermo"
if have getprop; then
    getprop | grep -i -E 'surface_flinger|renderengine|debug[.]sf|hwui|vulkan|opengles|sf[.]|display[.]|composer|vsync|gralloc|ro[.]build[.](fingerprint|version[.]incremental)|ro[.]vendor[.]build[.]fingerprint' | cut -c1-200 | head -n 60
fi

sec "Appendice: testo completo dei dump del watchdog (decompressi)"
i=0
for f in $LIST; do
    i=$((i + 1))
    T=$WORK/dump$i.txt
    [ -s "$T" ] || continue
    echo
    echo "##### INIZIO dump $i: ${f##*/}"
    head -c $((MAXKB * 1024)) "$T"
    echo
    echo "##### FINE dump $i"
done

rm -rf "$WORK"
