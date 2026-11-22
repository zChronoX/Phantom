# Handoff

Scritto: 2026-11-20; aggiornato con la **v1.3.0** (telemetria, Shizuku, Accessibilità, tasti di sistema, qualità adattiva, deriva audio, release seria, pulizia).
Branch: main - base `b948eda` (v1.2.1, pushata e taggata dall'utente) + modifiche v1.3.0 non committate (vedi «Uncommitted changes»).
Cartella sul PC: `C:\Users\Giovanni\Documents\Progetti personali\Phantom\Phantom` (spostata dall'utente).
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

1. Preflight, commit e push della v1.3.0; CI verde (`core`, `Android · Phantom APK`, `iOS · Phantom`). Punti mai
   compilati qui: Gradle (dipendenza Shizuku `dev.rikka.shizuku:api/provider:13.1.5`, risorse XML) e Swift.
2. (Una volta) `tools/release/create-keystore.ps1` e i 4 secret GitHub; poi disinstallare l'APK attuale.
3. Prove sul Redmi, nell'ordine: Standard senza Accessibilità (solo visione), Standard con Accessibilità (tap, swipe,
   pinch, Indietro/Home/Recenti/Notifiche, testo), Shizuku (tocco multi-dito, tastiera, schermo spento, audio),
   HUD espanso iOS (4 righe di telemetria), qualità adattiva (log «Qualità adattata alla rete»), intra-refresh.
4. Se tutto ok: tag `v1.3.0` (la CI controlla versioni e changelog), poi `tools/archive-legacy.ps1`.

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

**v1.3.2** (base: v1.3.1 = commit `3b7a0c2` sul PC). Dai test sul Redmi con **PixelOS Android 17** (senza root):
- Shizuku: `NoSuchMethodException DisplayControl.createDisplay(String, boolean)`. `RootDisplayCapture` prova ora
  `DisplayManager.createVirtualDisplay(name, w, h, displayIdToMirror=0, surface)` (statico nascosto, come scrcpy) per
  primo su Android 14+, poi `SurfaceControl.createDisplay`; su 10–13 l'ordine è invertito. `DisplayControl` resta
  ultima spiaggia, anche con la firma a 3 argomenti. Il metodo usato va nel log («Cattura display via …»).
- `root/WorkerEnvironment.kt`: porting dei `Workarounds` di scrcpy (ActivityThread finto, ConfigurationController,
  AppBindData `com.android.shell`, applicazione iniziale) + `FakeContext` (base = contesto di sistema,
  `getDeviceId() = 0`, attribuzione uid 2000). Chiamato in `RootHostMain` dopo `prepareMainLooper`; gli errori
  finiscono nel log come «Ambiente worker: …» senza bloccare l'avvio.
- Audio Shizuku NPE: `AudioStreamer.createForRoot` usa `WorkerEnvironment.context()`; se il builder lancia, ripiego su
  `root/ReflectiveAudioRecord.kt` (`native_setup` via riflessione, firme Android 12–13 e 14+).
- MediaProjection: `stream/ProjectionDisplay.kt` crea il `VirtualDisplay` una sola volta e poi `resize` + `setSurface`
  (Android 14+ vieta un secondo `createVirtualDisplay` sulla stessa proiezione: SecurityException al cambio di qualità).
- `RootDisplayPower`: su Android 15+ prima `DisplayManagerGlobal.requestDisplayPower(0, on)`.
- `VideoEncoder`: `vendor.qti-ext-enc-low-latency.enable=1` sugli encoder `c2.qti.*`/`OMX.qcom.*` (solo nei tentativi
  con hint di bassa latenza), log del nome encoder.
- iOS: Indietro/Home/Recenti nella pillola compatta (`RemoteControlSurface.collapsedHandle`, `pillNavButton`), timeout
  LAN 2,5/2,0 s quando c'è il P2P, indirizzo LAN aggiornato da Bonjour all'avvio della sessione.
- Versione `1.3.2`, sezione in `CHANGELOG.md`.

Verifiche: app Kotlin compilata con `kotlinc` (stub API 34), 29 test JVM ok. Core Rust invariato. Non verificati:
Gradle, Swift, telefono (in particolare Shizuku/audio su Android 17 e la pillola sull'iPhone).

**v1.3.1** (commit `3b7a0c2`; la v1.3.0 è nel commit `ddf5c78`). Dalla telemetria del Redmi in Shizuku:
- video a **488×1088**: l'encoder dichiara limiti «orizzontali» (1920×1088) e il codice li applicava alla lettera a uno
  schermo verticale. Ora `VideoEncoder.fitSizeToCapabilities` ragiona per lato lungo/corto e accetta la misura
  ruotata (`isSizeSupported(w,h) || isSizeSupported(h,w)`, fps interrogati sulla misura ruotata); in fondo ai tentativi
  c'è un ripiego `strictOrientation` con i limiti presi alla lettera, se l'encoder rifiutasse le misure ruotate.
- **tempesta di keyframe** in P2P (481 keyframe, RTT 1355 ms, coda host 489 KB): ogni pacchetto buttato chiedeva un
  keyframe. Nuovo `phantom_core::pacing::SourceDropper` (4 test): oltre ~250 ms di coda il frame si salta, a coda
  svuotata (≤ ¼ della soglia) un solo keyframe; un pacchetto buttato avvia lo svuotamento invece del keyframe.
  `FrameSendReport.skipped/request_keyframe`, `PhantomHost.frames_skipped`, `HostReport.frames_skipped` (campo in coda,
  decodifica tollerante: assente = 0, campi futuri ignorati; 1 test).
- **bitrate** a 33 Mbit/s per 488×1088: tetto `HostFormatHelpers.maxUsefulBitrate` (0,15 bpp HEVC, 0,22 H.264).
  P2P: avvio 6 Mbit/s, minimo 0,6 (era 1,5).
- HUD iOS: «saltati». `check-version.sh` chiama `changelog-section.sh` con `bash` (su Windows i file perdono il bit
  di esecuzione e la CI dei tag sarebbe fallita). Versione `1.3.1`, sezione in `CHANGELOG.md`.

Verifiche: `cargo test --workspace --release` 22 blocchi ok, `fmt`/`clippy`/`doc` puliti, app Kotlin compilata con
`kotlinc`, 29 test JVM ok. Non verificati: Gradle, Swift, telefono.
