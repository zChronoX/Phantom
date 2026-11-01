#!/system/bin/sh
# Avvio all'accensione (fase late_start). La sonda ascolta su tutte le interfacce,
# quindi non serve aspettare il Wi-Fi: risponde appena la rete è pronta.
MODDIR=${0%/*}
DATA=${PHANTOM_DATA:-/data/adb/phantom-f0}
export PHANTOM_MODDIR="$MODDIR" PHANTOM_DATA="$DATA"
mkdir -p "$DATA"
CONF=$DATA/probe.conf
now() { date -u '+%Y-%m-%dT%H:%M:%SZ'; }

# La modalità Wi-Fi a bassa latenza non sopravvive al riavvio: si azzera anche il flag.
[ -f "$CONF" ] && sed -i 's/^WIFI_LL=1$/WIFI_LL=0/' "$CONF"

# Guardia: il marker esiste solo mentre una sessione scrcpy è in corso. Se c'è ancora, la sessione non è
# finita in modo pulito (di solito il telefono si è bloccato e lo si è riavviato a mano): si prepara un
# rapporto con tutto ciò che il sistema ha conservato e si SPEGNE scrcpy, così non si ripete da sé.
if [ -f "$DATA/session.marker" ]; then
    TS=$(date -u '+%Y%m%dT%H%M%SZ')
    REP=$DATA/crash-$TS.txt
    {
        echo "Rapporto di chiusura anomala · $TS"
        echo "Una sessione scrcpy NON è terminata in modo pulito prima di questo avvio: il telefono si è probabilmente bloccato."
        echo
        echo "=== Marker (impostazioni in uso al momento del blocco) ==="
        cat "$DATA/session.marker"
        sh "$MODDIR/bin/phantom-postmortem.sh"
    } > "$REP" 2>&1
    cp "$REP" "$DATA/crash-last.txt"
    cp "$REP" /sdcard/Download/phantom-f0-crash-last.txt 2>/dev/null
    rm -f "$DATA/session.marker"
    [ -f "$CONF" ] && sed -i 's/^SCRCPY=1$/SCRCPY=0/' "$CONF"
    echo "$(now) ATTENZIONE: sessione scrcpy interrotta male; scrcpy spento; rapporto in $REP" >> "$DATA/service.log"
    # si tengono gli ultimi 5 rapporti
    # shellcheck disable=SC2012  # nomi di file generati da noi, solo cifre e lettere
    ls -t "$DATA"/crash-2*.txt 2>/dev/null | tail -n +6 | while read -r f; do rm -f "$f"; done
fi

[ -f "$CONF" ] && . "$CONF"
if [ "${AUTOSTART:-1}" = 1 ]; then
    echo "$(now) avvio dal boot" >> "$DATA/service.log"
    sh "$MODDIR/bin/phantomctl" start >> "$DATA/service.log" 2>&1
fi
