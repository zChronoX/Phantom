#!/system/bin/sh
# Phantom F0 · report del dispositivo.
# Raccoglie tutto ciò che serve per decidere API di cattura, encoder e strategia per il Doze.
# Va eseguito come root, con lo schermo acceso (contiene una prova di cattura di 5 secondi).
# Non raccoglie nomi di reti Wi-Fi, numeri, account o contenuti personali.

DATA=/data/adb/phantom-f0
REPORT=$DATA/report.txt
mkdir -p "$DATA"

section() { echo; echo "=== $1 ==="; }
prop() { v=$(getprop "$1"); [ -n "$v" ] && printf '%-36s %s\n' "$1" "$v"; }

codec_blocks() {
    # Stampa i blocchi <MediaCodec> degli encoder video (avc, hevc, vp8/9, av1) con limiti e funzioni.
    if command -v awk >/dev/null 2>&1; then
        awk '/<MediaCodec[ >]/ { blk = $0; inb = 1; if ($0 ~ /\/>/) inb = 0; next }
             inb { blk = blk "\n" $0 }
             /<\/MediaCodec>/ { if (inb && blk ~ /(encoder|enc")/ && blk ~ /(avc|hevc|vp8|vp9|av01|av1)/) print blk; inb = 0 }' "$1"
    else
        sed -n '/<MediaCodec .*enc/,/<\/MediaCodec>/p' "$1"
    fi | sed 's/^[[:space:]]*//' | grep -vE '^$|^<!--'
}

{
echo "Phantom F0 · report del dispositivo"
echo "generato: $(date -u '+%Y-%m-%d %H:%M:%S') UTC"

section "Sistema"
for k in ro.product.manufacturer ro.product.model ro.product.device ro.product.name \
         ro.build.version.release ro.build.version.sdk ro.build.version.security_patch \
         ro.lineage.version ro.lineage.build.version ro.lineage.releasetype ro.modversion \
         ro.build.display.id ro.build.type ro.build.flavor ro.vendor.build.fingerprint \
         ro.board.platform ro.soc.manufacturer ro.soc.model ro.hardware ro.product.cpu.abi \
         ro.vndk.version ro.hardware.egl ro.hardware.vulkan; do prop "$k"; done
echo "kernel                               $(uname -r)"
echo "uptime                               $(cut -d' ' -f1 /proc/uptime) s"

section "Root e SELinux"
echo "utente      $(id)"
echo "contesto    $(cat /proc/self/attr/current 2>/dev/null)"
echo "SELinux     $(getenforce 2>/dev/null)"
[ -d /data/adb/ksu ] && echo "KernelSU/SukiSU presente (/data/adb/ksu)"
for b in /data/adb/ksud /data/adb/ksu/bin/ksud; do
    [ -x "$b" ] && echo "ksud        $("$b" -V 2>/dev/null || "$b" --version 2>/dev/null)"
done
[ -d /data/adb/magisk ] && echo "Magisk presente"
command -v su >/dev/null && echo "su          $(command -v su)"

section "Schermo"
wm size 2>&1 | cat   # pipe: i servizi di sistema non possono scrivere su file in /data/adb
wm density 2>&1 | cat
dumpsys display 2>/dev/null | grep -m 10 -E "mActiveModeId|supportedModes|renderFrameRate|mDefaultModeId|DisplayDeviceInfo\{" | cut -c1-400
dumpsys SurfaceFlinger 2>/dev/null | grep -m 6 -iE "refresh-rate|vsync-period|present offset|app phase|sf phase" | cut -c1-200

section "Encoder video dichiarati"
files=""
for f in /vendor/etc/media_codecs*.xml /odm/etc/media_codecs*.xml /system/etc/media_codecs*.xml \
         /apex/com.android.media.swcodec/etc/media_codecs*.xml; do
    [ -f "$f" ] && files="$files $f"
done
for f in $files; do
    out=$(codec_blocks "$f")
    [ -n "$out" ] && { echo "-- $f"; echo "$out" | head -n 160; }
done
echo
# shellcheck disable=SC2086  # $files va diviso in parole di proposito
echo "file con intra-refresh:  $(grep -l 'intra-refresh' $files 2>/dev/null | tr '\n' ' ')"
# shellcheck disable=SC2086
echo "file con low-latency:    $(grep -l 'low-latency' $files 2>/dev/null | tr '\n' ' ')"
# shellcheck disable=SC2010
echo "Codec2 vendor (servizi): $(ls /vendor/bin/hw 2>/dev/null | grep -iE 'codec2|media.c2' | tr '\n' ' ')"

section "Prova di cattura con screenrecord (5 s, encoder hardware)"
screenrecord --help 2>&1 | grep -E "time-limit|bit-rate|codec|display-id|output-format" | head -n 8
CAP=$DATA/cap.h264
rm -f "$CAP"
t0=$(date +%s)
screenrecord --verbose --output-format=h264 --time-limit 5 --bit-rate 20000000 "$CAP" > "$DATA/cap.log" 2>&1
rc=$?
t1=$(date +%s)
head -n 25 "$DATA/cap.log"
echo "codice di uscita $rc, durata $((t1 - t0)) s, byte catturati $(wc -c < "$CAP" 2>/dev/null || echo 0)"
rm -f "$CAP"

section "Temperatura e batteria"
dumpsys thermalservice 2>/dev/null | grep -m 8 -E "Thermal Status|IsStatusOverride|Temperature\{mValue" | cut -c1-200
dumpsys battery 2>/dev/null | grep -E "AC powered|USB powered|status:|level:|temperature:|health:"

section "Doze e risparmio energetico"
dumpsys deviceidle 2>/dev/null | grep -m 10 -E "mDeepEnabled|mLightEnabled|mForceIdle|mScreenOn|mCharging|mState=|mLightState=" | cut -c1-200
echo "risparmio energetico: $(settings get global low_power 2>/dev/null)"
echo "Wi-Fi in sospensione: $(settings get global wifi_sleep_policy 2>/dev/null)"

section "Rete"
ip -4 -o addr show 2>/dev/null | awk '{ split($4, a, "/"); print $2, a[1] }'
dumpsys wifi 2>/dev/null | grep -m 1 "mWifiInfo" | grep -oE "(Wi-Fi standard|RSSI|Link speed|Tx Link speed|Rx Link speed|Frequency|Max Supported Tx Link speed): [^,]*" | head -n 8
echo "Tailscale installato: $(pm list packages 2>/dev/null | grep -ci tailscale)"
ip -4 -o addr show tailscale0 2>/dev/null | awk '{ split($4, a, "/"); print "tailscale0", a[1] }'

section "Sonda Phantom"
sh "${PHANTOM_MODDIR:-/data/adb/modules/phantom_f0}/bin/phantomctl" status 2>&1
} > "$REPORT" 2>&1

cat "$REPORT"
cp "$REPORT" /sdcard/Download/phantom-f0-report.txt 2>/dev/null && echo && echo "Report salvato in Download/phantom-f0-report.txt"
