#!/usr/bin/env bash
# Prove degli script del modulo (phantomctl, service.sh, post-mortem) su un finto telefono.
# Il «telefono» è una cartella temporanea; la sonda vera è sostituita da uno script che registra gli argomenti.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$HERE/.."
FAIL=0
ok()  { echo "  ok   $1"; }
bad() { echo "  FAIL $1"; FAIL=$((FAIL + 1)); }
check() { if eval "$2"; then ok "$1"; else bad "$1  [$2]"; fi; }

T=$(mktemp -d)
export PHANTOM_MODDIR="$T/mod" PHANTOM_DATA="$T/data"
mkdir -p "$PHANTOM_MODDIR/bin" "$PHANTOM_DATA" "$T/Download"
cp "$SRC/bin/phantomctl" "$SRC/bin/phantom-postmortem.sh" "$SRC/bin/phantom-deep.sh" "$SRC/service.sh" "$PHANTOM_MODDIR/bin/"
mv "$PHANTOM_MODDIR/bin/service.sh" "$PHANTOM_MODDIR/service.sh"
# La finta sonda: registra gli argomenti, scrive il pid e resta in attesa come un demone.
cat > "$PHANTOM_MODDIR/bin/phantom-probe" <<'PROBE'
#!/bin/sh
echo "$@" > "$PHANTOM_DATA/probe.args"
pidfile=""; while [ $# -gt 0 ]; do [ "$1" = "--pidfile" ] && pidfile="$2"; shift; done
( sleep 300 & echo $! > "$pidfile"; wait ) >/dev/null 2>&1 &
PROBE
chmod +x "$PHANTOM_MODDIR/bin/"* "$PHANTOM_MODDIR/service.sh"
echo -n fakejar > "$PHANTOM_MODDIR/bin/scrcpy-server"; echo 5.0.1 > "$PHANTOM_MODDIR/bin/scrcpy-server.version"
# Finti dump di Android per il post-mortem approfondito: watchdog (compresso), tombstone, ANR.
export PHANTOM_DROPBOX="$T/dropbox" PHANTOM_TOMBSTONES="$T/tombstones" PHANTOM_ANR="$T/anr"
mkdir -p "$PHANTOM_DROPBOX" "$PHANTOM_TOMBSTONES" "$PHANTOM_ANR"
cat > "$T/wd.txt" <<'WD'
Process: system_server
Subject: Blocked in monitor com.android.server.wm.WindowManagerGlobalLock on foreground thread (android.fg)
Memory pressure stats:
some avg10=0.50 avg60=1.20 avg300=0.30 total=123456
full avg10=0.10 avg60=0.40 avg300=0.10 total=23456
  23% 1527/surfaceflinger: 12% user + 11% kernel

----- pid 1234 at 2026-11-01 16:41:00.123 -----
Cmd line: system_server

"main" prio=5 tid=1 Blocked
  at com.android.server.wm.WindowManagerService.relayoutWindow(WindowManagerService.java:2000)
  - waiting to lock <0x0a1b2c3d> (a com.android.server.wm.WindowManagerGlobalLock) held by thread 33

"binder:1234_3" prio=5 tid=14 Native
  native: #00 pc 000000000009c6a8  libc.so (__ioctl+8)

"android.display" prio=5 tid=33 Waiting
  at android.view.SurfaceControl.nativeCreateDisplay(Native method)

----- pid 1527 at 2026-11-01 16:41:01.000 -----
Cmd line: /system/bin/surfaceflinger

"surfaceflinger" sysTid=1527
    #00 pc 00000000000a1b2c  /vendor/lib64/libgsl.so (ioctl_kgsl_wait+44)
WD
gzip -c "$T/wd.txt" > "$PHANTOM_DROPBOX/system_server_pre_watchdog@1793547660450.txt.gz"
printf 'Build: test\n' > "$PHANTOM_DROPBOX/SYSTEM_BOOT@1793547572357.txt"
printf 'pid: 1527, tid: 1527, name: surfaceflinger  >>> /system/bin/surfaceflinger <<<\nsignal 6 (SIGABRT), code -1 (SI_QUEUE), fault addr --------\nAbort message: prova\nbacktrace:\n      #00 pc 000000000009c6a8  libc.so (abort+168)\n' > "$PHANTOM_TOMBSTONES/tombstone_43"
touch "$PHANTOM_ANR/anr_2026-11-01-16-41-00"
C="sh $PHANTOM_MODDIR/bin/phantomctl"
args() { cat "$PHANTOM_DATA/probe.args"; }
opts_of() { args | tr ' ' '\n' | grep -A1 -- '--scrcpy-opt' | grep -v -- '--scrcpy-opt\|^--$' ; }

echo "profili"
out=$($C scrcpy profiles)
for n in sw-low hw-low hw-low-rt hw-mid hw-high hw-max hw-max-rt; do check "elenco contiene $n" "echo \"\$out\" | grep -q '$n'"; done

echo "profilo 1 (software, neutro)"
$C scrcpy on 1 >/dev/null
check "argomenti: porta e profilo" "args | grep -q -- '--scrcpy-port 47803 --scrcpy-jar .*scrcpy-server --scrcpy-version 5.0.1 --scrcpy-profile sw-low'"
check "argomenti: cartella di stato" "args | grep -q -- \"--state-dir $PHANTOM_DATA\""
check "encoder software" "opts_of | grep -qx 'video_encoder=c2.android.avc.encoder'"
check "max_size 1280, 30 fps, 4 Mbps" "opts_of | grep -qx 'max_size=1280' && opts_of | grep -qx 'max_fps=30' && opts_of | grep -qx 'video_bit_rate=4000000'"
check "parametri neutri e keyframe ogni 2 s" "opts_of | grep -qx 'video_codec_options=i-frame-interval=2,priority:int=1,latency:int=0'"
check "codec h264" "opts_of | grep -qx 'video_codec=h264'"
check "scrcpy acceso nella configurazione" "grep -qx 'SCRCPY=1' $PHANTOM_DATA/probe.conf"

echo "profilo 2 (hardware, neutro) con H.265"
$C scrcpy on 2 h265 >/dev/null
check "nessun encoder forzato" "! opts_of | grep -q video_encoder"
check "codec h265" "opts_of | grep -qx 'video_codec=h265'"
check "profilo salvato" "grep -qx 'SCRCPY_PROFILE=hw-low' $PHANTOM_DATA/probe.conf"
$C scrcpy on 1 h265 >/dev/null 2>&1; rc=$?
check "H.265 rifiutato con l'encoder software" "[ $rc -ne 0 ]"

echo "profilo 3 (hardware leggero, tempo reale): senza parametri neutri"
$C scrcpy on 3 h264 >/dev/null
check "solo il keyframe ogni 2 s" "opts_of | grep -qx 'video_codec_options=i-frame-interval=2'"
check "stesso carico del profilo 2" "opts_of | grep -qx 'max_size=1280' && opts_of | grep -qx 'max_fps=30'"

echo "profilo 6 (massimo, neutro) e 7 (massimo, tempo reale)"
$C scrcpy on hw-max >/dev/null
check "nessun max_size, 120 fps, 20 Mbps" "! opts_of | grep -q max_size && opts_of | grep -qx 'max_fps=120' && opts_of | grep -qx 'video_bit_rate=20000000'"
check "il massimo è neutro" "opts_of | grep -qx 'video_codec_options=i-frame-interval=2,priority:int=1,latency:int=0'"
$C scrcpy on 7 >/dev/null
check "profilo 7 = massimo con tempo reale" "opts_of | grep -qx 'max_fps=120' && opts_of | grep -qx 'video_codec_options=i-frame-interval=2' && args | grep -q -- '--scrcpy-profile hw-max-rt'"
$C scrcpy on hw-mid-rt >/dev/null
check "variante -rt generica" "opts_of | grep -qx 'max_size=1920' && opts_of | grep -qx 'video_codec_options=i-frame-interval=2'"
$C scrcpy on sw-low-rt >/dev/null 2>&1; rc=$?
check "il software non ha la variante -rt" "[ $rc -ne 0 ]"
$C scrcpy on 9 >/dev/null 2>&1; rc=$?
check "profilo sconosciuto rifiutato" "[ $rc -ne 0 ]"

echo "opzioni extra a mano"
echo 'SCRCPY_OPTS="max_size=999 video_encoder=x.y.z"' >> "$PHANTOM_DATA/probe.conf"
$C restart >/dev/null
check "le opzioni extra vengono per ultime" "args | grep -q -- '--scrcpy-opt max_size=999 --scrcpy-opt video_encoder=x.y.z\$'"

echo "scrcpy off"
$C scrcpy off >/dev/null
check "senza porta scrcpy" "! args | grep -q -- '--scrcpy-port'"

echo "guardia al riavvio dopo un blocco"
$C scrcpy on 4 >/dev/null
printf 'pid=999\nstart=1\nprofilo=hw-mid\nversione=5.0.1\nclient=1.2.3.4:5\nopzioni=max_fps=60 video_bit_rate=10000000\n' > "$PHANTOM_DATA/session.marker"
$C stop >/dev/null
( cd "$T" && sh "$PHANTOM_MODDIR/service.sh" ) >/dev/null 2>&1
check "marker rimosso" "[ ! -f $PHANTOM_DATA/session.marker ]"
check "scrcpy spento per sicurezza" "grep -qx 'SCRCPY=0' $PHANTOM_DATA/probe.conf"
check "rapporto creato" "ls $PHANTOM_DATA/crash-2*.txt >/dev/null 2>&1 && [ -f $PHANTOM_DATA/crash-last.txt ]"
check "il rapporto contiene il marker" "grep -q 'profilo=hw-mid' $PHANTOM_DATA/crash-last.txt && grep -q 'max_fps=60' $PHANTOM_DATA/crash-last.txt"
check "il rapporto contiene il post-mortem" "grep -q 'Marker della sessione interrotta' $PHANTOM_DATA/crash-last.txt && grep -q 'Ultime righe del log' $PHANTOM_DATA/crash-last.txt"
check "il rapporto contiene il post-mortem approfondito" "grep -q 'post-mortem approfondito' $PHANTOM_DATA/crash-last.txt && grep -q 'waiting to lock' $PHANTOM_DATA/crash-last.txt"
check "il servizio ha scritto l'avviso" "grep -q 'ATTENZIONE' $PHANTOM_DATA/service.log"
check "la sonda è ripartita senza scrcpy" "! args | grep -q -- '--scrcpy-port'"
st=$($C status)
check "lo stato mostra l'avviso" "echo \"\$st\" | grep -q 'NON è finita in modo pulito'"
check "lo stato mostra il profilo del blocco" "echo \"\$st\" | grep -q 'profilo=hw-mid'"
$C scrcpy on 1 >/dev/null
check "riaccendere scrcpy cancella l'avviso" "[ ! -f $PHANTOM_DATA/crash-last.txt ]"
check "i rapporti restano archiviati" "ls $PHANTOM_DATA/crash-2*.txt >/dev/null 2>&1"

echo "nessun falso allarme dopo una chiusura pulita"
rm -f "$PHANTOM_DATA"/crash-*.txt "$PHANTOM_DATA/service.log"
$C stop >/dev/null
( cd "$T" && sh "$PHANTOM_MODDIR/service.sh" ) >/dev/null 2>&1
check "nessun rapporto senza marker" "! ls $PHANTOM_DATA/crash-2*.txt >/dev/null 2>&1"
check "scrcpy resta com'era" "grep -qx 'SCRCPY=1' $PHANTOM_DATA/probe.conf"

echo "post-mortem a mano"
out=$($C postmortem 2>&1)
check "il post-mortem produce il rapporto" "echo \"\$out\" | grep -q 'Phantom F0 · post-mortem' && [ -f $PHANTOM_DATA/postmortem.txt ]"

echo "post-mortem approfondito"
out=$($C postmortem deep 2>&1)
D=$PHANTOM_DATA/deep.txt
check "il rapporto approfondito esiste" "[ -s $D ]"
check "elenca la voce del dropbox con la data in UTC" "grep -q 'system_server_pre_watchdog@1793547660450.txt.gz · .* · 2026-11-01 15:41:00 UTC' $D"
check "riporta il motivo del watchdog" "grep -q 'Subject: Blocked in monitor' $D"
check "riporta la pressione di memoria" "grep -q '^full avg10=0.10' $D"
check "il filtro tiene il thread bloccato e chi aspetta" "grep -q 'waiting to lock' $D && grep -q '\"android.display\"' $D"
check "il filtro tiene lo stack di surfaceflinger" "grep -q 'ioctl_kgsl_wait' $D"
check "il filtro scarta i thread inattivi" "! sed -n '/thread bloccati/,/=== Tombstone/p' $D | grep -q 'binder:1234_3'"
check "l'appendice contiene il dump intero" "grep -q '##### INIZIO dump 1' $D && grep -q '##### FINE dump 1' $D"
check "il tombstone è riportato" "grep -q 'signal 6 (SIGABRT)' $D && grep -q 'Abort message: prova' $D"
check "elenca gli ANR" "grep -q 'anr_2026-11-01-16-41-00' $D"
check "nessuna cartella di lavoro lasciata in giro" "[ ! -d $PHANTOM_DATA/deep-work ]"
check "l'uscita a video mostra l'inizio e dice dove è il file" "echo \"\$out\" | grep -q 'phantom-f0-deep.txt\|deep.txt' && echo \"\$out\" | grep -q 'post-mortem approfondito'"
PHANTOM_DROPBOX="$T/vuoto" PHANTOM_TOMBSTONES="$T/vuoto" PHANTOM_ANR="$T/vuoto" $C postmortem deep >/dev/null 2>&1
check "senza dump non si rompe e lo dice" "grep -q 'nessun dump del watchdog' $D && grep -q 'nessun tombstone' $D"
$C postmortem nonsense >/dev/null 2>&1; rc=$?
check "argomento sconosciuto rifiutato" "[ $rc -ne 0 ]"

echo "aiuto"
check "senza argomenti stampa l'uso" "$C nonsense 2>&1 | grep -q 'phantomctl scrcpy on'"

$C stop >/dev/null 2>&1
pkill -f "$T" 2>/dev/null
rm -rf "$T"
if [ $FAIL -eq 0 ]; then echo; echo "Tutti i test superati"; else echo; echo "$FAIL test falliti"; exit 1; fi
