# Stato del lavoro

Ultimo aggiornamento: 2026-11-19. Branch `main`, commit `a382c9c` (allineato a `origin/main`) più modifiche locali non committate per la nuova app host Android (`android/app/`, `ADR-010`, CI `android-app.yml`).
Regole di lavoro in `AGENTS.md`; ripresa in `HANDOFF.md`.

## Dove siamo

- **Fase 0 · fattibilità: in corso.** Strumenti e misure parziali; vedi
  `docs/f0-guida.md` e `docs/f0/report-2026-11-01.txt`.
- **Fase 1 · fondamenta: iniziata.** Core Rust con protocollo v0, apertura
  obbligatoria della sessione (`Hello`/`HelloAck`: versione e funzioni comuni,
  senza accordo niente video né input), simulatore di rete, orologi, video,
  controllo del bitrate, tastiera e chiusura ordinata, binding UniFFI e prove;
  vedi `core/README.md` e gli ADR-004…009.
- **App iPhone v0.5.1:** test UDP, video dal vivo (scrcpy e sorgente base),
  banco del decoder hardware, tocco/frequenza, telemetria, scatola nera,
  loopback del core Rust che apre (`Hello`/`HelloAck`) e chiude la sessione.
- **Modulo Android 0.5.2:** cattura, relay scrcpy, telemetria, post-mortem,
  controllo del bitrate, WebUI.
- Test iOS del remoto eseguiti in Docker `swift:5.10`: parser scrcpy, scatola
  nera/telemetria, statistiche del banco e conversioni AVCC ↔ Annex-B tutti
  superati.

## Sessione 2026-11-16

- Trovata una divergenza: 2 commit locali basati su `9657040` contro 31 commit
  remoti (ultimo `e8070ca`, 1 novembre).
- Confrontati i componenti: parser Annex-B, client TCP 47802, decoder, UI e test
  erano già presenti sul remoto in forma più avanzata.
- Lavoro locale conservato in `archive/f0-avsamplebuffer` (`d0ebcb1`, `04361a0`),
  non pushato e non cancellato.
- `main` allineato a `origin/main`; nessuna modifica aggiunta al remoto per non
  duplicare o rompere il codice esistente.
- Aggiunto `tools/repo-preflight.ps1` e la regola di preflight in `AGENTS.md`.
- Scelta l'area con l'utente: core Rust. Creato `PLAN.md` (T1 baseline test,
  T2 fmt/clippy, T3 README UniFFI).
- T3: `README.md` diceva "Mancano i binding UniFFI" mentre `phantom-ffi`, la CI
  `ffi-apple`/`ffi-android` e `CoreLoopback.swift` li usano già; frase corretta.

## Sessione 2026-11-17

- T1: `cargo test --workspace --release` in Docker `rust:1` (toolchain 1.100.0):
  exit 0, 17 blocchi `test result: ok`, 0 `FAILED`.
- T2: `cargo fmt --check` pulito; `cargo clippy -D warnings` era rosso su
  clippy 1.100 e ora è pulito senza warning. Modifiche: ciclo FEC di
  `phantom-video` con `iter().flatten()`, `[lints]` in `uniffi-bindgen` più doc
  di crate, tabella `[lints]` esplicita in `phantom-ffi` (`unsafe_code` allow).
- Suite di T1 ripetuta dopo T2: exit 0, 17 blocchi ok.
- T4: negoziazione `Hello`/`HelloAck` nel core (prima i messaggi c'erano solo su
  filo). `Client` apre e ritenta ogni `hello_retry_us` (250 ms) finché non
  arriva l'ack; `Host` sceglie `min(proto_max, VERSION)`, interseziona le
  funzioni (`caps`) e risponde, oppure chiude con `BAD_PROTOCOL`; entrambi
  espongono `negotiated() -> Option<Negotiated>`. 5 test nuovi, suite completa
  verde, fmt e clippy puliti.
- Test e2e `burst_loss_triggers_keyframe_recovery` riscritto su nove seed con
  soglie sulle medie: il primo `Hello` sposta i pescaggi del generatore del
  simulatore e la vecchia soglia su un singolo seed (84%) era una realizzazione
  fortunata. Misura su 30 seed: con/senza `Hello` 0,809 / 0,823 di consegne,
  0,802 / 0,794 nell'ultimo terzo.
- T5: tastiera nel core. `Client::key(now_us, code, down)` numera e spedisce
  `Message::Key`; `Host::take_key()` restituisce il più vecchio `KeyEvent`.
  Sono eventi (si accodano tutti), la coda è limitata a 32 e i duplicati o i
  riordini si scartano come per il tocco. `HostStats::key_events` conta quelli
  accettati. 4 test.
- T6: chiusura ordinata. `Host::close`/`Client::close` spediscono `Close` e
  portano la sessione in stato terminale (`is_closed()`, `close_reason()`);
  da chiusa non si risponde più e non si manda più niente (video, ping, tocco,
  tastiera, statistiche). Un `Hello` rifiutato per versione non chiude l'host.
  4 test.
- T7: nei binding UniFFI quello che il core ora sa fare:
  `SessionSettings.nonce`, `NegotiatedInfo` + `negotiated()` su host e client,
  `Capability` + `caps_contains`, `KeyInput` + `send_key`/`take_key`, chiusura
  (`is_closed`, `close_reason`, `close`) e `HostCounters.key_events`. 4 test
  nuovi in `phantom-ffi` (8 in tutto).
- Binding verificati **anche in locale**: `cargo build -p phantom-ffi` produce
  `libphantom_ffi.so` su Linux, `uniffi-bindgen` genera Swift e Kotlin, e
  `swiftc` di `swift:5.10` compila ed esegue `swift-smoke/main.swift` contro
  quella libreria: «OK: 362 frame, 600 stati del tocco, 30 tasti, offset orologi
  5000000 µs, bitrate 20 Mbps».
- T8: la prova «tutto sul telefono» usa apertura e chiusura della sessione.
  Nonce casuale di 16 byte, accordo registrato appena arriva l'ack e scritto nel
  rapporto (protocollo, codec, recupero delle perdite, nonce dell'altro lato),
  stop se l'HEVC non è concordato o se la sessione si chiude da sola, `Close` a
  fine prova con verifica che l'host lo registri e che il client non mandi più
  niente. La lettura dell'accordo sta in `ios/PhantomF0/SessionAgreement.swift`,
  provata da `ios/Tests/SessionAgreementTests.swift`: 14 controlli `ok` e
  «Tutti i test superati» nel quinto blocco di `run-tests.sh`.
- T9: trasporto QUIC (fase 2, prima parte). Nuovo crate `phantom-quic` con
  `HostEndpoint`/`ClientEndpoint` sopra `quinn` 0.11 e `rustls` 0.23 (`ring`):
  certificato autofirmato effimero generato dall'host, fissato byte per byte
  dal client, datagrammi non affidabili (RFC 9221), ALPN `phantom/0`, API
  bloccante con runtime Tokio privato (due thread per endpoint). Il core non è
  stato toccato. Prova end-to-end su UDP loopback: negoziazione, 235 frame
  inviati/234 consegnati, 272 stati del tocco all'epoca e 269 dopo il cancello
  di T10, 8 tasti, chiusura ordinata su
  entrambi i lati; certificato sbagliato rifiutato. `max_datagram_size` alla
  MTU iniziale di 1200 è 1162: il test sceglie `frag_size = max − 40`, e
  ADR-004/ADR-009 annotano che i 1200 byte valgono solo dopo la path MTU
  discovery. Versioni bloccate per l'MSRV 1.82 (`quinn 0.11.8`, `quinn-proto
  0.11.12`, `quinn-udp 0.5.13`, `tokio 1.47.1`, `rcgen 0.13.2`, `time 0.3.36`,
  `zeroize 1.8.1`); nessuna dipendenza preesistente aggiornata. CI `cross`
  Android: aggiunti `CC`/`AR`/`RANLIB` dell'NDK perché `ring` compila C anche
  durante `cargo check`.

## Sessione 2026-11-18

- T10: apertura obbligatoria nel core. Finché la sessione non è negoziata
  (`negotiated()` è `None`) l'host non manda video (`send_frame` vuoto,
  `wants_keyframe` falso) e ignora ping, NACK, statistiche, richieste di
  keyframe, tocco e tastiera; il client manda solo `Hello` (niente ping, timer
  del video o statistiche) e scarta video e pong, mentre `touch_tick`, `key` e
  `stats_message` restituiscono `None`. `Close` vale sempre, anche prima
  dell'apertura. Nuovo modulo `gate` con 4 test; aggiornati i test di ping e
  tastiera e i due test FFI che mandavano input prima dell'ack. Suite completa
  → 20 blocchi `test result: ok` (core 30 test, `phantom-ffi` 9), prova QUIC → 3
  test `ok` (269 stati del tocco, 8 tasti), `swift-smoke` → «OK: 362 frame, 600
  stati del tocco, 30 tasti...», `fmt`/`clippy`/`doc`/MSRV 1.82 puliti. T1–T10
  committati dall'utente in `62ba082`.
- Esposto `phantom-quic` nei binding UniFFI (`phantom-ffi`): `QuicHostEndpoint`,
  `QuicClientEndpoint`, `QuicConnection`, `QuicTransportError`, `SessionSettings.frag_size`,
  `video_payload_for_datagram`, `default_quic_server_name`. Aggiornato `ffi-android`
  in `.github/workflows/core.yml` con `CC`/`AR`/`RANLIB` dell'NDK per `ring`.
  Verificati `fmt`, `clippy -D warnings`, `cargo doc -Dwarnings`, `cargo test --workspace --release`
  (12 test in `phantom-ffi`), generazione binding Swift/Kotlin e `swift-smoke`
  (con prova QUIC loopback reale).
- Realizzato il client di controllo remoto iOS su QUIC («stile emulazione»):
  `TouchMapper.swift` (`VideoRect.aspectFit`, `TouchTracker` multi-touch fino a 10 dita,
  `EndpointParser`, `CertificateCodec`), `RemoteInputMap.swift` (`HidKeyCode`,
  `RemoteQuickAction`, `HidCharacterMapper`), `VideoDecoderSupport.swift` (H.264/HEVC),
  `RemoteSession.swift` (QUIC reale + loopback QUIC locale su iPhone),
  `RemoteControlSurface.swift` (superficie full-screen multi-touch, tastiera software/fisica,
  `GCController`, HUD collassabile), `RemoteConnectView.swift`, link in `ContentView.swift`
  e supporto Portrait/Landscape in `ios/project.yml`. `ios/Tests/run-tests.sh`
  (6 blocchi) verde in Docker `swift:5.10`; committato e pushato in `1a97d97` con
  tutti i workflow CI verdi.

## Sessione 2026-11-19

- Prova su iPhone reale del build `1a97d97`: il loopback QUIC si fermava subito con
  «Sessione chiusa: ancora aperta» e spinner sovrapposto all'HUD.
- Causa individuata: `let clientEp` in `RemoteSession.runConnect` usciva di scope a fine
  funzione e `Drop for ClientEndpoint` chiudeva l'endpoint e tutte le connessioni attive;
  `onTick` usava `SessionAgreement.closeText(client.closeReason())` anche per una chiusura
  del solo trasporto QUIC.
- Fix applicati:
  - `phantom-ffi`: `QuicHostEndpoint` e `QuicClientEndpoint` usano `Arc`, e `QuicConnection`
    conserva `_keep_alive: QuicEndpointKeepAlive` dichiarato dopo `link` (quindi `link`
    droppa prima dell'endpoint/runtime). Nuovo test `quic_connection_survives_dropping_endpoints`
    e prova Swift `runQuicEndpointLifetimeSmoke()`.
  - `RemoteSession.swift`: mantenuto `clientEndpoint` fino a `stop()`, bind `127.0.0.1:0` in
    loopback, messaggi distinti per chiusura sessione vs trasporto QUIC, `isIdleTimerDisabled`
    sul main thread, eco visivo dei tocchi ricevuti dall'host nei frame HEVC del loopback e
    `VTCompressionSessionCompleteFrames` a chiusura.
  - `RemoteControlSurface.swift`: spinner mostrato solo con `session.running`, icona avviso
    quando fermo, banner centrato verticalmente senza sovrapposizione all'HUD.
- Nota: il blocco di fix `QuicConnection`/loopback è stato committato e pushato in `cbca9c9`
  e verificato su iPhone reale (110 fps, RTT 0,6 ms, multi-touch centrato nei frame HEVC).
- Rifinitura e ottimizzazione iOS (locale, non committata):
  - `TouchMapper.swift`: `VideoDimensions`, ritorno `Bool` su `TouchTracker.move`/`end`,
    `TouchDispatchState` (con invio garantito di `[]` + ripetizione al rilascio) e `AnnexBScanner`.
  - `VideoDecoderSupport.swift`: `BuiltVideoSample`, conversione diretta Annex-B → `CMBlockBuffer`
    in copia singola e flag `formatChanged`.
  - `RemoteSession.swift`: timer `tick` a 250 Hz, uso `TouchDispatchState`, loopback con sfondo
    luma precalcolato, pacing a scadenza assoluta e adattamento bitrate.
  - `RemoteControlSurface.swift`: renderer `AVSampleBufferVideoRenderer` su coda background,
    niente cerchi fantasma per tocchi fuori video, `RemoteDisplayViewController` con
    `preferredScreenEdgesDeferringSystemGestures = .all`, reset tocchi su `inactive`, pulizia
    completa `GamepadBridge`.
  - `RemoteConnectView.swift`: incolla certificato dagli appunti + indicatore di validità.
  - `RemoteInputTests.swift`: aggiunti test per `TouchTracker.move`/`end`, `TouchDispatchState`
    e `AnnexBScanner`. Committato e verificato in `a382c9c` su iPhone reale.
- Cambio hardware Android e nuova architettura host (`ADR-010`):
  - Dispositivo target aggiornato a **Redmi Note 11 Pro 5G** (Snapdragon 695, `arm64-v8a`),
    abbandonando il modulo Magisk al boot per il server di produzione a favore di una normale APK
    Kotlin con root on-demand (`su` → `app_process`) + fallback `MediaProjection`.
  - Creata `android/app/` (senza Jetpack Compose/AppCompat, solo framework Android + JNA):
    moduli puri (`ThermalGuard`, `AnnexBFramePrep`, `HidToAndroidKeyMap`, `TouchEventSynthesizer`),
    streaming (`VideoEncoder` con profilo default **1080p · 120 fps** e fallback 120→60 fps / HEVC→H.264,
    `PhantomHostSession` su `QuicHostEndpoint`), worker root (`RootDisplayCapture`,
    `RootInputInjector`, `RootHostMain`), servizio visibile in notifica (`HostForegroundService`),
    UI (`MainActivity`), 4 suite JUnit JVM e workflow `.github/workflows/android-app.yml`.
  - Test JVM e compilazione APK in attesa di verifica sulla CI GitHub Actions al push.

## Decisioni

- `origin/main` è l'unica fonte di verità. Prima di lavorare si esegue il
  preflight e si leggono `HANDOFF.md`, `PROGRESS.md` e la documentazione della
  parte toccata.
- Non si scarta lavoro locale: se duplicato o superato, si conserva su un branch
  `archive/...` e si chiede all'utente.
- Non si modificano `VideoClient` né altri percorsi video funzionanti senza un
  telefono per verificare. Riconnessione automatica, AU multi-slice e contatori
  di scarto restano possibili migliorie future, da fare una alla volta.
- Niente POCO disponibile: niente misure sul telefono né test del server Android.
- Solo Windows: niente Xcode/`swiftc`/`gh`; test Swift in Docker `swift:5.10`.
- Gli script bash del repo hanno CRLF nel checkout Windows: in Docker vanno
  eseguiti con `tr -d '\r'`.
- Rust non è installato su Windows: i comandi del core girano in Docker
  `rust:1` con `bash -c` (non `-lc`, che perde `/usr/local/cargo/bin`).
  `core/target/` è in `.gitignore`, quindi gli artefatti non sporcano il tree.
- Non committare né pushare senza richiesta.
- La baseline del core è verde su clippy 1.100 / Rust stabile: qualunque nuova
  modifica deve lasciare `cargo test --workspace --release`,
  `cargo fmt --all -- --check` e `cargo clippy --workspace --all-targets --
  -D warnings` puliti (comandi e criteri in `PLAN.md`).
- `phantom-ffi` non eredita `[lints] workspace = true` (il codice generato da
  UniFFI usa `unsafe`): ha una tabella `[lints]` esplicita equivalente tranne
  `unsafe_code = "allow"`. `uniffi-bindgen` invece eredita i lint del workspace.
- La sessione la apre solo il client (`Hello` con `role = Client`); un `Hello`
  con ruolo host si ignora. Il primo `HelloAck` valido (`proto == VERSION`)
  vince e i duplicati non cambiano nulla; l'host risponde di nuovo a ogni
  `Hello` ripetuto (l'ack può perdersi).
- Le funzioni negoziate sono l'intersezione dei bit di `phantom_proto::caps`;
  `XOR_PARITY` è annunciata dall'host solo con `fec_group > 0`.
- L'apertura è un cancello **obbligatorio**: finché `negotiated()` è `None`
  l'host non manda video (`send_frame` vuoto, `wants_keyframe` falso) e ignora
  ping, NACK, statistiche, richieste di keyframe, tocco e tastiera; il client
  manda solo `Hello` e scarta video e pong, mentre `touch_tick`/`key`/
  `stats_message` restituiscono `None`. Le prime cose utili partono al giro dopo
  l'ack. `Close` vale in qualunque momento, anche prima dell'apertura.
  `SessionConfig::nonce` è a zero finché non arriva la cifratura (fase 5): lo
  riempirà l'app.
- Scartato: generatore casuale separato per verso nel `NetSim`. Isolerebbe il
  video dai pacchetti di controllo, ma cambia la sequenza dei ping e fa sforare
  `phantom-clock::drift_is_estimated_and_corrected` (−378 µs contro 300).
  Non riprovare senza ritarare quel test e spiegare perché.
- La tastiera è fatta di **eventi**, non di stato: `Host::take_key()` consegna
  tutti gli eventi in ordine (premere e rilasciare sono due eventi), la coda è
  limitata a `MAX_PENDING_KEYS = 32` e in caso di pieno si perde il più vecchio.
  Duplicati e riordini si scartano con lo stesso confronto a wrap-around del
  tocco.
- `Close` è uno stato **terminale**: dopo `Host::close`/`Client::close` o dopo
  aver ricevuto `Close`, `is_closed()` è true, `on_datagram` restituisce vuoto,
  `tick`/`touch_tick`/`key`/`stats_message` non mandano nulla e `send_frame`
  restituisce vuoto. Per una nuova sessione l'app crea un nuovo `Host`/`Client`.
  Il motivo 0 è `close_reason::NORMAL`; il rifiuto di `Hello` per versione
  **non** chiude l'host.
- `HostStats` ha il contatore `key_events`; `phantom-ffi` costruisce il proprio
  record leggendo i campi per nome, quindi non va toccato se si aggiungono
  contatori al core.
- Nei binding il `nonce` è `Vec<u8>`/`Data`: più corto di 16 byte si completa con
  zeri, più lungo si tronca, senza errori da gestire nell'app. Le funzioni
  concordate si leggono con `caps_contains(caps, Capability)` (Swift:
  `capsContains(caps:capability:)`) invece che con numeri magici.
- I binding Swift e Kotlin si possono provare su Linux senza Mac:
  `cargo build -p phantom-ffi --release`, poi
  `cargo run -q -p uniffi-bindgen --release -- generate --library
  target/release/libphantom_ffi.so --language swift --out-dir ../dist/swift`
  (idem `--language kotlin --no-format`), e infine `swiftc` di `swift:5.10` su
  `dist/swift/phantom_ffi.swift` + `swift-smoke/main.swift` con
  `-L core/target/release -lphantom_ffi` e `LD_LIBRARY_PATH` per l'esecuzione.
  `dist/` è in `.gitignore`.
- La prova «tutto sul telefono» apre e chiude la sessione come farà l'app con il
  POCO: nonce vero, controllo che l'HEVC sia concordato prima di fidarsi del
  video, stop se la sessione si chiude da sola, `Close` alla fine. La parte pura
  (lettura dell'accordo e testi di chiusura) sta in `SessionAgreement.swift` per
  essere provata su Linux; `CoreLoopback.swift` importa UIKit e si compila solo
  su macOS, quindi in locale si controlla la sintassi con `swiftc -parse`.
- Il trasporto QUIC sta in un crate separato (`phantom-quic`), non nel core: il
  core resta puro e il crate sposta solo datagrammi opachi. QUIC con datagrammi
  (non flussi) per non fermare tocco e tastiera con il video; ALPN `phantom/0`;
  API bloccante con runtime Tokio privato, così l'app non deve conoscere Tokio.
- Il client **fissa** il certificato autofirmato effimero dell'host (confronto
  byte per byte); niente PKI, niente verifica del nome. È il ponte verso
  l'accoppiamento della fase 5: un certificato diverso fa fallire il handshake.
- Alla MTU iniziale di 1200 byte il massimo datagramma applicativo QUIC è 1162:
  il video deve usare `frag_size = max_datagram_size − 40` e non dare per
  scontati i 1200 byte dell'ADR-004. La prova usa 1122 byte di payload; la
  path MTU discovery in LAN porta il massimo oltre 1200.
- Versioni delle dipendenze QUIC bloccate per l'MSRV 1.82: le ultime di
  `quinn`, `quinn-proto`, `quinn-udp`, `tokio`, `rcgen`, `time` e `zeroize`
  richiedono Rust 1.85 o l'edizione 2024. Non aggiornarle senza rifare la prova
  `cargo +1.82 check`.
- `Endpoint::server`/`Endpoint::client`/`Endpoint::connect_with` avviano task
  Tokio: vanno chiamati dentro `runtime.enter()` (oppure dentro `block_on`).
  Chiamarli da fuori fa panico con «there is no reactor running». Anche
  `close` e i `Drop` dei nostri endpoint entrano nel runtime.
- La CI `cross` per Android ora passa `CC`/`AR`/`RANLIB` dell'NDK perché `ring`
  compila C anche con `cargo check`. Il job iOS usa Xcode/`xcrun`.

## Prossimo passo

Modifiche locali pronte (da committare e pushare dall'utente su `origin/main` per
far girare `.github/workflows/android-app.yml` e generare `PhantomHost-debug.apk`):
- `docs/adr/ADR-010-host-android-apk.md`, `.gitignore`
- `android/app/` (nuova app host Kotlin leggera + 4 suite di unit test)
- `.github/workflows/android-app.yml`
- `HANDOFF.md`, `PROGRESS.md`

Dopo il push, scaricare `PhantomHost-debug.apk` dalla release `f0-latest` di GitHub Actions,
installarlo sul Redmi Note 11 Pro 5G e provare lo stream QUIC 1080p · 120 fps verso l'iPhone.
