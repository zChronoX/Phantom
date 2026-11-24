# Changelog

Le note di ogni release vengono prese da qui dalla CI (sezione `## [x.y.z]` che corrisponde al tag `vx.y.z`).

## [1.5.0] - 2026-11-24

### Correzioni
- **Video fermo per un minuto con la rete perfetta**: dopo una raffica di perdite QUIC riporta la dimensione massima
  dei pacchetti a 1200 byte per 60 s, ma il video continuava a essere spezzato in pezzi da ~1400 byte decisi
  all'inizio della sessione, e ogni pezzo veniva rifiutato. Ora la dimensione dei pezzi segue il massimo attuale,
  frame per frame. Nel log gli invii rifiutati per questo motivo sono contati a parte («troppo grandi»).
- **Meno scatti per i blocchi brevi del Wi-Fi**: in LAN la coda di invio assorbe fino a ~600 ms di video invece di
  ~250 ms (coda QUIC da 2 MiB invece di 512 KiB). Un blocco della radio di qualche centinaio di ms non butta più via
  i frame e non costringe a un keyframe: finito il blocco, il Wi-Fi smaltisce l'arretrato in pochi millisecondi. In
  P2P la soglia resta di ~250 ms.
- **Qualità più stabile**: dopo uno stallo il bitrate scende del 15% invece del 25%; la risoluzione scende dopo 6 s
  di rete scarsa (prima 3 s) e risale dopo 20 s di rete buona (prima 8 s), e non cambia mai più di una volta ogni 30
  s. Prima si passava da 720p a 1080p e ritorno anche 11 volte in 45 minuti, con un piccolo fermo a ogni cambio.
- **Niente più risalite a vuoto**: se dopo una salita a 1080p bisogna riscendere entro 2 minuti, il tentativo
  successivo aspetta 2 minuti di rete buona, poi 4, 8, fino a 10; restare 5 minuti sul gradino raggiunto cancella la
  penalità. Nei test del 24/11 la risoluzione era cambiata 21 volte in un'ora, sempre a minuti di distanza. Il log
  dice quanto si aspetterà prima della prossima salita.
- **Parità adattiva**: il pacchetto di parità (che ricostruisce un pezzo perso senza aspettare la ritrasmissione)
  passa da 1 ogni 10 a 1 ogni 5 o 3 quando il client riporta perdite sparse con la coda bassa (5G, relay), e torna a
  1 ogni 10 dopo 10 s di rete pulita o subito alla prima congestione. Sul simulatore, con il 4% di pacchetti persi:
  83% di frame consegnati invece di 63%, un terzo di keyframe in meno.
- **Log più pulito**: le connessioni P2P in arrivo con il primo pacchetto illeggibile (rumore di rete, innocuo)
  finivano nel log una per una, 52 in un giorno; ora c'è un riepilogo all'ora.

### Background e avvio
- **Silenzio in background**: uscendo dall'app l'audio di Android tace (prima si sentiva finché non si toglieva con
  «Audio off»). La sessione resta collegata; al ritorno l'audio riprende come lo si era lasciato. Chi vuole
  continuare ad ascoltare attiva «Audio in background» nelle impostazioni dell'iPhone.
- **Video in pausa in background**: l'iPhone chiede al telefono Android di smettere di mandare il video e di
  sospendere l'encoder finché l'app non torna in primo piano; al ritorno il video riparte subito da un keyframe.
  Prima il video continuava ad arrivare e veniva buttato: su 5G anche ~1 GB all'ora. La pausa si rinnova ogni
  secondo e scade da sola dopo 3 s, quindi non resta mai appesa.
- **Avvio automatico dell'host** (opzione nell'app Android): all'accensione e dopo un aggiornamento dell'app l'host
  parte da solo in modalità Root, e in Shizuku se Shizuku è già attivo (lo aspetta fino a 45 s). Se non può partire
  da solo, una notifica apre l'app.
- **Limiti di qualità dell'encoder** (Android 12+): QP tra 12 e 45, per non sprecare bit su una schermata ferma e
  non scendere a blocchi grossi nelle scene complesse.

### P2P
- **Ritorno automatico in LAN**: in P2P l'iPhone riprova ogni tanto l'indirizzo di casa e, se risponde, si ricollega
  in LAN (prima ci si restava, con qualità peggiore). Se il ritorno fallisce, riprova più di rado (fino a 5 minuti).
- **Diretto o relay nell'HUD**: accanto a «P2P» compare se il percorso è diretto o passa dal relay.
- Il log dell'host registra ogni svuotamento della coda e i frame saltati, anche quando non ci sono pacchetti
  scartati: prima un crollo della qualità in P2P non lasciava traccia.

### iPhone
- **Diagnosi in un tocco**: dal pannello della HUD, «Diagnosi» prepara un file con iPhone e versione, telemetria, il
  diario della sessione (connessioni, errori, cambi di rete, background), l'ultima chiusura inattesa e il **log del
  telefono Android**, chiesto all'host durante la sessione. Si condivide con il foglio di sistema (Messaggi, Mail,
  File…).
- **Pannello della HUD compatto**: alto quanto il contenuto (prima occupava quasi tutto lo schermo), griglia senza
  tasti isolati, telemetria più fitta; il lato della HUD si cambia dalla griglia e «Termina la sessione» ha una riga
  sua.

### Protocollo
- Nuovo messaggio `HostLog` (log dell'host a pezzi) e comando `SendLog`, negoziati con la funzione `HOST_LOG`: con
  versioni precedenti la diagnosi funziona lo stesso, senza il log di Android.
- Nuovi comandi `PauseVideo`/`ResumeVideo`, negoziati con la funzione `VIDEO_PAUSE`; con un host più vecchio
  l'iPhone non li manda e il video in background viene scartato come prima.
- La dimensione del gruppo di parità può cambiare da un frame all'altro (il ricevitore la legge già da ogni pacchetto).

### Documentazione
- README riscritto: funzioni, architettura con schema, badge, requisiti, installazione, modalità, uso, prestazioni,
  sviluppo, CI e release, risoluzione dei problemi, privacy e crediti.

## [1.4.0] - 2026-11-23

### Raggiungibilità dell'host
- **Una connessione andata male non spegne più l'host**: se un handshake in arrivo falliva (es. «authentication
  failed» dopo un blackout o un cambio di rete) il P2P, e allo stesso modo la LAN, smettevano di accettare connessioni
  fino al riavvio dell'host. Ora l'errore riguarda solo quella connessione.
- **P2P da fuori casa più affidabile**: l'handshake di una connessione in arrivo aveva solo 400 ms per completarsi;
  via relay o con il 5G spesso non bastavano e la connessione veniva abbandonata. Ora ha 10 s.
- **P2P che si ripara da solo**: ogni 15 s l'host controlla di essere collegato al relay. Se non lo è, lo scrive nel
  log, chiede a iroh di ricontrollare la rete e dopo 3 minuti riapre l'endpoint (stesso ID, nessuna nuova
  associazione). Se il P2P non parte, riprova con attese crescenti invece di arrendersi.
- **Annuncio in LAN rifatto dopo un cambio di rete** (Wi-Fi che torna dopo un blackout, router riavviato), così
  l'iPhone ritrova il telefono a casa.
- Con Shizuku/Root: l'app viene esclusa dal risparmio batteria (Doze) e si prova a forzare il Wi-Fi a bassa latenza
  (risparmio energetico della radio spento). Su alcune ROM il secondo comando richiede root: l'esito è nel log.

### iPhone
- **Nuova HUD laterale**: una linguetta fissa a metà schermo sul bordo, con stato, FPS e Indietro/Home/Recenti sempre a
  portata. Toccandola o trascinandola verso il centro si apre un pannello con tutti i controlli e la telemetria; si
  chiude toccando fuori, trascinandolo verso il bordo o con la freccia. Il lato (destra o sinistra) si sceglie dal
  pannello e viene ricordato.
- **La sessione resta aperta in background**: aprendo un'altra app o bloccando lo schermo l'audio continua e la
  connessione resta attiva; il video si ferma e riparte da un keyframe al ritorno. Prima la sessione cadeva e si
  riconnetteva ogni volta.
- **Niente più falso «l'app si è chiusa all'improvviso»** dopo averla chiusa dalle app recenti.

### Diagnosi
- La riga «Rete in affanno» riporta anche le statistiche QUIC degli ultimi 5 s (RTT, finestra di congestione,
  pacchetti persi, eventi di congestione, percorso via relay): servono a capire se gli stalli vengono dal Wi-Fi o dal
  controllo di congestione.

## [1.3.3] - 2026-11-22

### Correzioni
- **Scatti periodici sull'iPhone**: la ricerca Bonjour accendeva AWDL (la radio peer-to-peer di AirDrop) e restava
  attiva per tutta la sessione, perché la schermata di streaming non chiude la Home. Con AWDL il Wi-Fi dell'iPhone
  salta di canale a intervalli regolari: picchi di latenza, coda di invio piena sull'host e un keyframe a ogni
  svuotamento (nei test della 1.3.2: 1902 pacchetti scartati e 138 keyframe in pochi minuti in LAN). Ora la ricerca
  usa solo la LAN e si ferma quando parte lo streaming.
- **La qualità torna a 1080p**: una volta scesa a 720p non risaliva più, perché la qualità adattiva vedeva il
  bitrate già limitato al tetto utile di 720p. Ora vede quello concesso dalla rete; l'encoder resta limitato.
- **Schermo spento da remoto con Shizuku/Root su Android 14–17** (prima «ok=false»): i display fisici si spengono
  come fa scrcpy, caricando `DisplayControl` con la sua libreria di sistema; `requestDisplayPower` resta solo come
  ultimo ripiego. Se non riesce, il log spiega perché.
- Il log dell'host non mostra più il falso errore «thread 'phantom-p2p' panicked … android context was not
  initialized»: era un tentativo di leggere i DNS di sistema, già gestito con un ripiego.

### Prestazioni
- Quando la coda di invio dell'host si deve svuotare, il bitrate scende subito e si fissa un tetto che si rialza piano:
  uno stallo ricorrente della rete porta il bitrate a un livello che regge, invece di un keyframe ogni pochi secondi.
- L'obiettivo del controllo di rete non sale più oltre il bitrate utile della risoluzione massima (prima arrivava a
  32 Mbit/s con l'encoder fermo a 15), così le riduzioni hanno effetto immediato.

## [1.3.2] - 2026-11-22

### Correzioni
- **Shizuku e Root su Android 14–17** (es. PixelOS Android 17): l'host non partiva per
  `NoSuchMethodException: DisplayControl.createDisplay`. La cattura dello schermo usa ora
  `DisplayManager.createVirtualDisplay` (la stessa tecnica di scrcpy), con ripiego sul metodo precedente per Android
  10–13. Il worker prepara un ambiente «da app» minimo (contesto `com.android.shell`) richiesto da Android recenti.
- **Audio con Shizuku su Android 17**: risolta la `NullPointerException` («Audio non disponibile… solo video»). Se il
  sistema rifiuta comunque la creazione standard, l'audio viene aperto per via diretta come fa scrcpy.
- **Modalità Standard/Accessibilità che si staccava** quando la qualità si adattava alla rete o il telefono ruotava
  (`SecurityException: Don't re-use the resultData`): da Android 14 la proiezione ammette un solo display virtuale,
  che ora viene creato una volta e poi solo ridimensionato.
- Schermo spento da remoto su Android 15+ con l'API di sistema `requestDisplayPower`.
- Il log «Qualità adattata alla rete» mostra il bitrate con un decimale (prima «0 Mbps» sotto 1 Mbit/s).

### Novità
- **iPhone: Indietro, Home e Recenti nella pillola compatta** (quella con solo gli FPS), con un leggero feedback
  aptico: non serve più tenere aperta tutta la HUD per navigare quando le gesture dell'iPhone chiuderebbero Phantom.

### Prestazioni
- Encoder Qualcomm (Snapdragon): attivata la modalità a bassa latenza del produttore; il log indica l'encoder usato.
- iPhone: più tempo alla connessione LAN prima di passare al P2P (2–2,5 s invece di 1,3–1,6 s) e indirizzo LAN
  salvato aggiornato quando Bonjour trova il telefono su un IP nuovo, così a casa non si finisce sul P2P (più lento).

## [1.3.1] - 2026-11-21

### Correzioni
- **Risoluzione**: su schermi verticali l'encoder applicava i suoi limiti «orizzontali» (es. 1920×1088) e il video
  usciva a 488×1088. Ora i limiti valgono per lato lungo/corto: sul Redmi Note 11 Pro 5G si passa a 864×1920 o più.
- **Niente più tempeste di keyframe**: quando la rete non regge, l'host salta i frame finché la coda di invio si
  svuota e riparte da un solo keyframe (prima ne chiedeva uno per ogni pacchetto perso: centinaia in pochi minuti in
  P2P, con RTT oltre il secondo).
- **Bitrate legato alla risoluzione**: al massimo ~0,15 bit per pixel in HEVC (≈15 Mbit/s a 864×1920 60 fps) invece
  di 32 Mbit/s fissi, che saturavano il Wi-Fi senza migliorare l'immagine.
- P2P: bitrate minimo 0,6 Mbit/s (era 1,5) per restare fluidi anche su relay lenti.
- HUD: nuovo contatore «saltati» (frame non spediti per svuotare la coda).

## [1.3.0] - 2026-11-20

### Novità
- **Modalità Shizuku**: controllo completo del telefono senza root (tocco multi-dito, tastiera, schermo spento,
  audio di sistema su Android 12+). Shizuku si avvia dal telefono con il Debug wireless.
- **Controllo con Accessibilità** in modalità Standard: tap, swipe, pinch, tasti di sistema e testo nei campi, senza
  root né Shizuku (i gesti partono quando si alza il dito).
- **Tasti di sistema** nell'HUD dell'iPhone: Indietro, Home, Recenti, Notifiche, Screenshot, Blocca, Volume, come
  comandi veri (non più scorciatoie da tastiera).
- **Telemetria nell'HUD**: fps dell'encoder e ricevuti, latenza dalla cattura (mediana e p95), tempo di codifica,
  RTT, coda, bitrate reale e obiettivo, coda di invio dell'host, perdite, keyframe, stato dell'audio e stato termico.
- **Qualità adattiva**: con poca banda si scende a 720p o 540p invece di sgranare il 1080p, e si risale da soli.
- **Intra-refresh** opzionale (sperimentale) per evitare i picchi dei keyframe.
- Esportazione dei log dall'app Android (Diagnostica → Esporta log).

### Miglioramenti
- Audio: la deriva tra gli orologi di Android e iPhone si corregge variando impercettibilmente la velocità invece di
  saltare.

## [1.2.1] - 2026-11-20

### Correzioni
- Audio continuo: buffer anti-jitter nel core, cattura Android a priorità audio con istanti dal conteggio dei
  campioni, `AudioSpecificConfig` ripetuto, ogni pacchetto audio spedito due volte.

## [1.2.0] - 2026-11-20

### Miglioramenti
- Trasporto QUIC con BBR e coda di invio 512 KiB, Wi-Fi a bassa latenza sul telefono, encoder adattato al SoC
  (risoluzione esatta, fps realistici), APK release non debuggable, invio dei frame in una sola chiamata FFI.

### Correzioni
- iPhone: un frame scartato dal decoder chiede subito un keyframe invece di lasciare l'immagine rovinata.
