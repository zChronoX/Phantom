# Handoff

Scritto: 2026-11-20; aggiornato con le correzioni **v1.2.0** per i lag sul Redmi Note 11 Pro 5G reale (trasporto BBR, Wi-Fi lock, encoder adattato al SoC, build release, invio frame in una chiamata FFI, keyframe su frame scartati in iOS).
Branch: main - base `0768f02` (= `origin/main`) + modifiche v1.2.0 non committate (vedi «Uncommitted changes»).
Archivio locale: `archive/f0-avsamplebuffer` - commit 04361a0

## Goal

Continuare Phantom dal vero stato del repository. Fase 0 iOS avanzata sul
remoto, fase 1 (core Rust) in corso. Sintesi in `README.md`, stato in
`PROGRESS.md`, lavoro fatto e criteri in `PLAN.md`.

## Done and verified

Otto task della sessione precedente più T9 e T10 chiusi e verificati in Docker
`rust:1` (toolchain 1.100.0) e, per i binding e l'app, anche con `swiftc` di
`swift:5.10`:

- T1: `cargo test --workspace --release` → exit 0, 17 blocchi
  `test result: ok`, 0 `FAILED`.
- T2: `fmt --check` → 0; `clippy -D warnings` → 0 senza warning (ciclo FEC di
  `phantom-video` con `iter().flatten()`, `[lints]` in `uniffi-bindgen` e
  `phantom-ffi`).
- T3: `README.md` non dice più «Mancano i binding UniFFI».
- T4: negoziazione `Hello`/`HelloAck` + `caps`/`close_reason` (5 test).
- T5: tastiera — `Client::key`, `Host::take_key`, coda limitata a 32 (4 test).
- T6: chiusura ordinata — `Host::close`/`Client::close`, stato terminale
  (`is_closed`, `close_reason`), nessun invio dopo la chiusura (4 test).
- T7: binding UniFFI — `SessionSettings.nonce`, `NegotiatedInfo` +
  `negotiated()`, `Capability` + `caps_contains`, `KeyInput` +
  `send_key`/`take_key`, `is_closed`/`close_reason`/`close`,
  `HostCounters.key_events` (4 test nuovi, 8 in `phantom-ffi`; 9 dopo T10).
- T8: app iPhone — la prova «tutto sul telefono» annuncia un nonce vero,
  registra e stampa l'accordo (`SessionAgreement.swift`), si ferma se l'HEVC non
  è concordato o se la sessione si chiude, e chiude con `Close` verificando che
  l'host lo registri e che il client taccia (14 controlli in `ios/Tests`).
- Test e2e `burst_loss_triggers_keyframe_recovery` su nove seed con soglie
  sulle medie (su 30 seed con/senza `Hello`: 0,809/0,823 consegne, 0,802/0,794
  nell'ultimo terzo).
- Binding Swift e Kotlin generati da `libphantom_ffi.so`; il programma
  `swift-smoke/main.swift` compila ed esegue su Linux: «OK: 362 frame, 600 stati
  del tocco, 30 tasti, offset orologi 5000000 µs, bitrate 20 Mbps».
- `ios/Tests/run-tests.sh` (Docker `swift:5.10`) → cinque blocchi, tutti i test
  superati; il nuovo blocco «sessione (apertura e chiusura)» ha 14 controlli.
- `swiftc -parse` su `CoreLoopback.swift`, `CoreLoopbackView.swift` e
  `SessionAgreement.swift` → exit 0 (solo sintassi: UIKit su Linux non c'è).
- T9: nuovo crate `phantom-quic` (fase 2, prima parte). `quinn 0.11.8` +
  `rustls 0.23`/`ring` + runtime Tokio privato, certificato autofirmato
  effimero generato dall'host e fissato dal client, datagrammi RFC 9221, ALPN
  `phantom/0`, API bloccante (`HostEndpoint`, `ClientEndpoint`, `QuicLink`).
  `cargo test -p phantom-quic --release` → 3 test `ok`: sessione completa su
  UDP loopback (235 frame inviati/234 consegnati, 272 stati del tocco all'epoca,
  269 dopo il cancello di T10, 8 tasti, chiusura su entrambi i lati,
  `max_datagram_size` 1162) e certificato sbagliato rifiutato. Suite intera → 20 blocchi `test result: ok`, exit 0;
  `fmt`, `clippy -D warnings`, `cargo doc -D warnings` e `cargo +1.82 check`
  puliti. Versioni bloccate per l'MSRV (`quinn-proto 0.11.12`,
  `quinn-udp 0.5.13`, `tokio 1.47.1`, `rcgen 0.13.2`, `time 0.3.36`,
  `zeroize 1.8.1`). CI `cross` Android: `CC`/`AR`/`RANLIB` dell'NDK per il C di
  `ring`.
- T10: apertura obbligatoria nel core. Finché `negotiated()` è `None` l'host non
  manda video (`send_frame` vuoto, `wants_keyframe` falso) e ignora ping, NACK,
  statistiche, richieste di keyframe, tocco e tastiera; il client manda solo
  `Hello` e scarta video e pong, mentre `touch_tick`/`key`/`stats_message`
  restituiscono `None`. `Close` vale sempre. Nuovo modulo `gate` (4 test), test
  di ping/tastiera e due test FFI aggiornati, `swift-smoke` manda il primo tasto
  dopo l'ack. Suite completa → 20 blocchi `test result: ok` (core 30 test, FFI
  9), prova QUIC → 3 test `ok` (269 stati del tocco, 8 tasti), `swift-smoke` →
  «OK: 362 frame, 600 stati del tocco, 30 tasti...», `fmt`/`clippy`/`doc`/MSRV
  1.82 puliti.
- `fmt`, `clippy` e suite completa ripetuti dopo l'ultima modifica: verdi.

## In progress (exact state)

- Le rifiniture e ottimizzazioni iOS sono state committate e verificate in `a382c9c` (funzionanti su iPhone).
- Nuovo bersaglio Android: **Redmi Note 11 Pro 5G** (Snapdragon 695, `arm64-v8a`), sostituisce il POCO F4 GT dopo il guasto al display.
- Creata la nuova app host Android leggera come APK normale con root on-demand (`android/app/`, `ADR-010`), senza modulo Magisk/SukiSU e senza toccare `android/module/`/`android/probe/`:
  - Gradle snello (`minSdk = 26`, ABI `arm64-v8a` + `x86_64` per supportare sia Redmi Note 11 Pro 5G sia LDPlayer 9 su PC; zero Compose/AppCompat/Material, solo Android SDK + JNA + `uniffi.phantom_ffi`, `extractNativeLibs="true"`).
  - Moduli puri con 4 suite di test JUnit JVM (`ThermalGuard`, `CodecConfigCache`/`HostFormatHelpers` inclusa selezione IPv4 LAN per Wi-Fi ed Ethernet bridge, `HidToAndroidKeyMap`/`KeyModifierState`, `TouchEventSynthesizer`).
  - Motore `VideoEncoder` (default **1080p · 120 fps**, parametri neutri + fallback automatico 120 → 60 fps, HEVC → H.264 e H.264/30 compatibile emulatore) + `PhantomHostSession` su `QuicHostEndpoint` (con reset input su disconnessione client) + protezione termica attiva (`ThermalGuard`).
  - Modalità **Root on-demand** (`su` → `app_process64`/`app_process` temporaneo su `RootHostMain` con `RootDisplayCapture` + `RootInputInjector`) e modalità **MediaProjection** senza root.
  - `HostForegroundService` (notifica persistente stile FKM con tasto *Ferma*, monitor batteria + `PowerManager.currentThermalStatus`) e `MainActivity`.
  - Nuovo workflow CI `.github/workflows/android-app.yml` che genera binding Kotlin + `libphantom_ffi.so` (`arm64-v8a` e `x86_64`), esegue `testDebugUnitTest` e pubblica `PhantomHost-debug.apk` nella release `f0-latest`.
- **Verifiche locali non eseguite**: su questo PC Windows non ci sono Java/Android SDK/Gradle in locale; i test unitari JVM e la build dell'`.apk` gireranno su GitHub Actions al push.

## Next steps (in order)

1. Eseguire `tools/repo-preflight.ps1`, rivedere il diff v1.2.0, committare e pushare (CI: `core.yml`, `android-app.yml`, `ios.yml`).
2. (Facoltativo ma consigliato) Creare il keystore di release e i secret GitHub `PHANTOM_KEYSTORE_BASE64`,
   `PHANTOM_KEYSTORE_PASSWORD`, `PHANTOM_KEY_ALIAS`, `PHANTOM_KEY_PASSWORD`: senza, l'APK è una release non debuggable
   firmata con la chiave di debug della run (va disinstallata la versione precedente a ogni build).
3. Disinstallare l'APK debug dal Redmi, installare il nuovo `Phantom.apk` (release) e provare in modalità Standard,
   guardando nell'HUD iOS espanso Mbps, coda ms e risoluzione, e nel log Android le righe «Risoluzione adattata»,
   «Frequenza adattata», «Wi-Fi a bassa latenza attivo» e «Rete in affanno».
4. Fase successiva concordata con l'utente: controllo senza root (Shizuku/ADB wireless), adattamento di risoluzione e
   fps, tasti di sistema nell'app iOS, telemetria estesa, release seria (firma, versioni, note).

## Decisions and constraints from the user

- **Redmi Note 11 Pro 5G** come nuovo telefono Android al posto del POCO F4 GT.
- **APK normale con root on-demand** (niente più modulo Magisk al boot, notifica persistente stile FKM, nessuna modifica di sistema, protezione termica attiva); stream predefinito a **1080p · 120 fps** con app leggera e snella.
- Solo Windows in locale: niente `cargo`/`rustc`, Xcode, `swiftc`, Java, Android
  SDK, `gh`. Docker funziona: `rust:1` per il core, `swift:5.10` per Swift.
- Non pushare e non cancellare branch o lavoro locale senza richiesta.
  `archive/f0-avsamplebuffer` è l'unica copia dei due commit locali.
- `origin/main` è la fonte di verità; preflight obbligatorio prima di ogni
  modifica.

## Do not retry

- Riprendere il vecchio piano iOS in 5 passi: basato su `9657040`, superato dal
  remoto.
- Rebasare o pushare i due commit archiviati: duplicano codice già presente e
  confliggono su `ContentView.swift` e `codemagic.yaml`.
- `docker run ... rust:1 bash -lc`: la shell di login perde
  `/usr/local/cargo/bin`. Usare `bash -c`.
- In PowerShell, `$?` dentro stringhe con doppi apici viene espanso da
  PowerShell (`EXIT=True`) e non arriva a bash: usare apici singoli.
- Generatore casuale separato per verso nel `NetSim`: fa sforare
  `phantom-clock::drift_is_estimated_and_corrected` (−378 µs contro 300).
  Provato e revertito.
- Tornare a soglie strette su un solo seed nei test e2e con raffiche: la
  dispersione va da 0,69 a 0,92 di frame consegnati.
- `ios/Tests/run-tests.sh` direttamente in Docker: il checkout Windows ha CRLF,
  serve `tr -d '\r' < run-tests.sh | bash -s`.
- Cercare il telefono su `192.168.1.118:47801` o `adb`: non è disponibile.
- Compilare `ios/PhantomF0/CoreLoopback.swift` su Linux o Windows: importa
  UIKit, AVFoundation e VideoToolbox. Al massimo `swiftc -parse` (sintassi);
  il typecheck dell'app lo fa la CI macOS.
- Installare Rust, Swift o l'SDK Android sul PC: usare Docker.
- Chiamare `Endpoint::server`, `Endpoint::client` o `Endpoint::connect_with`
  fuori dal contesto del runtime Tokio: `tokio::spawn` va in panico («there is
  no reactor running»). Usare `runtime.enter()` o `block_on`; i nostri `close`
  e `Drop` entrano nel runtime.
- Dare per scontati datagrammi da 1200 byte su QUIC alla prima connessione: con
  MTU iniziale 1200 il massimo è 1162. Leggere `max_datagram_size()` e scegliere
  `frag_size = max − 40`.
- Aggiornare `quinn`, `quinn-proto`, `quinn-udp`, `tokio`, `rcgen`, `time` o
  `zeroize` senza rifare `cargo +1.82 check`: le ultime versioni richiedono
  Rust 1.85 o l'edizione 2024.
- Togliere `CC`/`AR`/`RANLIB` dell'NDK dal job `cross` Android: `ring` compila C
  anche con `cargo check`.
- Passare `tokio::time::timeout(...)` direttamente a `runtime.block_on(...)` senza `async { ... }`: `timeout()` chiama `Handle::current()` prima che `block_on` entri nel runtime Tokio e va in panico (`SIGABRT` su iOS/Android). Avvolgere sempre in `runtime.block_on(async { tokio::time::timeout(...).await })`.
- Passare `-Djava.library.path=<libDir>` ad `app_process64`: sovrascrive `/system/lib64` e rompe le librerie JNI di sistema come `Conscrypt`/`NativeCrypto` (usato da `SecureRandom`). Caricare invece `libjnidispatch.so`/`libphantom_ffi.so` via `System.load(path)` + `-Djna.*` e preservare `/system/lib64:/vendor/lib64` in `LD_LIBRARY_PATH`.
- Usare un campo chiamato `message` nelle varianti di un enum `#[derive(uniffi::Error)]` in Rust: nel binding Kotlin generato da UniFFI collide con `Throwable.message` (`Conflicting declarations: val message: String`). Usare nomi come `detail`.
- Impostare `panic = "abort"` in `[profile.release]` di `core/Cargo.toml`: rompe sia il `catch_unwind` interno del `DnsResolver` di `iroh` su Android (fallback quando `ndk_context` non è inizializzato, causando `SIGABRT` immediato nel worker root `app_process64`) sia il boundary `catch_unwind` di UniFFI.
- Chiamare `AudioRecord.Builder().setAudioSource(REMOTE_SUBMIX).build()` dentro il worker root standalone `app_process64` senza `ActivityThread`/`Context` (soprattutto su Android < 11 come LDPlayer 9): `ActivityThread.currentOpPackageName()` è `null` e `android_media_AudioRecord_setup` in `libandroid_runtime` provoca un crash nativo immediato (`Worker root terminato (exit 0)` senza eccezione Java). Verificare sempre `Build.VERSION.SDK_INT >= Build.VERSION_CODES.R` e `currentOpPackageName()` non nullo prima di inizializzare `AudioRecord` in modalità Root.
- Mandare video, ping, tocco, tastiera o statistiche prima dell'apertura: il
  core ora le scarta (cancello T10). L'host deve aspettare `negotiated()` e il
  client l'ack; i test di ping e tastiera aprono prima la sessione. Non aggirare
  il cancello con un `SessionConfig` o un percorso separato: è voluto.

- Portare `DATAGRAM_SEND_BUFFER` sotto la dimensione di un keyframe (≈ 300–500 KB a 1080p ad alto bitrate): QUIC
  scarterebbe l'inizio del keyframe appena spedito e il client ne chiederebbe un altro all'infinito.
- Arrotondare per eccesso le dimensioni dell'encoder (`align16(1080) = 1088`): l'immagine si stira e si sfoca.

## How to verify

Core:

```
docker run --rm -v "C:\Users\Giovanni\Documents\Phantom\Phantom:/src" -w /src/core rust:1 bash -c 'cargo fmt --all -- --check; echo "fmt_exit=$?"; cargo clippy --workspace --all-targets -- -D warnings; echo "clippy_exit=$?"'
docker run --rm -v "C:\Users\Giovanni\Documents\Phantom\Phantom:/src" -w /src/core rust:1 bash -c 'cargo test --workspace --release; echo "test_exit=$?"'
docker run --rm -v "C:\Users\Giovanni\Documents\Phantom\Phantom:/src" -w /src/core rust:1 bash -c 'cargo test -p phantom-core negotiation --release; cargo test -p phantom-core keyboard --release; cargo test -p phantom-core close --release; cargo test -p phantom-core gate --release'
docker run --rm -v "C:\Users\Giovanni\Documents\Phantom\Phantom:/src" -w /src/core rust:1 bash -c 'cargo test -p phantom-quic --release'
docker run --rm -v "C:\Users\Giovanni\Documents\Phantom\Phantom:/src" -w /src/core rust:1 bash -c 'rustup toolchain install 1.82 --profile minimal >/dev/null 2>&1; CARGO_TARGET_DIR=/tmp/msrv cargo +1.82 check --workspace --lib --exclude phantom-ffi --exclude uniffi-bindgen; echo "msrv_exit=$?"'
```
Atteso: `fmt_exit=0`, `clippy_exit=0`, `test_exit=0` con 20 blocchi
`test result: ok` (30 test in `phantom-core`, 9 in `phantom-ffi`); 5 + 4 + 5 + 4
test nei quattro filtri; `phantom-quic` → 3 test `ok` (ultima riga `QUIC:
inviati ... consegnati ...`, 269 stati del tocco); `msrv_exit=0`.

Binding Swift (senza Mac), in due passi:

```
docker run --rm -v "C:\Users\Giovanni\Documents\Phantom\Phantom:/src" -w /src/core rust:1 bash -c 'cargo build -p phantom-ffi --release && mkdir -p ../dist/swift && cargo run -q -p uniffi-bindgen --release -- generate --library target/release/libphantom_ffi.so --language swift --out-dir ../dist/swift'
docker run --rm -v "C:\Users\Giovanni\Documents\Phantom\Phantom:/src" -w /src swift:5.10 bash -c 'swiftc -O -module-name PhantomSmoke -Xcc -fmodule-map-file=/src/dist/swift/phantom_ffiFFI.modulemap -I /src/dist/swift -L /src/core/target/release -lphantom_ffi /src/dist/swift/phantom_ffi.swift core/crates/phantom-ffi/swift-smoke/main.swift -o /tmp/smoke && LD_LIBRARY_PATH=/src/core/target/release /tmp/smoke'
```
Atteso: una riga `OK: ... frame, ... stati del tocco, ... tasti, ...`.

Test iOS dell'app (cinque blocchi, ora c'è anche la sessione):
```
docker run --rm -v "C:\Users\Giovanni\Documents\Phantom\Phantom:/src" -w /src/ios/Tests swift:5.10 bash -lc "tr -d '\r' < run-tests.sh | bash -s"
```
Atteso: cinque blocchi, ognuno con «Tutti i test superati» (l'ultimo è
«sessione (apertura e chiusura)»).

## Uncommitted changes

**v1.2.0 — lag e qualità sul Redmi Note 11 Pro 5G (Snapdragon 695, modalità Standard, Wi-Fi):**

- Trasporto (`phantom-quic`, `phantom-p2p`): BBR al posto di Cubic, coda datagrammi in uscita 1 MiB → 512 KiB,
  `datagram_send_buffer_space()` esposto; `noq-proto` aggiunto come dipendenza diretta di `phantom-p2p` (stessa
  versione usata da `iroh`). Dettagli e motivazioni in `ADR-009` (aggiornamento v1.2.0).
- Core: `Host::request_keyframe()` (keyframe chiesto dal lato host), `VideoReceiver::request_resync()` e
  `Client::request_keyframe()` (il decoder dell'app ha perso un frame: scarta i non-key e chiede un keyframe, motivo 2).
  Nuovo modulo di test `resync` (2 test). Due `if` dentro `match` trasformati in guardie per clippy 1.97.
- FFI: `PhantomHost::send_frame_via(connection, …) -> FrameSendReport`, `PhantomHost::request_keyframe`,
  `PhantomClient::request_keyframe`, `QuicConnection::queued_send_bytes`; il test loopback QUIC usa `send_frame_via`.
- Android:
  - `HostForegroundService`: Wi-Fi lock `LOW_LATENCY` + `HIGH_PERF` per tutta la sessione.
  - `VideoEncoder`: allineamento 8 (non più 1080 → 1088 stirato), scala esatta (1080×2400 → 864×1920 con limite
    1920), riduzione a passi del 3%, fps scelti dai `PerformancePoint` del SoC (120 → 60 se non regge),
    istante di cattura da `presentationTimeUs`.
  - `PhantomHostSession`: invio frame con `sendFrameVia`, contatori di invii falliti/pacchetti scartati e riga di log
    «Rete in affanno» ogni 5 s quando cambiano.
  - `HostFormatHelpers`: `scaledDimensions` senza arrotondare per eccesso, `fitEncoderSize`, `realisticFps`,
    `captureTimeUs` (+ 3 test JVM).
  - Gradle/CI: `assembleRelease` non debuggable, firma da keystore in secret o fallback chiave di debug,
    `lint.checkReleaseBuilds = false`; versione `1.2.0`.
- iOS: `RemoteControlSurface` segnala i frame scartati dal renderer o i fallimenti del decoder,
  `RemoteSession.requestKeyframeAfterDecoderDrop()` manda la richiesta di keyframe; versione `1.2.0`.
- `core/Cargo.lock` rigenerato: quello committato era fermo a prima di `phantom-p2p` (non conteneva `iroh`).

Verifiche fatte (container Linux, Rust 1.97): `cargo test --workspace --release` → 22 blocchi `test result: ok`;
`cargo fmt --check` e `cargo clippy --workspace --all-targets -D warnings` puliti; binding Kotlin e Swift rigenerati;
sorgenti Kotlin dell'app compilati con `kotlinc 2.0.21` contro `android.jar` API 34 + JNA + binding (unici errori:
`MainActivity` per la libreria ZXing non disponibile offline, file non toccato); 17 test JVM dell'app eseguiti con
uno stub JUnit → tutti ok. Non verificati: build Gradle/APK, compilazione Swift (lo fa la CI macOS), MSRV 1.82,
prova sul telefono.
