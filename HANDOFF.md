# Handoff

Scritto: 2026-11-19 da Particle-1 (Kilo); aggiornato dopo la creazione della nuova app host Android (`android/app/`).
Branch: main - Commit locale: `f5d0aa3` (allineato a `origin/main`) + modifiche locali non committate (identità persistente, QR 1-tap, Bonjour/mDNS, supporto 5G/4G + Tailscale/DDNS, rotazione schermo live, riconnessione automatica, multi-codec)
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

1. Eseguire `powershell -NoProfile -ExecutionPolicy Bypass -File tools/repo-preflight.ps1`.
2. Committare e pushare su `origin/main` per far partire `.github/workflows/android-app.yml` su GitHub Actions.
3. Scaricare `PhantomHost-debug.apk` dalla release `f0-latest` (o dagli artifacts della run):
   - provarlo in **modalità Root** su **LDPlayer** (x86_64, Root + Network Bridging abilitati) per testare end-to-end video + multi-touch + tastiera dall'iPhone;
   - provarlo in **modalità Standard (MediaProjection)** sul **Redmi Note 11 Pro 5G** (in attesa dello sblocco bootloader/root) per testare video QUIC 1080p · 120 fps sull'hardware reale.
4. A fine sessione aggiornare `PROGRESS.md` e questo file.

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
- Passare `-Djava.library.path=<libDir>` ad `app_process64`: sovrascrive `/system/lib64` e rompe le librerie JNI di sistema come `Conscrypt`/`NativeCrypto` (usato da `SecureRandom`). Caricare invece `libjnidispatch.so`/`libphantom_ffi.so` via `System.load(path)` + `-Djna.*` e preservare `/system/lib64:/vendor/lib64` in `LD_LIBRARY_PATH`.
- Usare un campo chiamato `message` nelle varianti di un enum `#[derive(uniffi::Error)]` in Rust: nel binding Kotlin generato da UniFFI collide con `Throwable.message` (`Conflicting declarations: val message: String`). Usare nomi come `detail`.
- Mandare video, ping, tocco, tastiera o statistiche prima dell'apertura: il
  core ora le scarta (cancello T10). L'host deve aspettare `negotiated()` e il
  client l'ack; i test di ping e tastiera aprono prima la sessione. Non aggirare
  il cancello con un `SessionConfig` o un percorso separato: è voluto.

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

- **Core Rust + QUIC + FFI**:
  - `core/crates/phantom-quic/src/lib.rs` + `tests/quic_loopback.rs`: `HostIdentity`, `generate_host_identity()`, `HostEndpoint::bind_with_identity`, keep-alive QUIC 3s + idle timeout 15s (per 5G/4G/NAT/Tailscale).
  - `core/crates/phantom-core/src/lib.rs`: `SessionConfig.extra_caps` per annunciare entrambi i codec dal client + test.
  - `core/crates/phantom-ffi/src/lib.rs`: esposti `HostIdentity`, `generate_host_identity`, `QuicHostEndpoint::bind_with_identity`, `PhantomClient::new_multi_codec`, risoluzione DNS (`ToSocketAddrs`, IPv4-first) in `QuicClientEndpoint::connect` per DDNS/Tailscale MagicDNS + test.
- **Android Host APK (`android/app/`)**:
  - `app/build.gradle.kts`, `AndroidManifest.xml`: aggiunto `com.google.zxing:core:3.5.3` e `CHANGE_WIFI_MULTICAST_STATE`.
  - `core/PairingCodec.kt` + `PairingCodecTest.kt`, `core/AnnexBFramePrep.kt`: URI `phantom://pair?...`, SHA-256 fingerprint, Base64URL e `selectPreferredVpnIpv4` (Tailscale/WireGuard).
  - `HostIdentityStore.kt`: identità TLS persistente salvata in `filesDir/identity/`.
  - `stream/PhantomHostSession.kt`, `root/RootHostMain.kt`, `root/RootInputInjector.kt`: identità persistente, rotazione display live Portrait ↔ Landscape sulla stessa connessione QUIC, floor bitrate 1 Mbps per 4G/5G.
  - `HostStateStore.kt`, `HostForegroundService.kt`, `MainActivity.kt`: annuncio mDNS `_phantom._udp.`, codice QR live (LAN + VPN/remoto), campo indirizzo remoto 5G/4G (Tailscale/DDNS) e rigenerazione chiave.
- **Client iOS (`ios/`)**:
  - `project.yml`: aggiunti `NSCameraUsageDescription` e `NSBonjourServices` (`_phantom._udp`).
  - `PairingModels.swift`, `PairedDeviceStore.swift`, `BonjourDiscovery.swift`, `QRScannerView.swift`: dispositivi salvati 1-tap, scanner QR nativo, scoperta Bonjour LAN, failover LAN → Remoto 5G/VPN, backoff riconnessione.
  - `RemoteSession.swift`, `RemoteControlSurface.swift`, `RemoteConnectView.swift`: failover multi-endpoint, riconnessione automatica su drop, `PhantomClient.newMultiCodec`, reset tocchi su cambio orientamento video.
  - `Tests/PairingModelsTests.swift` + `Tests/run-tests.sh`: nuova suite test per SHA-256, `PairingURI`, DNS, failover candidati e backoff.
- `HANDOFF.md`, `PROGRESS.md`: aggiornati.
