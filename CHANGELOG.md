# Changelog

Le note di ogni release vengono prese da qui dalla CI (sezione `## [x.y.z]` che corrisponde al tag `vx.y.z`).

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
