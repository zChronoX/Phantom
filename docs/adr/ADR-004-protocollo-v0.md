# ADR-004 · Protocollo di rete v0: datagrammi autonomi, tocco a stato completo

- Stato: accettata, da rivedere alla fine della fase 2 (quando il trasporto diventa QUIC)
- Data: 2026-11-01

## Contesto

Host (Android) e client (iPhone) parlano a oltre 100 messaggi al secondo in ogni verso: video a 120 fps, tocco a 240 Hz, ping per la sincronia, richieste di ritrasmissione. Il parser è esposto alla rete e gira dentro app mobili: un panico o una lettura fuori limiti non sono accettabili. Il trasporto di produzione sarà QUIC con datagrammi (fase 2) e la cifratura arriva nella fase 5; il formato non deve dipendere da nessuno dei due.

## Decisione

- **Un datagramma è un'unità autonoma di al massimo 1200 byte**: sta nel MTU minimo di QUIC senza frammentazione IP, e funziona identico su UDP semplice (usato nelle prove).
- Intestazione comune di 12 byte (magic `0x50`, versione, tipo, flags, `conn_id`, `seq`), tutto **little-endian**, campi a larghezza fissa. Versione 0 finché non esiste un dispositivo in produzione.
- **Nessun panico**: ogni lettura è controllata; dati malformati danno un errore, mai un crash. Un messaggio a lunghezza fissa con byte in eccesso è rifiutato. Provato con proptest: byte qualsiasi, e intestazioni valide seguite da byte qualsiasi, non causano mai un panico; tocco e video fanno andata e ritorno senza perdite.
- **Il tocco è lo stato completo delle dita** (fino a 10, con un numero di sequenza), non una sequenza di eventi. Se un datagramma si perde, il successivo contiene già tutto; chi arriva in ritardo e fuori ordine si ignora. A 240 Hz il costo è trascurabile.
- Il video è frammentato con un frammento di parità XOR opzionale per gruppo (campo `fec_group`); i dettagli del recupero sono nell'ADR-006.
- Le statistiche del client (`Stats`) usano contatori cumulativi: l'host ne fa le differenze, quindi la perdita di un messaggio non falsa nulla. Contengono byte ricevuti, frame consegnati e persi, frame salvati dalla parità, ritardo di coda e RTT (usati dall'ADR-007).

- **Gli orari che arrivano dalla rete non sono affidabili.** I conti sugli orologi dell'altro lato sono a 128 bit con saturazione (una risposta con orari assurdi non può far andare in overflow nulla), i giri di ping oltre i 60 s si scartano, e il client risponde solo ai ping che ha mandato: annota numero e istante di partenza, usa il proprio e ignora risposte duplicate o inventate. Prove di proprietà con host e client ostili, in compilazione di debug (con controllo degli overflow), lo verificano; hanno trovato proprio il difetto degli orari assurdi.

## Alternative scartate

Protobuf o simili (allocazioni, parser generato difficile da rendere privo di panici, byte in più sul percorso caldo); eventi di tocco incrementali (una perdita lascia un dito «premuto» per sempre); un flusso affidabile per i comandi (head-of-line blocking: un pacchetto perso ferma anche il tocco successivo).

## Conseguenze

Il formato è nostro e va mantenuto in due linguaggi di binding (UniFFI per Swift e Kotlin dalla fase 1, seconda parte). Ogni campo nuovo richiede un test di andata e ritorno. La versione 0 non ha negoziazione: la fase 2 introdurrà `Hello`/`HelloAck` completi con numero di versione e funzioni opzionali.
