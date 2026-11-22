# Stato del lavoro

Ultimo aggiornamento: 2026-11-22. Branch `main`, base `86c1761` (v1.3.2) più la v1.3.3 (patch consegnata).
Roadmap fino alla 2.0.0: documento di progetto «claude/PHANTOM-HANDOFF-ROADMAP.md».
Regole di lavoro in `AGENTS.md`; ripresa in `HANDOFF.md`.

## Ultime release

- **v1.3.3** (2026-11-22): AWDL spento durante lo streaming (scatti periodici in LAN), qualità che torna a 1080p,
  schermo spento con Shizuku/Root su Android 14–17, svuotamento della coda host → taglio del bitrate con tetto,
  obiettivo del bitrate limitato al tetto utile del gradino più alto, niente falso panic `ndk-context` nel log.
- **v1.3.2** (2026-11-22): Shizuku e audio su Android 14–17 (PixelOS Android 17 confermato), proiezione senza
  `SecurityException`, tasti nella pillola iOS.

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
    Kotlin con root on-demand (`su` → `app_process64`/`app_process`) + fallback `MediaProjection`.
  - Aggiunto supporto `x86_64` e `minSdk = 26` per test immediati in modalità Root su **LDPlayer**
    (Network Bridging su LAN Ethernet/powerline) in attesa dello sblocco bootloader sul Redmi.
  - Creata `android/app/` (senza Jetpack Compose/AppCompat, solo framework Android + JNA):
    moduli puri (`ThermalGuard`, `AnnexBFramePrep` con selezione IPv4 LAN per Wi-Fi/Ethernet bridge,
    `HidToAndroidKeyMap`, `TouchEventSynthesizer`), streaming (`VideoEncoder` con profilo default
    **1080p · 120 fps** e fallback 120→60 fps / HEVC→H.264 / H.264@30 compatibile emulatore,
    `PhantomHostSession` su `QuicHostEndpoint` con reset input alla disconnessione), worker root
    (`RootDisplayCapture`, `RootInputInjector`, `RootHostMain`), servizio visibile in notifica
    (`HostForegroundService`), UI (`MainActivity`), 4 suite JUnit JVM e workflow
    `.github/workflows/android-app.yml` (build `arm64-v8a` + `x86_64`).
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

- Fix CI `Android · Phantom Host APK` (su `f58d355`):
  - La compilazione Rust (`arm64-v8a` + `x86_64`) e tutto il codice Kotlin in `android/app/` sono passati, ma `kotlinc` ha segnalato `Conflicting declarations: val message: String` nel file generato `uniffi/phantom_ffi/phantom_ffi.kt` perché le varianti `InvalidAddress`, `ConnectionFailed` e `Transport` di `QuicTransportError` avevano un campo chiamato `message` che collide con `Throwable.message` in Kotlin.
  - Rinominato il campo `message` → `detail` in `core/crates/phantom-ffi/src/lib.rs` (nessun chiamante Swift/Kotlin destruttura il nome del campo) e rimosso `android:extractNativeLibs="true"` ridondante da `AndroidManifest.xml` (già gestito da AGP `useLegacyPackaging = true`).

- Prima prova su LDPlayer reale (`72cf2df`):
  - **Modalità Standard (MediaProjection)**: avviata subito con `libphantom_ffi.so` (`x86_64`), endpoint QUIC in ascolto e certificato DER generato.
  - **Rete LDPlayer**: rilevato `172.16.1.15` (rete virtuale interna LDPlayer) -> timeout QUIC dall'iPhone finché il bridge di rete non assegna un IP sulla stessa sottorete LAN del router.
  - **Modalità Root (`app_process64`)**: `RootDisplayCapture` (`1080×1920`) e `RootInputInjector` (`true`) partiti, seguiti da eccezione non catturata in `RootHostMain` (loggata solo su logcat da `RuntimeInit$KillApplicationHandler`).
  - Fix applicato in locale: preload esplicito di `libjnidispatch.so` e `libphantom_ffi.so` da `libDir` in `RootHostMain`, `UncaughtExceptionHandler` + `try/catch` con stacktrace nel log UI, log di tutte le interfacce IPv4 attive e declassamento di `172.16.1.*` quando esiste un IP LAN reale.

- Primo streaming reale Android ↔ iPhone verificato (`192.168.1.128:47800`, RTT 6,7 ms, video `1088×1920 @ 60 fps H.264`, 407 tocchi e 4 tasti ricevuti in tempo reale in modalità Standard su LDPlayer con Network Bridging attivo).
- Diagnostica `dbb6c13` in modalità Root: il preload di `libjnidispatch.so` e `libphantom_ffi.so` è riuscito senza errori, ma `-Djava.library.path=<libDir>` sovrascriveva `/system/lib64`, facendo fallire `com.android.org.conscrypt.NativeCrypto.get_cipher_names` all'inizializzazione di `SecureRandom()`.
- Fix locale: rimosso `-Djava.library.path` dal comando `app_process64` (mantenendo `/system/lib64:/vendor/lib64` in `LD_LIBRARY_PATH`) e sostituito `SecureRandom` in `PhantomHostSession` con `/dev/urandom`.

- **Modalità Root verificata end-to-end (`f5d0aa3`)**: LDPlayer (`x86_64`, Root + Network Bridging) controllato dal vivo da iPhone (schermo nitido + tocco/tastiera + navigazione Android).
- **Implementazione Fasi 3/4/5 (LAN + 5G/4G remoto, locale non committata)**:
  - Core Rust (`phantom-quic`, `phantom-core`, `phantom-ffi`): identità TLS persistente (`HostIdentity`, `generate_host_identity`, `bind_with_identity`), keep-alive QUIC 3s / idle timeout 15s per reti mobili 4G/5G, risoluzione DNS (`ToSocketAddrs`, IPv4-first) per Tailscale MagicDNS/DDNS, e `PhantomClient::new_multi_codec` (`H264 | H265`).
  - Android (`android/app/`): identità salvata in `HostIdentityStore`, rilevamento automatico LAN + VPN/Tailscale (`100.64.0.0/10` / `tun*`/`wg*`), annuncio mDNS `_phantom._udp.`, codice QR live (`com.google.zxing:core`) con `phantom://pair?...`, campo indirizzo remoto 5G/4G e rotazione display live Portrait ↔ Landscape sulla stessa connessione QUIC (in Root e MediaProjection).
  - iOS (`ios/`): scanner QR nativo (`QRScannerView`), `PairedDeviceStore` 1-tap, scoperta LAN Bonjour (`BonjourDiscovery`), failover automatico LAN → Remoto 5G/4G, riconnessione automatica con backoff, reset tocchi su cambio orientamento video e nuova suite di test `PairingModelsTests.swift`.
  - **Nota verifiche**: non essendoci Docker/Rust/Swift/Gradle attivi in locale su Windows, la compilazione e i test di tutti e 3 i target gireranno su GitHub Actions al push.

- **Pairing QR e build 3-target verificati (`ea6b1da`)**: tutti e 3 i workflow CI verdi al primo colpo; accoppiamento QR verificato dall'utente su dispositivi reali.
- **Trasporto P2P integrato `iroh` + switch automatico Wi-Fi ↔ 5G/4G (locale non committato)**:
  - Nuovo crate `core/crates/phantom-p2p` su `iroh = "1"` (TLS `ring`, UDP hole punching + relay cifrato E2E, `network_change`, chiave Ed25519 persistente) unificato dentro `QuicConnection` di `phantom-ffi`; MSRV workspace CI aggiornato a Rust 1.91 (richiesto da `iroh 1.x`).
  - Android (`android/app/`): chiave P2P salvata in `HostIdentityStore`, `&p2p=<EndpointId>` incluso nel QR code, doppio listener simultaneo LAN QUIC + P2P `iroh` in `PhantomHostSession`, monitor rete `ConnectivityManager` → `NET_CHANGE`.
  - iOS (`ios/`): nuovo `NetworkPathObserver` (`NWPathMonitor`), skip immediato LAN quando su solo 5G/4G per connettersi subito via P2P, migrazione live `p2pEndpoint.networkChange()` e failover rapido su passaggio Wi-Fi ↔ 5G/4G — tutto dentro l'app senza Tailscale né port-forwarding.

- Fix CI su `fea4038`:
  - Tutti i bersagli Rust cross (`aarch64-linux-android`, `aarch64-apple-ios`, `aarch64-apple-ios-sim`, MSRV 1.91, binding Swift) e tutti i sorgenti Swift sono compilati al primo colpo con `iroh 1.x`.
  - Aggiunti `-lresolv -framework SystemConfiguration -framework Security -framework Network` in `ios/project.yml` per i simboli di `n0_dns_resolver`/`netdev` in fase di link iOS.
  - Allineata la formattazione di `connect` in `core/crates/phantom-p2p/src/lib.rs` e fissato il test loopback su `127.0.0.1:<port>` esplicito (`bound_sockets()`).

- Fix crash iOS `SIGABRT` su `2562dd7` (e test `phantom-p2p`):
  - Il crash log su iPhone (`CrashLog`) ha mostrato `tokio::runtime::scheduler::Handle::current` chiamato da `P2pEndpoint::drop`: `tokio::time::timeout(...)` veniva valutato prima di entrare in `self.runtime.block_on(...)`.
  - Avvolti `Drop for P2pEndpoint` e `network_change` in `self.runtime.block_on(async { ... })` e aggiunto `self.runtime.enter()` in `P2pLink::close` e `Drop for P2pLink` (committato in `f851c3a`).

## Sessione 2026-11-20

- Prova su LDPlayer + iPhone del build `f851c3a`: niente più crash su iPhone, ma errore `ConnectionFailed(detail: "No addressing information available")` sia in Wi-Fi sia in 5G; su Android (LDPlayer Root) il log mostrava `Worker root terminato (exit 0)` nello stesso secondo di `Endpoint QUIC LAN pronto sulla porta 47800` (senza `STOPPED`).
- Causa individuata: `P2pHostEndpoint.bind` crea il `DnsResolver` predefinito di `iroh`, che su Android tenta di leggere i DNS di sistema via JNI (`ndk_context`) affidandosi a `std::panic::catch_unwind` per passare ai DNS di fallback; `panic = "abort"` in `core/Cargo.toml` impediva l'unwind e abortiva subito `app_process64` (uccidendo sia la LAN sia il P2P prima della registrazione su `dns.iroh.link`).
- Fix applicato in locale (da committare/pushare):
  - `core/Cargo.toml`: rimosso `panic = "abort"` in `[profile.release]` (default `unwind`, utile anche per il boundary `catch_unwind` di UniFFI).
  - `core/crates/phantom-p2p/src/lib.rs` e `core/crates/phantom-ffi/src/lib.rs`: esposto `is_online()` su `P2pEndpoint` / `P2pHostEndpoint`.
  - `android/app/.../PhantomHostSession.kt`: timeout `online` a 8 s e log esplicito `online` vs `in attesa relay`.
  - `ios/PhantomF0/RemoteSession.swift`: aggregati gli errori di tutti i candidati tentati (`LAN: ... · P2P: ...`).

- Verifica reale su LDPlayer + iPhone dopo il fix `panic = "unwind"`:
  - `iroh` cattura `ndk-context` e va online in 1 s (`P2P integrato (iroh) online`);
  - connessione LAN (Wi-Fi), passaggio Wi-Fi → 5G (~10 s dovuti all'attesa dell'`idle_timeout` della vecchia sessione LAN) e passaggio 5G → Wi-Fi (quasi istantaneo senza caduta della sessione) tutti riusciti.
- Implementazione blocco completo di produzione (locale, da committare e pushare):
  1. **Preemption immediata + switch Wi-Fi → 5G rapido (~1–2 s)**: Android pre-autentica il primo `Hello` della nuova connessione e chiude subito la sessione precedente stale; iOS usa ritardo immediato (50 ms) sul passaggio Wi-Fi → 5G attivo e `onlineTimeoutMs = 2500`.
  2. **Sicurezza P2P + LAN (autenticazione mutua)**: token segreto persistente 128-bit (`auth.token`) generato sull'host e incluso nel QR (`&tok=`), verificato a tempo costante in `phantom-core` / `verify_client_hello` (rifiuto con `Close(UNAUTHORIZED = 2)` senza interrompere sessioni attive).
  3. **Audio in tempo reale Android → iPhone**: `Message::Audio` (AAC-LC 48 kHz stereo in singolo datagramma), `AudioStreamer.kt` (`REMOTE_SUBMIX` / `AudioPlaybackCapture` + `MediaCodec` AAC-LC con fallback pulito su emulatori) e `AudioPlayer.swift` (`AVSampleBufferAudioRenderer` + cookie `esds` ISO 14496-1 + tasto muto nell'HUD).
  4. **Controllo display, Doze e appunti condivisi**: `Message::Control` (`WakeScreen`, `ScreenOff`, `ScreenOn`) con `RootDisplayPower` (`SurfaceControl.setDisplayPowerMode`, ripristino schermo garantito su disconnessione/stop) + `KEYCODE_WAKEUP` automatico alla connessione, pulsante esenzione Doze su Android e `Message::Clipboard` bidirezionale iPhone ↔ Android.
  5. **Documentazione & Design System (`ADR-011`)**: aggiornato `README.md` allo stato reale e creati `docs/design-system.md` + `docs/adr/ADR-011-ui-ux-liquid-glass.md` (look monocromatico Leica/Nothing/Apple Pro, Liquid Glass Light/Dark, rename definitivo a **Phantom**, logo «Ghost Pure» con varianti B/C archiviate, Home pulita iOS, QR modale + Diagnostica collassabile + Quick Settings Tile su Android).
- **Restyling UI/UX completo + Icone ufficiali A2 (locale, da committare e pushare)**:
  - **Brand & Icone**: approvato il logo **A2 (Ghost Pure con occhi minimali a pillola su nero puro)**; generate icona iOS `1024×1024` (`Assets.xcassets/AppIcon.appiconset`), Adaptive + Monochrome Icon Android (`mipmap-anydpi-v26`), icona vettoriale notifica/tile (`ic_phantom_tile.xml`) e temi Light/Dark `Theme.Phantom`; nome app unificato a **Phantom** (`1.0.0`).
  - **iOS (Liquid Glass Light/Dark)**: nuovo `PhantomDesign.swift` (compatibile Xcode 16 / iOS 17+), Home dispositivi pulita in `PhantomHomeView` (`RemoteConnectView.swift`), strumenti Fase 0 spostati in *Impostazioni → Diagnostica* (`DiagnosticsView`), HUD video Liquid Glass collassato di default con pillola trascinabile e fix riavvio `NWPathMonitor` in `NetworkPathObserver.swift`.
  - **Android (Monocromatico Light/Dark + Quick Settings Tile)**: nuovo `HostTheme.kt`, `MainActivity.kt` ridisegnata (Hero Card + metriche compatte + QR modale + controlli segmentati + Doze + Diagnostica collassabile) e nuovo `PhantomTileService.kt` per avvio/arresto 1-tap dalla tendina di sistema.
- **Fix crash worker root LDPlayer + audit stabilità (locale, incluso nello stesso blocco da committare)**:
  - Causa crash su LDPlayer (`Fallback encoder H265...` → `Worker root terminato (exit 0)`): `AudioStreamer.createForRoot()` chiamava `AudioRecord.Builder().setAudioSource(REMOTE_SUBMIX).build()` dentro `app_process64` (dove `ActivityThread.currentOpPackageName()` è `null`), causando un crash nativo in `libandroid_runtime` non catturabile da Java. Aggiunta guardia (`SDK_INT >= 30` + `currentOpPackageName()` valido), `restoreIfTurnedOff()` in `RootDisplayPower`, lock su `rootStdin` in `HostForegroundService`, flush 40 ms di `Close(UNAUTHORIZED)` in `PhantomHostSession` e fix race di `handleDisconnect` su generazioni obsolete in `RemoteSession.swift`.

- **Verifica v1.0 su iPhone + LDPlayer 9 (Android 9, Root)**: confermati streaming 60 fps H.264, `Schermo Off/On` (`ok=true`), switch Wi-Fi ↔ 5G istantaneo nello stesso secondo grazie a preemption autenticata, e nuova UI Liquid Glass Light/Dark su entrambe le app.
- **Rifinitura finale e rilascio v1.0.0 (locale, pronto per commit + tag `v1.0.0`)**:
  - `AudioStreamer.kt`: aggiunto `RootAudioContext` (`AttributionSource` ufficiale Android 12+ / API 31+) per la cattura audio di sistema `REMOTE_SUBMIX` in modalità Root su Android 12+ (Redmi Note 11 Pro 5G), mantenendo solo-video sicuro su Android < 12 (LDPlayer 9 / Android 9).
  - Workflow CI/Release: `iOS · Phantom` (`Phantom.ipa`) e `Android · Phantom APK` (`Phantom.apk`) pubblicano automaticamente le release ufficiali sui tag `v*`; ristretto `android-f0.yml` ai soli percorsi legacy (`android/module/**`, `android/probe/**`, `tools/**`); aggiornato `README.md`.

- **Fix encoder Snapdragon 695 (Redmi Note 11 Pro 5G reale, modalità Standard)**:
  - Alla prima prova sul Redmi reale (schermo `1080×2400`, LAN + P2P `iroh` + QR/auth tutti subito verdi), `MediaCodec.configure` rifiutava `1088×2400` con `IllegalArgumentException` su tutti i tentativi perché l'encoder hardware Qualcomm SM6375 limita il lato lungo a 1920 px.
  - Aggiunti `HostFormatHelpers.fitWithinBounds` (con test JVM) e adattamento automatico a `MediaCodecInfo.VideoCapabilities` (risoluzione, macroblocchi, FPS e bitrate) + fallback progressivo 1920/1600/1280 in `VideoEncoder.kt`, più richiesta automatica di `RECORD_AUDIO` in modalità Standard su Android 10+ per l'audio di sistema.

- **Verifica reale su Redmi Note 11 Pro 5G (Standard senza root)**: streaming video + **audio AAC-LC in tempo reale** confermati sull'hardware reale!
- **Ottimizzazioni fluidità & nitidezza `v1.1.0` (locale, pronte per commit + release `v1.1.0`)**:
  - `VideoEncoder.kt`: `KEY_I_FRAME_INTERVAL = 10`, flag realtime (`KEY_PRIORITY = 0`, `KEY_MAX_B_FRAMES = 0`, `KEY_LATENCY = 1` con fallback), `THREAD_PRIORITY_URGENT_DISPLAY` e polling output a 5 ms.
  - `PhantomHostSession.kt`: bitrate LAN alzato (`12–32 Mbps`, min `3 Mbps`) e P2P 5G (`8–20 Mbps`, min `1.5 Mbps`), keyframe on-demand immediato su `recvThread` (0 ms di ritardo), adattamento bitrate ogni 250 ms (soglia 5%) e priorità `URGENT_DISPLAY`.
  - `RemoteControlSurface.swift`: telemetria live nell'HUD espanso (`risoluzione · codec · Mbps · coda ms`); versioni bumpate a `1.1.0` su iOS e Android.

- **Correzioni lag Redmi `v1.2.0` (locale, da committare)**: analisi del perché il Redmi (Wi-Fi, modalità Standard,
  1080×2400, Snapdragon 695) andasse molto peggio di LDPlayer (cavo, Root, risoluzione bassa) e correzione dei
  cinque colpevoli più i bug trovati:
  1. trasporto QUIC con BBR e coda di invio 512 KiB (prima Cubic e 1 MiB: coda lunga, poi datagrammi scartati);
  2. Wi-Fi lock a bassa latenza sul telefono (prima il risparmio energetico della radio ritardava ping e NACK, il
     controllo del bitrate lo leggeva come congestione e i frame scadevano);
  3. encoder: niente più 1088 stirato, scala esatta, fps scelti dalle prestazioni dichiarate dal SoC;
  4. APK release non debuggable in CI (firma da secret o chiave di debug);
  5. invio dei frame in una sola chiamata FFI (`send_frame_via`) invece di una chiamata JNA per pacchetto;
  - bug: frame scartati dal renderer iOS ora chiedono un keyframe (prima immagine rovinata fino a 10 s), errori di
    invio contati e mostrati nel log, istante di cattura preso dal `presentationTimeUs`.
  Dettagli in `HANDOFF.md` («Uncommitted changes») e `ADR-009`.

- **Prova v1.2.0 sul Redmi**: video migliorato, quasi sincronizzato nel 70–80% dei casi; audio spesso muto o
  gracchiante.
- **Correzioni audio `v1.2.1` (locale, da committare)**: buffer anti-jitter nel core Rust (`phantom_core::jitter`,
  esposto come `AudioJitterBuffer`), player iOS riscritto sopra il buffer, cattura Android a priorità audio con
  istanti dal conteggio dei campioni e senza PCM perso, `AudioSpecificConfig` ripetuto, ogni pacchetto audio spedito
  due volte. Dettagli in `HANDOFF.md`.

- **Prova v1.2.1**: audio buono (qualche micro-lag), video ok; fps mostrati max 60/62 (limite dello Snapdragon 695
  a 1080p: l'encoder scende da 120 a 60 da solo).
- **v1.3.0 (locale, da committare)**: telemetria completa nell'HUD, modalità Shizuku, Accessibilità in modalità
  Standard, tasti di sistema veri, qualità adattiva e intra-refresh, correzione morbida della deriva audio, release
  seria (changelog, controllo versioni, keystore, esportazione log) e pulizia (archivio legacy via script, MSRV).
  Dettagli in `HANDOFF.md` e `CHANGELOG.md`.

## Prossimo passo

1. Committare e pushare la v1.3.0, provare sul Redmi le tre modalità (vedi `HANDOFF.md`), poi tag `v1.3.0` e
   archivio della legacy.
2. Quando il Redmi Note 11 Pro 5G avrà il root, verificare anche la modalità Root (stesso motore di Shizuku).
