#!/system/bin/sh
# Avvio all'accensione (fase late_start). La sonda ascolta su tutte le interfacce,
# quindi non serve aspettare il Wi-Fi: risponde appena la rete è pronta.
MODDIR=${0%/*}
DATA=/data/adb/phantom-f0
mkdir -p "$DATA"
[ -f "$DATA/probe.conf" ] && . "$DATA/probe.conf"
# La modalità Wi-Fi a bassa latenza non sopravvive al riavvio: si azzera anche il flag.
[ -f "$DATA/probe.conf" ] && sed -i 's/^WIFI_LL=1$/WIFI_LL=0/' "$DATA/probe.conf"
if [ "${AUTOSTART:-1}" = 1 ]; then
    echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') avvio dal boot" >> "$DATA/service.log"
    PHANTOM_MODDIR=$MODDIR sh "$MODDIR/bin/phantomctl" start >> "$DATA/service.log" 2>&1
fi
