# Plan

Obiettivo: portare il core Rust a una baseline verde e verificata su questa
macchina (Windows + Docker), allineare i documenti allo stato reale dei binding
UniFFI e far usare all'app iPhone quello che il core sa fare.

Contesto: `main` a `e8070ca`, allineato a `origin/main`. Su Windows non ci sono
`cargo`/`rustc`: si usa Docker (immagine `rust:1`, toolchain stabile come
`core/rust-toolchain.toml`). Il workspace dichiara `rust-version = "1.82"`; la
toolchain dell'immagine è 1.100.0. `core/target/` è in `.gitignore`.

## T1 - Baseline dei test del core in Docker
- Status: done
- Acceptance: questo comando esce con codice 0 e mostra i riepiloghi `test result: ok` di tutti i crate.
  ```
  docker run --rm -v "C:\Users\Giovanni\Documents\Phantom\Phantom:/src" -w /src/core rust:1 bash -c 'cargo test --workspace --release -q >/tmp/t.log 2>&1; echo "cargo_exit_code=$?"; grep -c "test result: ok" /tmp/t.log'
  ```
- Notes: usare `bash -c`, non `bash -lc`: la shell di login ricarica
  `/etc/profile` e perde `/usr/local/cargo/bin` dall'immagine `rust:1`
  (`rustc: command not found`). In PowerShell l'outer string va in apici
  singoli, altrimenti `$?` viene espanso da PowerShell e non arriva a bash.
- Esito (2026-11-17): `cargo_exit_code=0`, 17 blocchi `test result: ok`,
  0 `FAILED`. Ripetuto dopo le modifiche di T2 con lo stesso esito.

## T2 - Formattazione e clippy puliti
- Status: done
- Acceptance: entrambi i comandi escono con 0 e nessun warning.
  ```
  docker run --rm -v "C:\Users\Giovanni\Documents\Phantom\Phantom:/src" -w /src/core rust:1 bash -c 'cargo fmt --all -- --check; echo "fmt_exit=$?"; cargo clippy --workspace --all-targets -- -D warnings; echo "clippy_exit=$?"'
  ```
- Notes: `cargo fmt --all -- --check` era già pulito. Tre modifiche necessarie:
  1. `phantom-video`, `FrameState::try_fec`: clippy 1.100 segnala
     `needless_range_loop` e poi `unnecessary if let`; il ciclo ora usa
     `self.data[first..last].iter().flatten()` (stesso XOR, stessi test).
  2. `uniffi-bindgen`: aggiunto `[lints] workspace = true` e la doc di crate in
     `src/main.rs` (senza, `missing_docs` falliva su `-D warnings`).
  3. `phantom-ffi`: sostituito il commento con una tabella `[lints]` esplicita
     (`unsafe_code = "allow"` perché il codice generato da UniFFI usa `unsafe`,
     più `missing_docs` e `clippy::all` come nel workspace). Toglie il warning
     cargo sul manifest senza cambiare le regole del resto del workspace.
- Esito (2026-11-17): `fmt_exit=0`, `clippy_exit=0`, nessuna riga
  `error:`/`warning:` nell'output.

## T3 - Allineare il README di progetto sui binding UniFFI
- Status: done
- Acceptance: `README.md` non afferma più che i binding mancano; la frase
  descrive il vero stato (`core/crates/phantom-ffi`, binding Swift e Kotlin
  generati e provati dalla CI, librerie iOS/Android come artifact). Verifica:
  `grep -n "UniFFI" README.md core/README.md` e rilettura delle righe toccate.
- Notes: il testo precedente ("Mancano i binding UniFFI") contraddiceva
  `core/README.md` e `.github/workflows/core.yml` (`ffi-apple`, `ffi-android`).
  `ios/PhantomF0/CoreLoopback.swift` importa già `PhantomHost`/`PhantomClient`
  dai binding. `core/README.md` era già corretto e non è stato toccato.

## T4 - Negoziazione della sessione (`Hello`/`HelloAck`) nel core
- Status: done
- Acceptance: `cargo test -p phantom-core negotiation --release` (in Docker
  `rust:1`) mostra 4 test `ok`, e la suite completa resta verde (T1 + T2).
- Notes: ADR-004 riga 27 promette per la fase 2 `Hello`/`HelloAck` completi;
  i messaggi esistono già in `phantom-proto` (encode/decode e round-trip) ma
  `Host`/`Client` non li usano e l'app iOS non li conosce. Specifica:
  - `phantom-proto`: nuovo modulo `caps` (`H264`, `H265`, `XOR_PARITY`, `NACK`,
    `KEYFRAME_REQUEST`, `QUEUE_DELAY`, tutti `u32 = 1 << n`) e modulo
    `close_reason` (`NORMAL = 0`, `BAD_PROTOCOL = 1`); `Codec::caps_bit()`.
  - `phantom-core`: `pub struct Negotiated { pub proto: u8, pub caps: u32,
    pub peer_nonce: [u8; 16] }`; `SessionConfig` ha `nonce: [u8; 16]` (default
    zero, l'app lo riempie con byte casuali) e `hello_retry_us: u64` (default
    250_000). `Host::negotiated()`/`Client::negotiated()`.
  - `Client::tick`: finché non ha un `HelloAck` manda `Hello { role: Client,
    proto_min: VERSION, proto_max: VERSION, caps, nonce }` al massimo ogni
    `hello_retry_us` (ritentare serve perché l'ack può perdersi). Accetta il
    primo `HelloAck` con `proto == VERSION`; i duplicati si ignorano.
  - `Host::on_datagram`: su `Hello` con `role == Client` sceglie
    `proto = min(proto_max, VERSION)`; se `proto < proto_min` risponde
    `Close { reason: BAD_PROTOCOL }` e non negozia; altrimenti risponde
    `HelloAck { proto, caps: peer_caps & host_caps, nonce: cfg.nonce }` e
    ricorda `Negotiated` (un `Hello` ripetuto riceve di nuovo l'ack: idempotente).
    Un `Hello` con `role == Host` si ignora.
  - Le capacità sono l'intersezione: codec configurato (`caps_bit`), `NACK`,
    `KEYFRAME_REQUEST`, `QUEUE_DELAY`, più `XOR_PARITY` solo se `fec_group > 0`.
  - Il resto del traffico non cambia: nessun gate, nessuna nuova voce in
    `HostStats`, nessuna modifica a `phantom-ffi` (i binding restano com'erano).
  - Test nuovi in `phantom-core` modulo `negotiation`: protocollo e caps,
    ritentativi finché manca l'ack, `role` sbagliato ignorato, versione futura
    rifiutata con `Close`. Aggiornare il test esistente
    `pongs_count_only_once_and_only_if_asked`, che prendeva `tick(1_000)[0]`
    come ping: ora il primo datagramma è `Hello`, quindi il ping va cercato con
    `matches!`.
- Esito (2026-11-17): `cargo test -p phantom-core negotiation --release` → 5 test
  `ok`; suite completa exit 0, 17 blocchi `test result: ok`; `fmt` e `clippy`
  puliti.
- Nota sui test end-to-end: il primo `Hello` cambia i pescaggi del generatore
  casuale del simulatore, quindi `burst_loss_triggers_keyframe_recovery` (prima
  su un solo seed, col 84% di consegne) è stato riscritto su nove seed con
  soglie sulle medie. Misurato su 30 seed: con e senza `Hello` le medie sono
  0,809 / 0,823 di frame consegnati e 0,802 / 0,794 nell'ultimo terzo, quindi la
  negoziazione non cambia nulla e i numeri precedenti erano una realizzazione
  fortunata.
- Provato e **scartato**: dare al simulatore un generatore per verso (per non
  far spostare le perdite del video dai pacchetti di controllo). Isola i versi,
  ma cambia la sequenza dei ping e `phantom-clock`
  `drift_is_estimated_and_corrected` sfora (−378 µs contro i 300 µs richiesti):
  reverted, vedi `PROGRESS.md`.

## T5 - Tastiera nel core
- Status: done
- Acceptance: `cargo test -p phantom-core keyboard --release` mostra i test
  `keyboard::*` `ok`, e la suite completa resta verde (T1 + T2).
- Notes: `Message::Key { key_seq, t_client_us, code, down }` esiste in
  `phantom-proto` (decoder che rifiuta `down` diverso da 0/1) ma nessuno lo usa:
  `Host::on_datagram` lo scarta con `_ => {}` e il client non ha un modo per
  mandarlo. Specifica:
  - `phantom-core`: `pub struct KeyEvent { pub seq: u32, pub t_client_us: u64,
    pub code: u16, pub down: bool }`; `Client::key(&mut self, now_us: u64,
    code: u16, down: bool) -> Option<Vec<u8>>` (usa `key_seq`, incrementale);
    `Host::take_key(&mut self) -> Option<KeyEvent>`.
  - Sono **eventi**, non stato: si accodano tutti (a differenza del tocco). La
    coda è limitata (`MAX_PENDING_KEYS = 32`) e in caso di pieno si butta il più
    vecchio: meglio perdere una pressione vecchia che quella appena fatta.
  - Numerazione come per il tocco: si ignorano i datagrammi con `key_seq` non
    più nuovo dell'ultimo accettato (duplicati e riordini), con confronto a
    wrap-around (`wrapping_sub < 1<<31`).
  - `HostStats` ha il nuovo contatore `key_events` (eventi accettati). Nessuna
    modifica ai binding UniFFI né al protocollo.

## T6 - Chiusura ordinata della sessione
- Status: done
- Acceptance: `cargo test -p phantom-core close --release` mostra i test
  `close::*` `ok`, e la suite completa resta verde.
- Notes: `Message::Close { reason }` esiste e l'host lo spedisce quando rifiuta
  la versione, ma nessuno dei due lati lo gestisce. Specifica:
  - Stato terminale: `closed: Option<u8>` (`None` = aperta) in `Host` e `Client`,
    esposto con `is_closed() -> bool` e `close_reason() -> Option<u8>`.
  - `Host::close(reason) -> Option<Vec<u8>>` e `Client::close(reason)`:
    mandano `Close` (se la sessione era aperta) e poi considerano la sessione
    chiusa. Il motivo `0` è `phantom_proto::close_reason::NORMAL`.
  - Da chiusa: `on_datagram` restituisce vuoto e non cambia stato (nessuna
    risposta, nemmeno a ping o `Hello`); `Client::tick`, `touch_tick`, `key`,
    `stats_message` restituiscono `None`/vuoto; `Host::send_frame` restituisce
    vuoto. Per una nuova sessione l'app crea un nuovo `Host`/`Client`.
  - `Close` ricevuto: si registra il motivo (vince il primo) e non si risponde.
  - Il rifiuto di `Hello` con `BAD_PROTOCOL` **non** chiude l'host: serve
    lasciargli accettare un `Hello` corretto con una versione minore.
- Esito (2026-11-17): `cargo test -p phantom-core keyboard --release` → 4 test
  `ok` (`press_and_release_arrive_in_order`,
  `duplicates_and_stale_events_are_dropped`, `queue_is_bounded_and_keeps_the_newest`,
  `closed_client_sends_no_keys`); `cargo test -p phantom-core close --release`
  → 5 test `ok` (`client_close_reaches_the_host`, `host_close_reaches_the_client`,
  `closing_twice_sends_nothing`, `refused_protocol_does_not_close_the_host`, più
  `keyboard::closed_client_sends_no_keys` che il filtro prende per il nome); suite completa exit 0, 17 blocchi
  `test result: ok`, 0 `FAILED`; `fmt` e `clippy` puliti.

## T7 - Nei binding UniFFI: negoziazione, tastiera, chiusura
- Status: done
- Acceptance: `cargo test -p phantom-ffi --release` → 8 test `ok`; il programma
  Swift `crates/phantom-ffi/swift-smoke/main.swift` compila e gira contro la
  libreria Linux (`swiftc` in Docker `swift:5.10`, comandi in `HANDOFF.md`).
- Notes: il core sa fare più di quanto i binding mostrino. Da esporre:
  - `SessionSettings.nonce: Vec<u8>` (16 byte; più corto completato con zeri,
    più lungo troncato) e `default_session_settings()` che lo riempie;
  - `NegotiatedInfo { proto: u8, caps: u32, peer_nonce: Vec<u8> }` con
    `PhantomHost::negotiated()` e `PhantomClient::negotiated()`;
  - `Capability` (H264, H265, XorParity, Nack, KeyframeRequest, QueueDelay) e
    `caps_contains(caps, capability) -> bool`, per non usare numeri magici
    nell'app;
  - `KeyInput { seq, client_time_us, code, down }`, `PhantomClient::send_key` e
    `PhantomHost::take_key()`;
  - chiusura: `is_closed()`, `close_reason()`, `close(reason) -> Option<Vec<u8>>`
    su entrambi gli oggetti;
  - `HostCounters.key_events`.
  Nessuna logica qui dentro: solo inoltro e conversione dei tipi. Aggiornare
  anche il programma Swift di prova (apertura, 30 eventi di tastiera, chiusura
  finale) e verificarlo in locale compilando la libreria `.so` su Linux e
  generando i binding con `uniffi-bindgen` (`--language swift`/`kotlin`).
- Esito (2026-11-17): `cargo test -p phantom-ffi --release` → 8 test `ok`
  (quattro nuovi: `negotiation_and_caps_are_visible`,
  `nonce_crosses_the_bindings_and_short_ones_are_padded`,
  `keyboard_events_pass_through_the_bindings`, `close_is_terminal_on_both_sides`).
  Binding generati su Linux da `libphantom_ffi.so`: Swift (poi compilato ed
  eseguito con `swiftc` di `swift:5.10` insieme a `swift-smoke/main.swift` →
  «OK: 362 frame, 600 stati del tocco, 30 tasti, offset orologi 5000000 µs,
  bitrate 20 Mbps») e Kotlin (`capsContains`, `takeKey`, `sendKey`,
  `NegotiatedInfo`, `KeyInput`, `keyEvents` presenti). Suite completa exit 0,
  17 blocchi, `fmt` e `clippy` puliti.

## T8 - Apertura e chiusura della sessione nella prova «tutto sul telefono»
- Status: done
- Acceptance: `ios/Tests/run-tests.sh` in Docker `swift:5.10` mostra il nuovo
  blocco «sessione» con tutte le righe `ok` e termina con «Tutti i test
  superati»; la suite del core resta verde (nessuna modifica ai crate).
- Notes: il core negozia e chiude dal T4/T6, i binding lo espongono dal T7, ma
  `ios/PhantomF0/CoreLoopback.swift` crea host e client e non guarda mai
  `negotiated()` né chiude la sessione. Specifica:
  - nonce vero di 16 byte casuali in `SessionSettings.nonce` (oggi resta zero);
  - nuovo file puro `ios/PhantomF0/SessionAgreement.swift` (`Foundation` e
    basta): proto, codec concordato (HEVC se c'è, altrimenti H.264, `nil` se
    nessuno), funzioni di recupero, nonce dell'altro lato, `summary` per il
    rapporto e `closeText(reason)` (0 = normale, 1 = versione non supportata);
    niente bit del protocollo duplicati: i `caps` si leggono con
    `capsContains` e arrivano qui come booleani;
  - nel giro del `tick`: appena arriva l'ack si registra l'accordo; se l'HEVC
    non è stato concordato la prova si ferma con un messaggio (codifica solo
    HEVC); se la sessione si chiude da sola la prova si ferma e il rapporto
    dice il motivo;
  - a fine prova il client chiude (`close(reason: 0)`), il `Close` arriva
    all'host, e il rapporto dice se entrambi risultano chiusi e se dopo la
    chiusura il client tace (né `tick` né `send_key` producono byte).
  - Test nuovo `ios/Tests/SessionAgreementTests.swift` aggiunto a
    `run-tests.sh`: codec scelto, funzioni di recupero in ordine, nessun codec
    in comune, testi di chiusura.
  - `CoreLoopback.swift` importa UIKit/VideoToolbox: non si compila né su
    Windows né su Linux. La verifica locale copre la parte pura e il percorso
    FFI equivalente (già in `swift-smoke`); la compilazione dell'app resta alla
    CI macOS.
- Esito (2026-11-17): `ios/Tests/run-tests.sh` in Docker `swift:5.10` → cinque
  blocchi tutti superati, compreso il nuovo «sessione (apertura e chiusura)» con
  14 controlli `ok` (codec scelto, recupero delle perdite in ordine, nessun
  codec in comune, testi di chiusura) e «Tutti i test superati». `swiftc -parse`
  su `CoreLoopback.swift`, `CoreLoopbackView.swift` e `SessionAgreement.swift`
  → exit 0: la parte nuova è sintatticamente valida, ma senza UIKit non si può
  fare di più su Linux. Il core non è stato toccato (suite di T7 già verde).

## T9 - Trasporto QUIC con datagrammi (fase 2, prima parte)
- Status: done
- Acceptance: in Docker `rust:1`,
  `cargo test -p phantom-quic --release` mostra i test `ok` (sessione completa
  host/client su QUIC reale in loopback, certificato sbagliato rifiutato); la
  suite completa del workspace resta verde; `cargo fmt --all -- --check` e
  `cargo clippy --workspace --all-targets -- -D warnings` puliti;
  `cargo +1.82 check --workspace --lib --exclude phantom-ffi --exclude
  uniffi-bindgen` verde.
- Notes: il core resta puro (macchine a stati, niente I/O). Il nuovo crate
  `phantom-quic` sposta datagrammi opachi sopra QUIC (RFC 9221), senza toccare
  `phantom-core`:
  - `HostEndpoint::bind(addr)` genera un certificato autofirmato effimero,
    espone `local_addr()`, `certificate_der()` (da fissare nel client) e
    `accept(timeout) -> QuicLink`.
  - `ClientEndpoint::bind(addr)` e
    `connect(addr, server_name, pinned_certificate_der, timeout) -> QuicLink`.
    Il client **fissa** il certificato dell'host (niente PKI: autenticazione e
    accoppiamento arrivano nella fase 5).
  - `QuicLink`: `send_datagram`, `recv_datagram(buf, timeout)`,
    `max_datagram_size`, `close(code, reason)`, `is_closed`.
  - ALPN `phantom/0`, datagrammi non affidabili, nessun flusso; il protocollo
    continua a negoziare con `Hello`/`HelloAck` dentro QUIC.
  - Dipendenze bloccate per l'MSRV 1.82: `quinn 0.11.8`, `quinn-proto
    0.11.12`, `quinn-udp 0.5.13`, `tokio 1.47.1`, `rcgen 0.13.2`, `time
    0.3.36`; `rustls 0.23` con `ring`.
  - Il job CI `cross` per Android deve passare `CC`/`AR` dell'NDK perché `ring`
    compila C anche con `cargo check`.
- Esito (2026-11-17): crate `phantom-quic` creato e aggiunto al workspace.
  `cargo test -p phantom-quic --release` → 3 test `ok` in 2,05 s:
  `session_over_quic_negotiates_exchanges_and_closes` (235 frame inviati,
  234 consegnati, 272 stati del tocco all'epoca e 269 dopo il cancello di T10,
  8 eventi di tastiera, `max_datagram_size` 1162, chiusura ordinata su entrambi
  i lati), `client_rejects_a_certificate_that_does_not_match`,
  `empty_pinned_certificate_is_rejected_immediately`. Il test adatta
  `frag_size` a `max_datagram_size − 40`: alla MTU iniziale di 1200 il payload
  massimo è 1162, quindi i datagrammi da 1200 byte dell'ADR-004 non ci stanno
  finché la path MTU discovery non cresce (annotato in ADR-004 e ADR-009).
  Suite completa `cargo test --workspace --release` → exit 0, 20 blocchi
  `test result: ok`, 0 `FAILED`; `cargo test -p phantom-quic` (debug) → 3 test
  `ok`; `fmt`, `clippy -D warnings` e `cargo doc --no-deps` con
  `RUSTDOCFLAGS=-D warnings` puliti; `cargo +1.82 check --workspace --lib
  --exclude phantom-ffi --exclude uniffi-bindgen` → exit 0.
  `Cargo.lock`: 70 pacchetti nuovi, nessuna dipendenza preesistente aggiornata;
  versioni fissate per l'MSRV (`quinn 0.11.8`, `quinn-proto 0.11.12`,
  `quinn-udp 0.5.13`, `tokio 1.47.1`, `rcgen 0.13.2`, `time 0.3.36`,
  `zeroize 1.8.1`). CI: al job `cross` Android aggiunto `CC`/`AR`/`RANLIB`
  dell'NDK per il C di `ring` (non eseguibile in locale: niente NDK su questo
  PC; la verifica vera la fa la CI).

## T10 - Apertura obbligatoria: cancello su video e input
- Status: done
- Acceptance: `cargo test -p phantom-core gate --release` mostra 4 test `ok`,
  `cargo test -p phantom-ffi --release` → 9 test `ok`, la suite completa resta
  verde, `fmt`/`clippy`/`doc`/MSRV 1.82 puliti; la prova QUIC resta verde con
  tocco e tastiera solo dopo l'ack.
- Notes: la negoziazione c'era ma non faceva da cancello: l'host spediva video
  appena l'encoder produceva un frame e il client mandava ping, tocco, tastiera
  e statistiche anche senza `HelloAck`. Specifica:
  - Host: `send_frame` restituisce vuoto e `wants_keyframe` è falso finché
    `negotiated()` è `None` (il frame scartato non entra nelle statistiche);
    `on_datagram` accetta solo `Hello` e `Close`, e ignora senza toccare i
    contatori `Ping`, `Nack`, `Stats`, `KeyframeRequest`, `Touch` e `Key`.
  - Client: `tick` manda solo `Hello` finché manca l'ack (niente ping, timer del
    video né statistiche); `touch_tick`, `key` e `stats_message` restituiscono
    `None`; `on_datagram` accetta solo `HelloAck` e `Close`, scartando video e
    pong. Il primo ping, tocco e statistiche partono al giro successivo all'ack.
  - `close` vale sempre, anche prima dell'apertura. Un `Hello` ripetuto riceve di
    nuovo l'ack; le copie non cambiano l'accordo.
  - I test esistenti di ping e tastiera aprono prima la sessione; nuovo modulo
    `gate` con 4 test (video dell'host, input dell'host, input del client,
    video/pong del client). Nei binding, `swift-smoke` manda il primo tasto dopo
    l'ack (passo 50) per non perdere l'evento.
- Esito (2026-11-18): `cargo test -p phantom-core gate --release` → 4 test `ok`;
  suite completa `cargo test --workspace --release` → exit 0, 20 blocchi
  `test result: ok`, 0 `FAILED`, con 30 test in `phantom-core` e 9 in
  `phantom-ffi`; prova QUIC → 3 test `ok` (235 frame inviati, 234 consegnati,
  269 stati del tocco, 8 eventi di tastiera, chiusura su entrambi i lati);
  `swiftc` di `swift:5.10` su `swift-smoke/main.swift` → «OK: 362 frame, 600
  stati del tocco, 30 tasti, offset orologi 5000000 µs, bitrate 20 Mbps»;
  `fmt_exit=0`, `clippy_exit=0`, `doc_exit=0`, `msrv_exit=0`.
