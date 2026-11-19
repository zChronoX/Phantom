# Handoff

Scritto: 2026-11-19 da Particle-1 (Kilo); aggiornato dopo il fix di ciclo di vita `QuicConnection`/endpoint QUIC e i miglioramenti UI/feedback del loopback iOS.
Branch: main - Commit locale: `1a97d97` (allineato a `origin/main`) + modifiche locali non committate
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

- Il blocco precedente è stato committato e pushato in `1a97d97` (build CI verdi).
- Nella prima prova su iPhone reale il loopback QUIC mostrava «Sessione chiusa: ancora aperta»
  con spinner fisso sovrapposto all'HUD: `let clientEp = try QuicClientEndpoint.bind(...)` in
  `RemoteSession.swift` usciva di scope a fine `runConnect`, e `Drop for ClientEndpoint` in
  `phantom-quic` chiudeva l'endpoint e tutte le connessioni aperte.
- Applicati i fix (non ancora committati):
  - `core/crates/phantom-ffi/src/lib.rs`: `QuicHostEndpoint` e `QuicClientEndpoint` tengono
    `Arc<HostEndpoint>`/`Arc<ClientEndpoint>` e ogni `QuicConnection` mantiene `_keep_alive`
    (dichiarato dopo `link`, così `link` droppa sempre prima dell'endpoint/runtime). Aggiunto
    test `quic_connection_survives_dropping_endpoints`.
  - `core/crates/phantom-ffi/swift-smoke/main.swift`: aggiunto `runQuicEndpointLifetimeSmoke()`.
  - `ios/PhantomF0/RemoteSession.swift`: mantenuto `clientEndpoint` in `RemoteSession` fino a
    `stop()`, usato `127.0.0.1:0` in loopback, separato il messaggio tra chiusura protocollo e
    chiusura trasporto QUIC, spostato `isIdleTimerDisabled` sul main thread, salvato l'ultimo
    `TouchState` in `LocalQuicHostRunner` per disegnare i tocchi ricevuti nel frame HEVC
    sintetico, aggiunto guard `!self.isStopped` + `VTCompressionSessionCompleteFrames`.
  - `ios/PhantomF0/RemoteControlSurface.swift`: spinner mostrato solo con `session.running`,
    icona di avviso quando fermo, banner centrato verticalmente senza sovrapposizione all'HUD.
- **Verifiche locali non eseguite in questa sessione**: Docker Desktop spento
  (`open //./pipe/dockerDesktopLinuxEngine: Impossibile trovare il file specificato`).

## Next steps (in order)

1. Eseguire `powershell -NoProfile -ExecutionPolicy Bypass -File tools/repo-preflight.ps1`.
2. Committare e pushare su `origin/main` (oppure avviare Docker Desktop per eseguire prima in
   locale i comandi nella sezione «How to verify»), poi scaricare il nuovo `PhantomF0.ipa`
   dalla release `f0-latest` di GitHub Actions.
3. Riprovare su iPhone «Controllo remoto → Schermo Android (QUIC) → Avvia in loopback QUIC locale»
   (verifica video HEVC, barra animata, quadrati gialli/luminosi sui tocchi ricevuti dall'host,
   rotazione, tastiera e tasti rapidi).
4. Lato Android: iniziare il server host Kotlin (`app_process`) che usa `PhantomHost`,
   `QuicHostEndpoint` e `MediaCodec`, oppure proseguire con Reed-Solomon / Fase 5.
5. A fine sessione aggiornare `PROGRESS.md` e questo file.

## Decisions and constraints from the user

- **Nessun POCO disponibile**: niente misure sul telefono né test del server
  Android.
- Solo Windows in locale: niente `cargo`/`rustc`, Xcode, `swiftc`, Java, Android
  SDK, `gh`. Docker funziona: `rust:1` per il core, `swift:5.10` per Swift.
- Non pushare e non cancellare branch o lavoro locale senza richiesta.
  `archive/f0-avsamplebuffer` è l'unica copia dei due commit locali.
- **Un solo commit** alla fine: il testo è in `COMMIT_MSG.md`.
- `origin/main` è la fonte di verità; preflight obbligatorio prima di ogni
  modifica.
- Pochi passi piccoli per sessione; il contesto si riempie facilmente.

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

- `core/crates/phantom-ffi/src/lib.rs`: `QuicHostEndpoint`/`QuicClientEndpoint` su `Arc` +
  `QuicEndpointKeepAlive` in `QuicConnection` (dopo `link`), test `quic_connection_survives_dropping_endpoints`.
- `core/crates/phantom-ffi/swift-smoke/main.swift`: aggiunto `runQuicEndpointLifetimeSmoke()`.
- `ios/PhantomF0/RemoteSession.swift`: mantenuto `clientEndpoint` fino a `stop()`, bind
  `127.0.0.1:0` per loopback, messaggi distinti per chiusura sessione/trasporto, `isIdleTimerDisabled`
  su main thread, eco visivo dei tocchi ricevuti dall'host nei frame HEVC del loopback e completamento
  pulito dell'encoder.
- `ios/PhantomF0/RemoteControlSurface.swift`: spinner solo con `session.running`, icona avviso
  quando fermo, `connectionBanner` centrato verticalmente senza sovrapporsi all'HUD.
- `HANDOFF.md`, `PROGRESS.md`: aggiornati a fine sessione.
