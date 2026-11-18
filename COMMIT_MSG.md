# Messaggio per il prossimo commit (unico)

Riferimento: `PLAN.md` T1–T10. Comprende negoziazione, apertura obbligatoria,
tastiera, chiusura ordinata, binding e app iPhone, trasporto QUIC,
riallineamento dei lint, documentazione e test end-to-end riscritto. Usare con:

```
git commit -F COMMIT_MSG.md
```

---

feat(core): sessione completa con apertura obbligatoria, trasporto QUIC e clippy pulito

Il protocollo sapeva già parlare della sessione e della tastiera, ma il core non
usava quei messaggi: host e client iniziavano a scambiarsi video e tocco senza
sapere che versione avesse l'altro lato, gli eventi di tastiera venivano scartati
e `Close` non era gestito da nessuno. Ora la sessione ha un inizio che fa da
cancello a video e input, un input di tastiera, una fine e le stesse possibilità
nei binding Swift e Kotlin; il nuovo crate `phantom-quic` porta gli stessi
datagrammi su QUIC, con TLS e datagrammi RFC 9221.

- `phantom-proto`: modulo `caps` con i bit delle funzioni (H.264/H.265, parità
  XOR, NACK, richiesta di keyframe, ritardo di coda), modulo `close_reason`
  (`NORMAL`, `BAD_PROTOCOL`) e `Codec::caps_bit()`.
- `phantom-core`, apertura: `Negotiated { proto, caps, peer_nonce }` esposto da
  `Host::negotiated()` e `Client::negotiated()`. Il client apre con `Hello` e
  ritenta ogni `hello_retry_us` (250 ms) finché l'ack non arriva, perché l'ack
  può perdersi. L'host sceglie `min(proto_max, VERSION)`, interseziona le
  funzioni annunciate e risponde; se non c'è una versione in comune chiude con
  `Close { reason: BAD_PROTOCOL }` e resta aperto per un `Hello` corretto. Un
  `Hello` con il ruolo dell'host si ignora; uno ripetuto riceve di nuovo l'ack.
  `SessionConfig` ha `nonce` (zero finché non arriva la cifratura della fase 5)
  e `hello_retry_us`.
- `phantom-core`, apertura obbligatoria: finché `negotiated()` è `None` l'host
  non manda video (`send_frame` vuoto, `wants_keyframe` falso) e ignora ping,
  NACK, statistiche, richieste di keyframe, tocco e tastiera; il client manda
  solo `Hello` (niente ping, timer del video o statistiche), scarta video e pong
  e lascia `touch_tick`/`key`/`stats_message` vuoti. `Close` vale in qualunque
  momento, anche prima dell'apertura. I test di ping e tastiera aprono prima la
  sessione; nuovo modulo `gate` con 4 test.
- `phantom-core`, tastiera: `Client::key(now_us, code, down)` e
  `Host::take_key() -> Option<KeyEvent>`. Sono eventi, non stato: si accodano
  tutti, la coda è limitata (`MAX_PENDING_KEYS = 32`, si perde il più vecchio) e
  duplicati o riordini si scartano con il confronto a wrap-around del tocco.
  `HostStats::key_events` conta quelli accettati.
- `phantom-core`, chiusura: `Host::close`/`Client::close` spediscono `Close` e
  portano la sessione in stato terminale, esposto con `is_closed()` e
  `close_reason()`. Da chiusa non si risponde più e non si manda più niente:
  `on_datagram` vuoto, `tick`/`touch_tick`/`key`/`stats_message` silenziosi,
  `send_frame` vuoto. Per una nuova sessione l'app crea un nuovo `Host`/`Client`.
- `phantom-ffi`: quello che il core sa fare ora si vede dai binding.
  `SessionSettings.nonce` (16 byte, completato o troncato), `NegotiatedInfo` con
  `PhantomHost::negotiated()`/`PhantomClient::negotiated()`, `Capability` e
  `caps_contains` per leggere le funzioni concordate senza numeri magici,
  `KeyInput` con `send_key`/`take_key`, chiusura (`is_closed`, `close_reason`,
  `close`) e `HostCounters::key_events`. Nessuna logica nuova nell'involucro.
- App iPhone (`ios/PhantomF0`): la prova «tutto sul telefono» annuncia un nonce
  vero, registra l'accordo appena arriva il `HelloAck` (versione, codec, funzioni
  di recupero, nonce dell'altro lato) e lo scrive nel rapporto; si ferma se
  l'HEVC non è stato concordato o se la sessione si chiude da sola, e alla fine
  chiude con `Close` controllando che l'host lo registri e che il client taccia.
  La lettura dell'accordo sta nel file puro `SessionAgreement.swift`, provato da
  `ios/Tests` con `swiftc`.
- Trasporto QUIC (fase 2, prima parte): nuovo crate `phantom-quic` sopra
  `quinn` 0.11 e `rustls` 0.23 (`ring`). `HostEndpoint` genera un certificato
  autofirmato effimero e lo espone in DER; `ClientEndpoint::connect` accetta
  solo quel certificato (confronto byte per byte, niente PKI: l'accoppiamento
  arriva nella fase 5). `QuicLink` sposta datagrammi non affidabili (RFC 9221,
  ALPN `phantom/0`), con API bloccante e runtime Tokio privato per endpoint:
  l'app non deve conoscere Tokio. Il core resta puro e identico a prima; la
  prova end-to-end su UDP loopback apre la sessione con `Hello`/`HelloAck`,
  scambia video, tocco e tastiera e chiude su entrambi i lati. Alla MTU iniziale
  di 1200 il massimo datagramma QUIC è 1162, quindi l'app deve scegliere
  `frag_size = max_datagram_size − 40` (ADR-009; ADR-004 aggiornato). Le
  versioni di `quinn`, `quinn-proto`, `quinn-udp`, `tokio`, `rcgen`, `time` e
  `zeroize` sono bloccate perché le ultime richiedono Rust 1.85 o l'edizione
  2024; la CI `cross` Android passa `CC`/`AR`/`RANLIB` dell'NDK per il C di
  `ring`.

Il core era verde per test e formattazione, ma `clippy -D warnings` falliva
sulla toolchain stabile corrente (1.100.0), la stessa che usa la CI:

- `phantom-video`: in `FrameState::try_fec` il ciclo sulla finestra del gruppo
  di parità indicizzava `self.data` con un contatore; ora usa
  `self.data[first..last].iter().flatten()`, con la stessa semantica e stessi
  test (`needless_range_loop`, `unnecessary if let`).
- `uniffi-bindgen`: eredita i lint del workspace (`[lints] workspace = true`) e
  ha la documentazione di crate che `missing_docs` richiedeva.
- `phantom-ffi`: il commento che spiegava l'esclusione dei lint diventa una
  tabella `[lints]` esplicita, uguale al workspace tranne
  `unsafe_code = "allow"` (il codice generato da UniFFI usa `unsafe`). Sparisce
  così anche il warning di cargo sul manifest.

`burst_loss_triggers_keyframe_recovery` passa da un solo seed a nove con soglie
sulle medie: il primo `Hello` sposta i pescaggi del generatore del simulatore e
la vecchia soglia (84% di consegne su un seed) era una realizzazione fortunata.
Misurato su 30 seed, con e senza `Hello` le medie sono 0,809/0,823 di frame
consegnati e 0,802/0,794 nell'ultimo terzo: la negoziazione non cambia il
comportamento, il vecchio test era fragile.

Il `README.md` diceva ancora «Mancano i binding UniFFI», mentre `phantom-ffi`,
la CI `ffi-apple`/`ffi-android` e `ios/PhantomF0/CoreLoopback.swift` li usano
già: la frase ora descrive i binding Swift e Kotlin generati e provati dalla CI
e le librerie pubblicate come artifact. ADR-004, ADR-008, ADR-009 e
`core/README.md` dicono che la sessione si apre con `Hello`/`HelloAck`, che la
tastiera c'è, che la chiusura è ordinata, che i binding espongono tutto questo e
che il trasporto QUIC vive in `phantom-quic`, con il limite dei datagrammi
legato alla path MTU.

Verifica (Docker: `rust:1` per il core, `swift:5.10` per Swift):

- `cargo test --workspace --release` → exit 0, 20 blocchi `test result: ok`,
  0 `FAILED`;
- `cargo test -p phantom-quic --release` → 3 test `ok`; sessione QUIC completa
  in loopback con 235 frame inviati e 234 consegnati, 269 stati del tocco,
  8 eventi di tastiera, chiusura ordinata su entrambi i lati e `max_datagram_size`
  1162; certificato diverso da quello fissato rifiutato;
- `cargo test -p phantom-core negotiation --release` → 5 test `ok`;
- `cargo test -p phantom-core keyboard --release` → 4 test `ok`;
- `cargo test -p phantom-core close --release` → 5 test `ok` (4 sono
  `close::*`, il filtro prende anche `keyboard::closed_client_sends_no_keys`);
- `cargo test -p phantom-core gate --release` → 4 test `ok`;
- `cargo test -p phantom-ffi --release` → 9 test `ok`;
- binding Swift generati da `libphantom_ffi.so` e compilati ed eseguiti con
  `swiftc` (Docker `swift:5.10`) insieme a `swift-smoke/main.swift` →
  «OK: 362 frame, 600 stati del tocco, 30 tasti, offset orologi 5000000 µs,
  bitrate 20 Mbps»; binding Kotlin generati con `NegotiatedInfo`, `KeyInput`,
  `KeyEvents`;
- `ios/Tests/run-tests.sh` (Docker `swift:5.10`) → cinque blocchi, tutti i test
  superati, compreso «sessione (apertura e chiusura)» con 14 controlli;
- `swiftc -parse` su `CoreLoopback.swift`, `CoreLoopbackView.swift` e
  `SessionAgreement.swift` → exit 0 (solo sintassi: UIKit su Linux non c'è);
- `cargo fmt --all -- --check` → exit 0;
- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0, nessun
  warning;
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` → exit 0;
- `cargo +1.82 check --workspace --lib --exclude phantom-ffi --exclude
  uniffi-bindgen` → exit 0 (dipendenze QUIC bloccate per l'MSRV).
