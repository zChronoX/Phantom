# ADR-008 · Binding UniFFI: un involucro sottile attorno a macchine a stati pure

- Stato: accettata, da rivedere nella fase 2 (quando il trasporto QUIC passa dentro il core)
- Data: 2026-11-01

## Contesto

Il core Rust deve girare nell'app iOS (Swift) e nel server Android (Kotlin). Con il trasporto fuori dal core, a 20 Mbps il confine si attraversa circa 2.500 volte al secondo in ricezione (un datagramma alla volta) più i ping, il tocco a 240 Hz e le statistiche: poco, ma tutto sul percorso della latenza.

## Decisione

- **UniFFI con macro procedurali** (nessun file di definizione da mantenere a mano) in modalità libreria: i binding Swift e Kotlin si generano dalla libreria compilata con il crate `uniffi-bindgen` del workspace, quindi non possono divergere dal codice.
- **L'involucro (`phantom-ffi`) non contiene logica.** Due oggetti, `PhantomHost` e `PhantomClient`, più record (`SessionSettings`, `TouchPoint`, `KeyInput`, `NegotiatedInfo`, `VideoFrame`, contatori) e due enumerazioni (`VideoCodec`, `Capability`). È l'unico crate con `unsafe` (generato dalle macro); tutti gli altri lo vietano con `forbid(unsafe_code)`.
- **Il tempo lo passa l'app** (`now_us`, microsecondi di un orologio monotono locale) e l'app possiede socket, thread e timer: chiama il core quando arriva un datagramma e a intervalli regolari (`tick`). Il core non crea thread, non legge orologi e non fa I/O. Così la stessa logica gira identica nelle prove, sull'iPhone e su Android, e il trasporto può cambiare (UDP, poi QUIC) senza toccare i binding.
- **Nessuna funzione restituisce errori**: un datagramma malformato o di un'altra sessione è ignorato (il parser non va mai in panico, ADR-004). Prima dell'apertura (`Hello`/`HelloAck`) il core ignora video e input e il client manda solo `Hello` (T10). Più di 10 dita nel tocco: le prime 10. Un `nonce` di lunghezza diversa da 16 byte si completa con zeri o si tronca.
- **Oggetti utilizzabili da più thread**: le chiamate sono serializzate da un blocco interno. Se un thread va in panico mentre lo tiene, il blocco si recupera invece di avvelenare tutto.
- L'**encoder decide i keyframe**: l'app chiede `wantsKeyframe`, lo domanda all'encoder (`requestSyncFrame` su MediaCodec) e passa a `sendFrame` ciò che è uscito, con il flag `key` vero. Il core non marca più come keyframe un frame che non lo è.

## Alternative scartate

- **C ABI a mano con cbindgen**: più `unsafe` scritto a mano, due interfacce da mantenere e i generatori Swift/Kotlin da trovare altrove.
- **Core con thread e socket propri che chiama l'app con callback**: più comodo per l'app, ma molto più difficile da provare in modo deterministico. Si valuta nella fase 2, quando il trasporto QUIC vivrà in Rust; l'attraversamento diventerà allora uno per frame e non uno per datagramma.
- Flutter, KMP e simili: vedi ADR-001.

## Conseguenze

- Ogni chiamata copia i byte (`Data` ↔ `Vec<u8>`): a 2.500 datagrammi al secondo è trascurabile.
- Con `panic = "abort"` nel profilo release un panico del core ferma il processo. Il core è scritto per non andare in panico con dati esterni (provato col parser e con i test di robustezza); un processo fermo è meglio di uno in stato incoerente. Da rivedere se in produzione ne comparisse uno.
- **Verifica in CI**: i binding Swift sono compilati ed eseguiti su macOS con un programma che pilota host e client (3 secondi di sessione in memoria, orologi sfasati di 5 s, tocco, tastiera, apertura e chiusura della sessione, thread concorrenti); i binding Kotlin sono generati e controllati nei nomi; la libreria statica per iPhone e simulatore e la libreria `.so` per Android arm64 sono compilate e pubblicate come artifact. Il Kotlin non viene ancora eseguito in CI (servono JVM e JNA): si proverà con il server Android nella fase 2. Lo stesso programma Swift si può compilare ed eseguire in locale su Linux (Docker `swift:5.10` e la libreria `.so` generata da `cargo build -p phantom-ffi`), quindi i binding si provano anche senza un Mac.
- UniFFI richiede un compilatore più recente del resto del workspace: la versione minima di Rust (1.82) vale per i crate del core ed esclude `phantom-ffi` e `uniffi-bindgen`.
