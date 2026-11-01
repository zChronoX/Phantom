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
cp "$SRC/bin/phantomctl" "$SRC/bin/phantom-postmortem.sh" "$SRC/bin/phantom-deep.sh" "$SRC/bin/phantom-captest.sh" "$SRC/service.sh" "$PHANTOM_MODDIR/bin/"
cp "$SRC/module.prop" "$PHANTOM_MODDIR/module.prop"
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
cp "$T/wd.txt" "$PHANTOM_ANR/anr_2026-11-01-16-41-00"
printf 'questo non è un gzip' > "$PHANTOM_DROPBOX/system_server_pre_watchdog@1793545722653.txt.gz"
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
check "argomenti: scatola nera sulla porta 47804" "args | grep -q -- '--telemetry-port 47804'"
check "argomenti: arresto automatico in stallo" "args | grep -q -- '--tm-autostop'"
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
printf 'T 100.000 e=1 | c u5 | PROVA-SCATOLA-NERA\nS 101.000 inizio: 5 task bloccati nel kernel\n' > "$PHANTOM_DATA/blackbox.txt"
$C stop >/dev/null
( cd "$T" && sh "$PHANTOM_MODDIR/service.sh" ) >/dev/null 2>&1
check "marker rimosso" "[ ! -f $PHANTOM_DATA/session.marker ]"
check "scrcpy spento per sicurezza" "grep -qx 'SCRCPY=0' $PHANTOM_DATA/probe.conf"
check "rapporto creato" "ls $PHANTOM_DATA/crash-2*.txt >/dev/null 2>&1 && [ -f $PHANTOM_DATA/crash-last.txt ]"
check "il rapporto contiene il marker" "grep -q 'profilo=hw-mid' $PHANTOM_DATA/crash-last.txt && grep -q 'max_fps=60' $PHANTOM_DATA/crash-last.txt"
check "il rapporto contiene il post-mortem" "grep -q 'Marker della sessione interrotta' $PHANTOM_DATA/crash-last.txt && grep -q 'Ultime righe del log' $PHANTOM_DATA/crash-last.txt"
check "il rapporto contiene il post-mortem approfondito" "grep -q 'post-mortem approfondito' $PHANTOM_DATA/crash-last.txt && grep -q 'waiting to lock' $PHANTOM_DATA/crash-last.txt"
check "il rapporto contiene la scatola nera" "grep -q 'Scatola nera: ultime 250 righe' $PHANTOM_DATA/crash-last.txt && grep -q 'PROVA-SCATOLA-NERA' $PHANTOM_DATA/crash-last.txt && grep -q 'inizio: 5 task bloccati' $PHANTOM_DATA/crash-last.txt"
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
check "il filtro scarta i thread inattivi" "! sed -n '/^\\[thread bloccati/,/^\\(--- \\|=== \\)/p' $D | grep -q 'binder:1234_3'"
check "l'appendice contiene il dump intero" "grep -q '##### INIZIO dump [12]: system_server_pre_watchdog@1793547660450' $D && grep -q '##### FINE dump [12]' $D"
check "il tombstone è riportato" "grep -q 'signal 6 (SIGABRT)' $D && grep -q 'Abort message: prova' $D"
check "elenca gli ANR" "grep -q 'anr_2026-11-01-16-41-00' $D"
check "i file ANR in chiaro vengono riassunti" "sed -n '/File ANR in chiaro/,/Dump del watchdog di system_server/p' $D | grep -q 'waiting to lock'"
check "e riportati per intero in appendice" "grep -q '##### INIZIO ANR 1: anr_2026-11-01-16-41-00' $D && grep -q '##### FINE ANR 1' $D"
check "un dump non decomprimibile lo dice" "grep -q 'NON decomprimibile su questo telefono' $D"
check "e ne riporta la copia codificata" "grep -q '##### INIZIO copia codificata del dump' $D && grep -q '##### FINE copia codificata del dump' $D"
check "la data del dump non decomprimibile è giusta (15:08:42 UTC)" "grep -q 'system_server_pre_watchdog@1793545722653.txt.gz · .* · 2026-11-01 15:08:42 UTC' $D"
check "nessuna cartella di lavoro lasciata in giro" "[ ! -d $PHANTOM_DATA/deep-work ]"
check "l'uscita a video mostra l'inizio e dice dove è il file" "echo \"\$out\" | grep -q 'phantom-f0-deep.txt\|deep.txt' && echo \"\$out\" | grep -q 'post-mortem approfondito'"
PHANTOM_DROPBOX="$T/vuoto" PHANTOM_TOMBSTONES="$T/vuoto" PHANTOM_ANR="$T/vuoto" $C postmortem deep >/dev/null 2>&1
check "senza dump non si rompe e lo dice" "grep -q 'nessun dump del watchdog' $D && grep -q 'nessun tombstone' $D"
$C postmortem nonsense >/dev/null 2>&1; rc=$?
check "argomento sconosciuto rifiutato" "[ $rc -ne 0 ]"

echo "scatola nera: comandi"
mkdir -p "$T/bin"
cat > "$T/bin/nc" <<'NC'
#!/bin/sh
# Finto nc: telemetria (porta 47804) come prima; per la cattura (47803) si comporta come il vero, che chiude il collegamento
# appena il suo ingresso finisce (EOF) e resta aperto finché l'ingresso è aperto. Alla fine la «sonda» scrive la riga di sessione.
echo "$@" >> "$PHANTOM_DATA/nc.calls"
if [ "$2" = 47804 ]; then echo "H telemetria 1 sonda=prova"; echo "T 1.000 e=1 | c u1 | finto"; sleep 30; exit 0; fi
t0=$(date +%s)
[ -n "$FAKE_NC_QUICK" ] || cat >/dev/null
dt=$(( $(date +%s) - t0 ))
frames=$(( dt * 30 )); [ -n "$FAKE_RX" ] && frames=$FAKE_RX
[ -n "$FAKE_NC_SILENT" ] || echo "2026-11-01T22:00:00Z scrcpy: sessione con 127.0.0.1:5 conclusa (l'iPhone ha chiuso) · frame ricevuti $frames, inviati $frames, scartati 0 in 0 eventi · 1.5 MB inviati" >> "$PHANTOM_DATA/probe.log"
NC
chmod +x "$T/bin/nc"
$C scrcpy on 1 >/dev/null
out=$(PATH="$T/bin:$PATH" $C live 1 2>&1)
check "live mostra le righe della telemetria" "echo \"\$out\" | grep -q 'H telemetria 1' && echo \"\$out\" | grep -q 'T 1.000'"
check "live si collega alla porta 47804 in locale" "grep -qx '127.0.0.1 47804' $PHANTOM_DATA/nc.calls"
out=$($C blackbox 5 2>&1)
check "blackbox mostra le ultime righe del registro" "echo \"\$out\" | grep -q 'PROVA-SCATOLA-NERA'"
rm -f "$PHANTOM_DATA/blackbox.txt"
out=$($C blackbox 2>&1)
check "blackbox senza registro lo dice" "echo \"\$out\" | grep -q 'nessun registro'"
echo "prova locale (captest): deve dire la verità"
export PHANTOM_CAPTEST_TAIL=1 PHANTOM_CAPTEST_ALIVE_TIMEOUT=1
cat > "$T/bin/wmfake" <<'WM'
#!/bin/sh
# finto «cmd window size»: risponde, a meno che il file «congelato» esista (allora resta appeso, come con il blocco del window manager)
if [ -f "$PHANTOM_DATA/congelato" ]; then sleep 100; fi
echo "Physical size: 1080x2400"
WM
chmod +x "$T/bin/wmfake"
export PHANTOM_WMCMD="$T/bin/wmfake"
rm -f "$PHANTOM_DATA/nc.calls" "$PHANTOM_DATA/congelato"

# 1) prova regolare: il collegamento resta aperto per tutta la durata e l'esito è VALIDA
out=$(PATH="$T/bin:$PATH" $C captest 4 4 2>&1)
check "captest accende scrcpy col profilo scelto" "args | grep -q -- '--scrcpy-profile hw-mid'"
check "captest spiega come proseguire" "echo \"\$out\" | grep -q 'PROVA LOCALE avviata' && echo \"\$out\" | grep -q 'Post-mortem approfondito' && echo \"\$out\" | grep -q 'RISULTATO: VALIDA'"
check "captest parte staccato (il comando torna subito)" "grep -q 'INIZIO prova locale · profilo hw-mid · 4 s' $PHANTOM_DATA/captest.log"
sleep 9
check "captest si collega al ponte scrcpy in locale" "grep -qx '127.0.0.1 47803' $PHANTOM_DATA/nc.calls"
check "captest scrive le leve in uso" "grep -q 'schermo automatico · display virtuali predefiniti · modulo v0.5' $PHANTOM_DATA/captest.log"
check "captest tiene il collegamento aperto per tutta la durata e dice VALIDA" "grep -q 'RISULTATO: VALIDA · NESSUN BLOCCO' $PHANTOM_DATA/captest.log"
check "captest conta i controlli del window manager" "grep -Eq 'il window manager ha risposto [0-9]+ volte su [0-9]+' $PHANTOM_DATA/captest.log"
check "captest status mostra l'esito" "$C captest status | grep -q 'RISULTATO: VALIDA'"

# 2) il difetto della 0.5.1: collegamento chiuso subito e nessun frame → NON deve mai dire «non si è bloccato»
export FAKE_NC_QUICK=1 FAKE_RX=0
PATH="$T/bin:$PATH" $C captest 4 4 manual >/dev/null 2>&1
sleep 9
check "se il collegamento si chiude subito senza frame la prova è NON VALIDA" "grep -q 'RISULTATO: NON VALIDA · la sessione si è chiusa senza nessun frame' $PHANTOM_DATA/captest.log"
check "…e non dice mai «NESSUN BLOCCO» né «non si è bloccato»" "! grep -q 'NESSUN BLOCCO' $PHANTOM_DATA/captest.log && ! grep -q 'NON si è bloccato' $PHANTOM_DATA/captest.log"
unset FAKE_NC_QUICK FAKE_RX

# 3) la sonda non ha registrato nessuna sessione
export FAKE_NC_SILENT=1
PATH="$T/bin:$PATH" $C captest 4 4 manual >/dev/null 2>&1
sleep 9
check "senza riga di sessione nel log della sonda la prova è NON VALIDA" "grep -q 'RISULTATO: NON VALIDA · la sonda non ha registrato nessuna sessione' $PHANTOM_DATA/captest.log"
unset FAKE_NC_SILENT

# 4) la scatola nera segnala uno stallo durante la prova, anche se il controllo risponde
PATH="$T/bin:$PATH" $C captest 4 4 manual >/dev/null 2>&1
sleep 1
echo "S 100.000 inizio: surfaceflinger aspetta da 2100 ms la risposta di un altro processo" >> "$PHANTOM_DATA/blackbox.txt"
sleep 9
check "stallo della scatola nera durante la prova: esito BLOCCO" "grep -q 'RISULTATO: BLOCCO · la scatola nera ha visto uno stallo' $PHANTOM_DATA/captest.log && grep -q 'surfaceflinger aspetta da 2100 ms' $PHANTOM_DATA/captest.log"
rm -f "$PHANTOM_DATA/blackbox.txt"

# 5) il window manager si ferma durante la cattura: BLOCCO rilevato, si ferma la cattura e si guarda se il sistema si riprende
PATH="$T/bin:$PATH" $C captest 30 4 manual >/dev/null 2>&1
sleep 2
touch "$PHANTOM_DATA/congelato"
for i in $(seq 1 40); do grep -q 'BLOCCO RILEVATO' "$PHANTOM_DATA/captest.log" && break; sleep 0.5; done
check "window manager fermo: BLOCCO RILEVATO" "grep -q 'BLOCCO RILEVATO dopo' $PHANTOM_DATA/captest.log"
rm -f "$PHANTOM_DATA/congelato"
for i in $(seq 1 40); do grep -q 'RISULTATO: BLOCCO' "$PHANTOM_DATA/captest.log" && break; sleep 0.5; done
check "se si riprende dopo aver fermato la cattura lo scrive" "grep -q 'RISULTATO: BLOCCO · il sistema si è RIPRESO' $PHANTOM_DATA/captest.log"
rm -f "$PHANTOM_DATA/congelato"

# 6) il sistema è già bloccato prima di iniziare
touch "$PHANTOM_DATA/congelato"
PATH="$T/bin:$PATH" $C captest 4 4 manual >/dev/null 2>&1
sleep 3
check "se il window manager non risponde già all'inizio la prova è NON VALIDA" "grep -q 'RISULTATO: NON VALIDA · il window manager non risponde NEMMENO prima' $PHANTOM_DATA/captest.log"
rm -f "$PHANTOM_DATA/congelato"
unset PHANTOM_WMCMD PHANTOM_CAPTEST_TAIL PHANTOM_CAPTEST_ALIVE_TIMEOUT

$C captest 1 >/dev/null 2>&1; rc=$?
check "captest rifiuta durate troppo brevi" "[ $rc -ne 0 ]"
$C captest 5 99 >/dev/null 2>&1; rc=$?
check "captest rifiuta profili sconosciuti" "[ $rc -ne 0 ]"
$C captest 5 4 sempre >/dev/null 2>&1; rc=$?
check "captest rifiuta una modalità sconosciuta" "[ $rc -ne 0 ]"
echo 'TELEMETRY=0' >> "$PHANTOM_DATA/probe.conf"
$C restart >/dev/null
check "con TELEMETRY=0 la porta di telemetria non viene richiesta" "! args | grep -q -- '--telemetry-port'"
check "live lo dice chiaramente" "$C live 1 2>&1 | grep -q 'telemetria è spenta'"
sed -i '/^TELEMETRY=0$/d' "$PHANTOM_DATA/probe.conf"
$C restart >/dev/null

echo "leve di prova: refresh e display virtuali"
cat > "$T/bin/settings" <<'SET'
#!/bin/sh
# finto `settings`: chiave=valore in un file
DB="$PHANTOM_DATA/settings.db"; touch "$DB"
case "$1" in
    get) v=$(grep "^$3=" "$DB" | cut -d= -f2); echo "${v:-null}" ;;
    put) grep -v "^$3=" "$DB" > "$DB.t"; echo "$3=$4" >> "$DB.t"; mv "$DB.t" "$DB" ;;
    delete) grep -v "^$3=" "$DB" > "$DB.t"; mv "$DB.t" "$DB" ;;
esac
SET
chmod +x "$T/bin/settings"
cat > "$T/bin/getprop" <<'GP'
#!/bin/sh
case "$1" in debug.sf.enable_hwc_vds) echo 1 ;; ro.surface_flinger.force_hwc_copy_for_virtual_displays) echo true ;; *) echo ;; esac
GP
chmod +x "$T/bin/getprop"
export PATH="$T/bin:$PATH"
echo 'min_refresh_rate=0.0' > "$PHANTOM_DATA/settings.db"
out=$($C hz 60 2>&1)
check "hz 60 blocca minimo e massimo a 60" "grep -qx 'min_refresh_rate=60.0' $PHANTOM_DATA/settings.db && grep -qx 'peak_refresh_rate=60.0' $PHANTOM_DATA/settings.db"
check "hz 60 lo dice e dice come tornare" "echo \"\$out\" | grep -q 'BLOCCATA a 60 Hz' && echo \"\$out\" | grep -q 'hz auto'"
$C hz 120 >/dev/null
check "hz 120 cambia il blocco" "grep -qx 'min_refresh_rate=120.0' $PHANTOM_DATA/settings.db"
check "lo stato mostra il blocco" "$C status | grep -q 'refresh bloccato a 120 Hz'"
$C hz auto >/dev/null
check "hz auto ripristina il valore di prima (0.0)" "grep -qx 'min_refresh_rate=0.0' $PHANTOM_DATA/settings.db"
check "hz auto elimina la chiave che prima non c'era" "! grep -q '^peak_refresh_rate=' $PHANTOM_DATA/settings.db"
check "lo stato non mostra più il blocco" "$C status | grep -q 'refresh automatico'"
$C hz 90 >/dev/null 2>&1; rc=$?
check "hz rifiuta valori sconosciuti" "[ $rc -ne 0 ]"
$C vds gpu >/dev/null
check "vds gpu scrive system.prop con le due proprietà" "grep -qx 'debug.sf.enable_hwc_vds=0' $PHANTOM_MODDIR/system.prop && grep -qx 'ro.surface_flinger.force_hwc_copy_for_virtual_displays=false' $PHANTOM_MODDIR/system.prop"
check "vds status lo mostra" "$C vds status | grep -q 'display virtuali in GPU' && $C vds status | grep -q 'debug.sf.enable_hwc_vds=1'"
check "lo stato mostra display virtuali in GPU" "$C status | grep -q 'display virtuali in GPU'"
$C vds default >/dev/null
check "vds default toglie system.prop" "[ ! -f $PHANTOM_MODDIR/system.prop ]"
echo 'AUTOSTOP=0' >> "$PHANTOM_DATA/probe.conf"
$C restart >/dev/null
check "con AUTOSTOP=0 l'arresto automatico non viene richiesto" "! args | grep -q -- '--tm-autostop' && args | grep -q -- '--telemetry-port 47804'"
sed -i '/^AUTOSTOP=0$/d' "$PHANTOM_DATA/probe.conf"
$C restart >/dev/null

echo "aiuto"
check "senza argomenti stampa l'uso" "$C nonsense 2>&1 | grep -q 'phantomctl scrcpy on'"

$C stop >/dev/null 2>&1
pkill -f "$T" 2>/dev/null
rm -rf "$T"
if [ $FAIL -eq 0 ]; then echo; echo "Tutti i test superati"; else echo; echo "$FAIL test falliti"; exit 1; fi
