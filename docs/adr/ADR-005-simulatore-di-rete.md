# ADR-005 · Simulatore di rete deterministico a tempo virtuale

- Stato: accettata
- Data: 2026-11-01

## Contesto

Il comportamento con perdite, jitter, riordino e collegamenti lenti decide la qualità dell'esperienza, ma sul Wi-Fi vero è impossibile da ripetere: ogni prova è diversa e sul telefono costa minuti. Servono prove che girino in millisecondi, diano sempre lo stesso risultato e si possano eseguire in CI su tre sistemi operativi.

## Decisione

- **Tempo virtuale**: nessun componente del core legge l'orologio né dorme; il tempo è un argomento (`now_us`). Host, client, ricevitore video, stimatore di orologio e controllo del bitrate sono macchine a stati pure. Il simulatore consegna i pacchetti quando il tempo avanza.
- **Generatore pseudo-casuale con seme**: stesso seme, stesso risultato, su ogni piattaforma (provato).
- Cosa modella, per ciascun verso: ritardo fisso e jitter, perdita casuale, **perdite a raffica a tempo continuo** (due stati, buono e cattivo, con durate medie in microsecondi), riordino e duplicazione, collo di bottiglia con banda e coda finite (tail drop), e cambi di configurazione durante la prova (interruzioni, allontanamento dal router).
- **Le raffiche sono a tempo, non a pacchetti.** Un primo modello a pacchetti allungava le raffiche quando il bitrate scendeva (meno pacchetti al secondo per attraversare lo stesso numero di stati) e faceva sembrare dannosa qualunque riduzione del bitrate. Un'interferenza sul Wi-Fi dura quanto dura, a prescindere da quanto si sta mandando.
- **Il jitter conserva l'ordine (FIFO)** per impostazione predefinita, come una coda reale: un pacchetto in ritardo ritarda quelli dietro. Il riordino vero è un evento separato (`reorder`). Con jitter indipendente per pacchetto (`fifo: false`) il ricevitore vedeva buchi ovunque e chiedeva ritrasmissioni a vuoto (in Wi-Fi buono 317 ritrasmissioni su 708 frame, contro 1 con il jitter FIFO): un artefatto del modello, non un problema del Wi-Fi. Resta come prova di stress, con un limite sulle ritrasmissioni.
- Le prove su **UDP reale in loopback** (thread, socket, orologi di processi diversi) affiancano il simulatore per scoprire ciò che il simulatore non può: dimensioni dei datagrammi, timer, scheduler. Le soglie sono larghe di proposito.

## Conseguenze

Ogni regressione di comportamento di rete si vede in CI in meno di un secondo. Il simulatore non sostituisce il Wi-Fi vero: i numeri assoluti (latenze, soglie del controllo del bitrate) vanno riverificati con misure reali nella fase 2 e le configurazioni (`good_wifi`, `bad_wifi`) vanno ritarate sui dati del POCO e dell'iPhone. Lo stesso simulatore servirà per le prove di congestione e di equità della fase 5.
