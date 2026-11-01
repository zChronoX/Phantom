# ADR-006 · Recupero delle perdite video: parità XOR, NACK con budget, keyframe ripetuti

- Stato: accettata per la fase 1; parità Reed-Solomon e riordino adattivo rimandati alla fase 5
- Data: 2026-11-01

## Contesto

Il video H.264/H.265 dipende dai frame precedenti: un frame perso rende inutilizzabili i successivi fino al prossimo keyframe. Aspettare una ritrasmissione costa almeno un RTT (qualche ms su Wi-Fi) e non si può aspettare a lungo: a 120 fps ogni frame ha 8,3 ms.

## Decisione

- **Frammentazione** a pacchetti da al massimo 1200 byte, con intestazione video di 28 byte (frame, indice, conteggio, flag keyframe/config/parità/ritrasmissione, istante di cattura, gruppo di parità).
- **Un frammento di parità XOR ogni N di dati** (N predefinito 10, quindi circa il 10% di byte in più): recupera un solo frammento perso per gruppo senza aspettare nessuno. Misurato sul simulatore con il 2% di perdita casuale: latenza p95 6,1 ms con parità contro 18,0 ms senza; i NACK scendono da 257 a 19.
- **NACK** per ciò che la parità non copre, dopo un breve ritardo dal primo frammento (3 ms). I «buchi» (mancanti con indice minore dell'ultimo arrivato) si chiedono allo scadere del ritardo; i mancanti in coda solo se il frame non riceve più nulla da altrettanto. Per lo stesso frame si ripete la richiesta non prima di 8 ms o 1,5 RTT, al massimo 3 volte.
- **Budget di ritrasmissione**: al massimo il 50% della velocità di invio recente (minimo 1 Mbps), con un secchio da 64 KB. Un collegamento saturo non deve essere peggiorato dalle ritrasmissioni (provato: sul collegamento saturo senza adattamento le ritrasmissioni restano al 49% dei datagrammi primari, mai a valanga).
- **Consegna in ordine** con due scadenze: un frame che non riceve nulla da 60 ms, o che è aperto da più di 400 ms, viene scartato. Allora si scartano i successivi fino al prossimo keyframe e si chiede un keyframe. Un keyframe completo «salta avanti» e rende obsoleto tutto ciò che lo precede.
- **La richiesta di keyframe si ripete ogni 500 ms (almeno 2 RTT) finché il keyframe non arriva.** Lezione dalla prova a raffiche: la richiesta partiva una volta sola, e se il keyframe richiesto andava perso a sua volta l'immagine restava ferma fino al keyframe periodico, per sempre se l'encoder non ne produce. L'host rispetta una distanza minima di 300 ms tra keyframe su richiesta.
- Il ricevitore non conta come perdita di rete i frame scartati in attesa del keyframe (sono la conseguenza di una perdita già contata): altrimenti una sola raffica falserebbe le statistiche usate dal controllo del bitrate.

## Verifiche

Oltre alle prove sul simulatore, due prove di proprietà sul ricevitore: con frammenti ostili (campi estremi, incoerenti tra loro, duplicati, frame fuori ordine, tempo che salta o torna indietro) non va in panico e non consegna due volte lo stesso frame né in disordine; con schemi casuali di perdite, duplicati e riordino ogni frame consegnato è identico all'originale, anche quando un frammento è stato ricostruito con la parità.

## Limiti noti

- Una raffica più lunga di qualche frame costa sempre un keyframe e circa 300 ms di immagine ferma. Per ridurla servono invalidazione dei frame di riferimento o intra-refresh periodico nell'encoder (fase 2/5).
- Con riordino forte (prova ostile, jitter indipendente) le ritrasmissioni inutili arrivano al tetto del budget (45% dei byte primari). Il ritardo del NACK dovrebbe adattarsi allo scostamento di arrivo misurato (fase 5).
- La parità XOR recupera un solo frammento per gruppo; per perdite a raffica serve Reed-Solomon o parità su più gruppi (fase 5).
