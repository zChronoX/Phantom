#!/system/bin/sh
# Phantom F0 · prova di cattura locale: il telefono si collega a se stesso (senza iPhone e senza Wi-Fi) e butta il video.
# La prova si controlla da sola: durante tutta la cattura chiede al window manager di Android se risponde (è lì che si
# ferma il sistema quando "si blocca": system_server tiene il blocco del window manager mentre aspetta SurfaceFlinger),
# guarda se la scatola nera ha segnalato uno stallo e, alla fine, confronta quello che ha scritto la sonda (frame davvero
# ricevuti) prima di dire «non si è bloccato».
#
# Uso: phantom-captest.sh <secondi> <profilo> <comando nc> <auto|manual> <cartella dati> <log della sonda> [descrizione leve]
# Variabili per i test: PHANTOM_WMCMD (comando di controllo, di norma «cmd window size»), PHANTOM_CAPTEST_ALIVE_TIMEOUT, PHANTOM_CAPTEST_PORT,
#                       PHANTOM_CAPTEST_TAIL (secondi di attesa dopo la fine, di norma 3).

secs=$1; prof=$2; NC=$3; activity=${4:-auto}; DATA=$5; LOG=$6; knobs=$7
OUT=$DATA/captest.log
BB=$DATA/blackbox.txt
CHK=$DATA/captest.checks
FLAG=$DATA/captest.frozen
PORT=${PHANTOM_CAPTEST_PORT:-47803}
TAILWAIT=${PHANTOM_CAPTEST_TAIL:-3}
WMCMD=${PHANTOM_WMCMD:-}
ALIVE_T=${PHANTOM_CAPTEST_ALIVE_TIMEOUT:-3}

stamp() { date -u '+%Y-%m-%dT%H:%M:%SZ'; }
say() { echo "$(stamp) $*" >> "$OUT"; }
now() { date +%s; }

if [ -z "$WMCMD" ]; then
    if command -v cmd >/dev/null 2>&1; then WMCMD="cmd window size"; else WMCMD="wm size"; fi
fi
# Il window manager risponde? Se system_server tiene il suo blocco, questa chiamata non torna entro il tempo limite.
alive() { timeout "$ALIVE_T" $WMCMD >/dev/null 2>&1; }

# Un input può restare appeso se il sistema si blocca: ogni chiamata ha un tempo limite.
swipe() { timeout 5 input swipe "$1" "$2" "$3" "$4" "$5" >/dev/null 2>&1; }

say "INIZIO prova locale · profilo $prof · $secs s · $([ "$activity" = auto ] && echo 'scorrimento automatico' || echo 'uso manuale') · ${knobs:-leve di fabbrica}"

if ! alive; then
    say "RISULTATO: NON VALIDA · il window manager non risponde NEMMENO prima di iniziare la cattura (il sistema è già in difficoltà): riavvia il telefono e riprova"
    exit 0
fi

L0=$(wc -l < "$LOG" 2>/dev/null); L0=${L0:-0}
B0=$(wc -l < "$BB" 2>/dev/null); B0=${B0:-0}
t0=$(now)
end=$((t0 + secs))
rm -f "$FLAG"; : > "$CHK"

# nc chiude il collegamento appena il suo ingresso finisce: per tutta la durata deve restare aperto, altrimenti la sessione
# finisce dopo un paio di secondi senza catturare nulla (era il difetto della prova della versione 0.5.1).
( sleep "$secs" | timeout $((secs + 6)) $NC 127.0.0.1 "$PORT" >/dev/null 2>&1 ) &
ncpid=$!

if [ "$activity" = auto ]; then
    if command -v input >/dev/null 2>&1; then
        input keyevent 224 >/dev/null 2>&1 # sveglia lo schermo, se spento
        timeout 8 am start -a android.settings.SETTINGS >/dev/null 2>&1
        sleep 1
    else
        activity=manual
        say "(il comando input non c'è: nessuno scorrimento automatico, usa tu il telefono)"
    fi
fi

# Controllo del window manager in un processo a parte: se uno scorrimento resta appeso, il controllo non si ferma con lui.
(
    consec=0
    while [ "$(now)" -lt "$end" ]; do
        if alive; then
            echo ok >> "$CHK"; consec=0
        else
            echo miss >> "$CHK"; consec=$((consec + 1))
            echo "$(stamp) il window manager non ha risposto entro $ALIVE_T s (di fila: $consec, dopo $(($(now) - t0)) s di cattura)" >> "$OUT"
            if [ "$consec" -ge 2 ]; then echo frozen > "$FLAG"; break; fi
        fi
        sleep 1
    done
) &
monpid=$!

while [ "$(now)" -lt "$end" ] && [ ! -f "$FLAG" ]; do
    if [ "$activity" = auto ]; then
        swipe 540 1800 540 600 220
        swipe 540 700 540 1800 220
    else
        sleep 1
    fi
done

checks=$(grep -c . "$CHK" 2>/dev/null); checks=${checks:-0}
misses=$(grep -c miss "$CHK" 2>/dev/null); misses=${misses:-0}

if [ -f "$FLAG" ]; then
    tf=$(now)
    say "BLOCCO RILEVATO dopo $((tf - t0)) s di cattura. Fermo la cattura per vedere se il sistema si riprende (la scatola nera sta già salvando lo stato)."
    kill "$ncpid" 2>/dev/null
    pkill -f "$NC 127.0.0.1 $PORT" 2>/dev/null
    rec=0; i=0
    while [ "$i" -lt 12 ]; do
        sleep 2; i=$((i + 1))
        if alive; then rec=1; break; fi
    done
    if [ "$rec" = 1 ]; then
        say "RISULTATO: BLOCCO · il sistema si è RIPRESO $(($(now) - tf)) s dopo aver chiuso la cattura: a bloccarlo è la cattura, e fermarla lo sblocca."
    else
        say "RISULTATO: BLOCCO · il sistema NON si è ripreso nemmeno 24 s dopo aver chiuso la cattura: riavvia il telefono (accensione tenuta premuta) e apri il Post-mortem approfondito."
    fi
    exit 0
fi

# Fine regolare: si lascia chiudere nc e alla sonda il tempo di scrivere la riga di fine sessione.
kill "$monpid" 2>/dev/null
wait "$ncpid" 2>/dev/null
sleep "$TAILWAIT"
t1=$(now)
dur=$((t1 - t0 - TAILWAIT))
if [ "$activity" = auto ]; then timeout 5 input keyevent 3 >/dev/null 2>&1; fi # torna alla schermata iniziale

new=$(tail -n +$((L0 + 1)) "$LOG" 2>/dev/null)
conc=$(echo "$new" | grep 'sessione con' | tail -n 1)
rx=$(echo "$conc" | sed -n 's/.*frame ricevuti \([0-9][0-9]*\),.*/\1/p')
mb=$(echo "$conc" | sed -n 's/.* \([0-9][0-9.]*\) MB inviati$/\1/p')
why=$(echo "$conc" | sed -n 's/.*conclusa (\(.*\)) · frame.*/\1/p')
bbstall=$(tail -n +$((B0 + 1)) "$BB" 2>/dev/null | grep -E '^S .*(inizio:|arresto automatico)' | cut -c1-260 | head -n 6)
say "fine della cattura dopo circa $dur s · il window manager ha risposto $((checks - misses)) volte su $checks"
echo "$new" | tail -n 6 | cut -c1-300 >> "$OUT"

if [ -n "$bbstall" ]; then
    echo "$bbstall" >> "$OUT"
    say "RISULTATO: BLOCCO · la scatola nera ha visto uno stallo durante la prova (righe S qui sopra) anche se il controllo ha risposto: i dettagli sono in blackbox.txt e binder-last.txt (Post-mortem approfondito)."
elif [ -z "$conc" ]; then
    say "RISULTATO: NON VALIDA · la sonda non ha registrato nessuna sessione: la cattura non è mai partita, quindi questa prova non dice nulla sul blocco."
elif [ -z "$rx" ] || [ "$rx" -le 0 ]; then
    say "RISULTATO: NON VALIDA · la sessione si è chiusa senza nessun frame (motivo: ${why:-sconosciuto}): il server di cattura non ha prodotto video, quindi questa prova non dice nulla sul blocco."
elif [ "$dur" -lt $((secs - 3)) ]; then
    say "RISULTATO: NON VALIDA · la sessione è finita dopo $dur s invece di $secs (motivo: ${why:-sconosciuto}, $rx frame): troppo corta per fidarsi."
else
    say "RISULTATO: VALIDA · NESSUN BLOCCO: $rx frame catturati in $dur s (${mb:-?} MB), il window manager ha sempre risposto ($((checks - misses)) su $checks), nessuno stallo segnalato."
fi
exit 0
