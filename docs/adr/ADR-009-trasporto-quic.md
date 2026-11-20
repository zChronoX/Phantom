# ADR-009 · Trasporto QUIC con datagrammi (fase 2, prima parte)

- Stato: accettata, parziale (il trasporto c'è; app e binding non sono ancora collegate)
- Data: 2026-11-17

## Contesto

Il core (`phantom-core`) è una macchina a stati pura: non crea thread, non legge orologi e non fa I/O. Finora l'app gli consegnava datagrammi UDP già letti dal socket. La fase 2 chiede il trasporto di produzione, QUIC con datagrammi: niente head-of-line blocking per tocco, tastiera e video, come deciso nell'ADR-004.

QUIC richiede TLS 1.3 anche in rete locale. Non esiste ancora un'identità stabile dei dispositivi (cifratura e accoppiamento sono la fase 5), quindi serve un meccanismo provvisorio ma non insicuro: un certificato autofirmato per host, fissato dal client. Inoltre QUIC limita i datagrammi applicativi al path MTU: alla MTU iniziale di 1200 byte il massimo payload è circa 1160, quindi i datagrammi Phantom da 1200 byte **non** ci stanno finché la path MTU discovery non trova un MTU maggiore (tipicamente 1500 in LAN).

## Decisione

- **Nuovo crate `phantom-quic`**, separato dal core. Il core non cambia: `Host` e `Client` continuano a produrre e consumare `Vec<u8>`; il crate si occupa solo di trasporto.
- **`quinn` 0.11** come implementazione QUIC, con **`rustls` 0.23** e **`ring`**. Versioni bloccate (`quinn 0.11.8`, `quinn-proto 0.11.12`, `quinn-udp 0.5.13`, `tokio 1.47.1`, `rcgen 0.13.2`, `time 0.3.36`, `zeroize 1.8.1`) perché le ultime alzano l'MSRV oltre 1.82, il minimo dichiarato dal workspace e controllato dalla CI.
- **Datagrammi, non flussi**: ALPN `phantom/0`, buffer di ricezione e invio da 1 MiB, nessuno stream aperto. Il protocollo resta quello dell'ADR-004 (`Hello`/`HelloAck`, video, tocco, tastiera, `Close`); il trasporto non lo interpreta.
- **API bloccante**, pensata per app con thread propri: `HostEndpoint::bind(addr)` genera il certificato e aspetta con `accept(timeout)`; `ClientEndpoint::bind(addr)` e `connect(addr, server_name, pinned_certificate_der, timeout)`; `QuicLink::{send_datagram, recv_datagram(buf, timeout), max_datagram_size, close, is_closed}`. Ogni endpoint ha un runtime Tokio privato con due thread di lavoro; non serve un runtime nell'app.
- **Certificato fissato**: l'host genera un certificato autofirmato effimero a ogni avvio; il client riceve il DER fuori banda (per ora in memoria, nella fase 5 con l'accoppiamento) e accetta **solo** quel certificato. Niente PKI, niente verifica del nome, nessun certificato «simile»: il confronto è byte per byte. Un certificato sbagliato fa fallire il handshake.
- **Dimensione dei datagrammi**: l'app deve leggere `max_datagram_size()` e scegliere `frag_size = max − (HEADER_LEN + VIDEO_HEADER_LEN)`, con il limite `MAX_VIDEO_PAYLOAD`. In loopback, alla MTU iniziale di 1200, `max_datagram_size()` misura 1162 byte: la prova end-to-end usa 1122 byte di payload video e consegna 234 frame su 235. I 1200 byte dell'ADR-004 restano validi su UDP e su QUIC dopo la path MTU discovery; non vanno dati per scontati alla prima connessione.
- **Apertura della sessione**: `Hello`/`HelloAck` resta l'apertura e avviene dentro QUIC. Il core applica il cancello (T10): finché host e client non hanno concluso l'apertura non passano video, tocco, tastiera, ping né statistiche, e `Close` resta sempre valido.

## Alternative scartate

- **Flussi QUIC per tutto**: affidabili e ordinati, ma un pacchetto perso fermerebbe anche tocco e tastiera (head-of-line blocking), contro l'ADR-004.
- **UDP con cifratura fatta in casa**: reinventa TLS, chiavi e replay protection; QUIC li dà già e la fase 5 dovrà comunque accordare le chiavi.
- **Certificati di sistema o una CA locale**: su iPhone e Android richiede profili e installazioni manuali; per l'accoppiamento della fase 5 il certificato fissato è la base giusta.
- **`s2n-quic` o `quiche`**: dipendenze C/CMake più pesanti e meno portabili su iOS/Android di `ring`.
- **Runtime Tokio condiviso con l'app**: l'app Swift/Kotlin non ha Tokio; tenerlo dentro il crate evita di esporre il runtime e i suoi tipi attraverso UniFFI.

## Conseguenze

- `ring` compila C: la CI che fa `cargo check --workspace` per Android deve passare il clang e l'`ar` dell'NDK (il job lo fa); per iOS bastano Xcode e `xcrun`. Non è più vero che il job `cross` non ha bisogno dell'NDK.
- Le versioni delle dipendenze sono bloccate e vanno riviste insieme all'MSRV: un aggiornamento «normale» può richiedere Rust 1.85 o l'edizione 2024.
- Il crate non è ancora nei binding UniFFI né nell'app: l'integrazione richiede una prova su un dispositivo vero, che oggi non c'è. Il programma Swift di prova e il core continuano a girare su UDP.
- La prova end-to-end è in `core/crates/phantom-quic/tests/quic_loopback.rs`: due endpoint veri su UDP loopback, handshake, negoziazione, video, tocco, tastiera, chiusura e rifiuto di un certificato sbagliato.
- Prossimi passi: esporre il trasporto nei binding, collegare l'app, accoppiamento e identità stabile nella fase 5, adattamento automatico di `frag_size` al `max_datagram_size`.

## Aggiornamento v1.2.0 (2026-11-20): coda e controllo di congestione

Sul Redmi Note 11 Pro 5G in Wi-Fi la qualità crollava mentre su LDPlayer (cavo) era buona. Cause lato trasporto:

- **Cubic** (predefinito di quinn) dimezza la finestra a ogni perdita, e sul Wi-Fi di un telefono molte perdite sono
  casuali: il trasporto spediva meno di quanto l'encoder produceva e i datagrammi si accumulavano.
- **Coda di invio di 1 MiB**: a 12 Mbit/s sono circa 0,7 s di video in attesa; quando si riempie QUIC scarta i
  datagrammi più vecchi, il frame arriva rotto, il client chiede un keyframe (più grosso) e la coda cresce ancora.

Decisioni:

- Controllo di congestione **BBR** (`quinn::congestion::BbrConfig` in LAN, `noq_proto::congestion::Bbr3Config` nel P2P
  `iroh`). Il bitrate vero resta deciso da Phantom (ADR-007); BBR evita solo che il trasporto si strozzi da solo.
  Il BBR di quinn è marcato «sperimentale»: se dà problemi si torna a Cubic cambiando una riga in `transport_config()`.
- **Coda di invio di 512 KiB** (`DATAGRAM_SEND_BUFFER`): contiene ancora un keyframe 1080p ad alto bitrate (una coda
  più corta del keyframe ne scarterebbe l'inizio appena spedito), ma dimezza la latenza possibile.
- **Invio del frame in una chiamata** (`PhantomHost::send_frame_via`): frammenta e spedisce senza passare da
  JNA/Swift per ogni pacchetto, conta i datagrammi entrati in coda piena (`queue_overflows`) e in quel caso chiede
  subito un keyframe dal lato host (`Host::request_keyframe`, con la solita distanza minima).
- `frag_size` resta calcolato sulla MTU iniziale (1162 → 1122 byte di payload): il protocollo limita comunque i
  datagrammi a 1200 byte (ADR-004), quindi la path MTU discovery guadagnerebbe al massimo il 2%. Alzare
  `initial_mtu` rischierebbe di rompere la connessione su VPN/5G con MTU più piccola.
