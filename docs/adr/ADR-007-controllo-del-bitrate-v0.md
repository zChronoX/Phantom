# ADR-007 · Controllo del bitrate v0: scendere solo se c'è davvero coda

- Stato: accettata come soluzione provvisoria; il controllo completo è nella fase 5
- Data: 2026-11-01

## Contesto

Su un collegamento saturo, senza adattamento, il flusso collassa: sul simulatore con 12 Mbps contro i circa 22 offerti, i frame consegnati sono lo 0%. Serve almeno un controllo che trovi la capacità. Ma un primo controllo basato solo sulle perdite di frame abbassava il bitrate al minimo (2 Mbps) anche su un Wi-Fi scarico che perdeva pacchetti a raffiche: qualità persa per nulla, perché meno bit non curano le interferenze.

## Decisione

Tre segnali, riferiti dal client ogni 200 ms (messaggio `Stats`, contatori cumulativi):

1. **Perdita di frame** per cause di rete.
2. **Byte ricevuti**, da cui l'host ricava la velocità con cui il collegamento sta consegnando.
3. **Ritardo di coda** = RTT recente (mediana degli ultimi 3 ping, 10 al secondo) meno il 25° percentile dei RTT degli ultimi 16 s. Il percentile, e non il minimo, perché il jitter normale del Wi-Fi sposta ogni RTT di qualche ms e il minimo lo scambierebbe per coda. I ping viaggiano nella stessa coda del video.

Regole:

- **Congestione** = perdite ≥ 4% con coda ≥ 15 ms, oppure coda ≥ 60 ms da sola. Si scende subito a 0,82 × la velocità di consegna (non meno della metà in un colpo) e si ricorda un **tetto** a 0,82 × la velocità di consegna.
- **Perdite pesanti** (≥ 30%) senza coda alta a spiegarle o senza misura della coda: si scende di un passo fisso (× 0,75), senza tetto, perché può essere una raffica.
- **Coda misurata e bassa (< 8 ms) vince su tutto**: si può salire anche con perdite.
- Si parte a 6 Mbps e si sale del 25% per finestra (**startup**) finché non c'è congestione, così il RTT di riferimento si misura a collegamento scarico; poi +4% (più 50 kbps) per finestra, dopo almeno 1 s dall'ultima riduzione.
- **Il tetto si rialza del 10% ogni 4 s**; se la sonda fallisce (nuova congestione con il tetto ancora in vigore) l'intervallo raddoppia fino a 32 s; il tetto si dimentica dopo almeno 60 s tranquilli. Perché: ogni sonda oltre la capacità costa frame persi, un keyframe e circa 300 ms di immagine ferma. Modello a fluido: 9 congestioni in 5 minuti invece di 116, per il 2% di bitrate medio in meno.

Risultati sul simulatore (semi fissi; i keyframe su richiesta sono grossi come quelli veri, perché li produce l'encoder simulato): Wi-Fi buono 100% dei frame, bitrate invariato; Wi-Fi disturbato 98,8%, bitrate invariato; raffiche (9% del tempo al 95% di perdita) 84,0%, e 99% nell'ultimo terzo, bitrate invariato; collegamento saturo 91,5% dei frame (100% nell'ultimo terzo) a 9,4 Mbps su circa 10,5 utili, 2 riduzioni.

## Limiti noti

- Nessuna sonda di banda attiva né equità con altro traffico sul collegamento.
- Con buffer cortissimi (meno di 15 ms) l'eccesso si vede solo con perdite ≥ 30%.
- Se il collegamento è già saturo da altri quando parte la sessione, il RTT di riferimento è gonfiato e la coda sottostimata (resta la regola delle perdite pesanti).
- Soglie e fattori sono tarati sul simulatore: vanno verificati con misure sul Wi-Fi vero (fase 2) e sostituiti da un controllo basato sul gradiente del ritardo (fase 5).
- L'encoder deve rispettare il bitrate richiesto: su Android significa `setParameters(KEY_BIT_RATE)` al volo (da verificare sul POCO).
