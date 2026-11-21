//! Buffer anti-jitter per l'audio (lato client).
//!
//! L'audio arriva come pacchetti AAC da 1024 campioni (21,3 ms a 48 kHz) dentro datagrammi non affidabili, nella
//! stessa coda del video: possono arrivare in ritardo, a raffiche, in disordine, doppi (l'host manda ogni pacchetto
//! due volte per coprire le perdite isolate) o mai. Riprodurli appena arrivano, come faceva l'app, vuol dire che
//! ogni pacchetto in ritardo diventa un buco o un salto (il gracchiare) e ogni perdita accorcia la linea del tempo.
//!
//! Qui ogni pacchetto riceve una posizione fissa sulla linea del tempo, ricavata dall'istante di cattura dell'host
//! (`(capture_ts - primo) / durata del pacchetto`, arrotondato): perdite e doppioni non spostano più gli altri.
//! La riproduzione parte con un ritardo iniziale (`initial_delay`) che lascia ai pacchetti il tempo di arrivare;
//! un pacchetto mancante si aspetta finché la sua ora non è quasi arrivata (`skip_margin`), poi si salta (silenzio
//! di un pacchetto). Se i pacchetti arrivano troppo tardi più volte di fila il ritardo cresce di un passo; se il
//! buffer si gonfia troppo (orologi di Android e iPhone leggermente diversi) si torna al ritardo giusto.
//!
//! Il buffer non conosce l'orologio di riproduzione: l'app gli passa la posizione attuale del riproduttore
//! (`playhead`, in campioni) e lui risponde con i pacchetti da accodare e, se serve, la nuova posizione da impostare
//! (`resync`). Tutte le posizioni sono in campioni: un pacchetto di indice `i` parte da `i * frame_samples`.

use std::collections::{BTreeMap, VecDeque};

/// Parametri del buffer.
#[derive(Clone, Copy, Debug)]
pub struct JitterConfig {
    /// Frequenza di campionamento (Hz).
    pub sample_rate: u32,
    /// Campioni per pacchetto (1024 per AAC-LC).
    pub frame_samples: u32,
    /// Ritardo di riproduzione iniziale (µs).
    pub initial_delay_us: u64,
    /// Ritardo massimo a cui può crescere (µs).
    pub max_delay_us: u64,
    /// Di quanto cresce il ritardo dopo troppi ritardatari (µs).
    pub delay_step_us: u64,
    /// Un pacchetto mancante si salta quando mancano meno di questi µs alla sua riproduzione.
    pub skip_margin_us: u64,
    /// Quanti ritardatari in `late_window_us` fanno crescere il ritardo.
    pub late_threshold: usize,
    /// Finestra in cui si contano i ritardatari (µs, orologio locale).
    pub late_window_us: u64,
    /// Oltre `ritardo + excess_us` di audio in coda per `excess_hold_us` si torna al ritardo giusto.
    pub excess_us: u64,
    /// Per quanto l'eccesso deve durare prima di intervenire (µs, orologio locale).
    pub excess_hold_us: u64,
    /// Un salto di indice oltre questi µs (avanti o indietro) è una discontinuità: si riparte da capo.
    pub discontinuity_us: u64,
    /// Pacchetti tenuti al massimo in memoria.
    pub max_pending: usize,
    /// Scostamento massimo della velocità di riproduzione per correggere la deriva (0,005 = ±0,5%).
    pub max_rate_offset: f64,
    /// In quanti secondi si vorrebbe riassorbire lo scarto tra audio in coda e ritardo voluto.
    pub rate_correction_s: f64,
}

impl JitterConfig {
    /// Valori per AAC-LC a `sample_rate` Hz su una rete locale o mobile.
    pub fn aac(sample_rate: u32) -> Self {
        JitterConfig {
            sample_rate: sample_rate.max(8_000),
            frame_samples: 1024,
            initial_delay_us: 80_000,
            max_delay_us: 240_000,
            delay_step_us: 30_000,
            skip_margin_us: 12_000,
            late_threshold: 3,
            late_window_us: 3_000_000,
            excess_us: 200_000,
            excess_hold_us: 3_000_000,
            discontinuity_us: 3_000_000,
            max_pending: 256,
            max_rate_offset: 0.005,
            rate_correction_s: 4.0,
        }
    }

    fn us_to_samples(&self, us: u64) -> i64 {
        (us as i128 * i128::from(self.sample_rate) / 1_000_000) as i64
    }

    fn frame_us(&self) -> f64 {
        f64::from(self.frame_samples) * 1_000_000.0 / f64::from(self.sample_rate)
    }
}

/// Un pacchetto pronto per il riproduttore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyAudio {
    /// Posizione del primo campione sulla linea del tempo (campioni).
    pub pts_samples: i64,
    /// Dati codificati.
    pub data: Vec<u8>,
}

/// Risposta di [`AudioJitter::poll`].
#[derive(Clone, Debug, PartialEq)]
pub struct JitterPoll {
    /// Se presente, l'app deve svuotare il riproduttore e portare il suo orologio a questa posizione (campioni)
    /// prima di accodare `frames`.
    pub resync_playhead: Option<i64>,
    /// Pacchetti da accodare, in ordine.
    pub frames: Vec<ReadyAudio>,
    /// Velocità di riproduzione consigliata (1,0 = normale, al massimo ±0,5%): corregge piano la differenza tra
    /// l'orologio audio di Android e quello dell'iPhone, senza i salti di una risincronizzazione.
    pub playback_rate: f64,
}

impl Default for JitterPoll {
    fn default() -> Self {
        JitterPoll { resync_playhead: None, frames: Vec::new(), playback_rate: 1.0 }
    }
}

/// Contatori.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JitterStats {
    /// Pacchetti ricevuti (doppioni compresi).
    pub received: u64,
    /// Doppioni scartati (la copia di ridondanza di un pacchetto già arrivato).
    pub duplicates: u64,
    /// Pacchetti arrivati dopo che il loro posto era già stato saltato.
    pub late: u64,
    /// Posti saltati perché il pacchetto non è arrivato in tempo (un pacchetto di silenzio ciascuno).
    pub skipped: u64,
    /// Risincronizzazioni (avvio compreso).
    pub resyncs: u64,
    /// Ritardo di riproduzione attuale (µs).
    pub delay_us: u64,
}

/// Il buffer anti-jitter.
#[derive(Clone, Debug)]
pub struct AudioJitter {
    cfg: JitterConfig,
    base_ts_us: Option<u64>,
    pending: BTreeMap<i64, Vec<u8>>,
    next: Option<i64>,
    highest: Option<i64>,
    recently_skipped: VecDeque<i64>,
    late_at: VecDeque<u64>,
    excess_since: Option<u64>,
    delay_us: u64,
    needs_resync: bool,
    /// Media mobile dell'audio in coda (campioni), per la correzione di velocità.
    buffered_avg: Option<f64>,
    rate: f64,
    stats: JitterStats,
}

impl AudioJitter {
    /// Crea il buffer.
    pub fn new(cfg: JitterConfig) -> Self {
        AudioJitter {
            cfg,
            base_ts_us: None,
            pending: BTreeMap::new(),
            next: None,
            highest: None,
            recently_skipped: VecDeque::new(),
            late_at: VecDeque::new(),
            excess_since: None,
            delay_us: cfg.initial_delay_us,
            needs_resync: true,
            buffered_avg: None,
            rate: 1.0,
            stats: JitterStats { delay_us: cfg.initial_delay_us, ..JitterStats::default() },
        }
    }

    /// Contatori.
    pub fn stats(&self) -> JitterStats {
        JitterStats { delay_us: self.delay_us, ..self.stats }
    }

    /// Dimentica tutto (nuova sessione o nuovo formato audio); il ritardo torna quello iniziale.
    pub fn reset(&mut self) {
        let stats = self.stats;
        *self = AudioJitter::new(self.cfg);
        self.stats = JitterStats { delay_us: self.cfg.initial_delay_us, ..stats };
    }

    fn frame_pts(&self, index: i64) -> i64 {
        index * i64::from(self.cfg.frame_samples)
    }

    fn restart_timeline(&mut self, capture_ts_us: u64) {
        self.base_ts_us = Some(capture_ts_us);
        self.pending.clear();
        self.next = None;
        self.highest = None;
        self.recently_skipped.clear();
        self.excess_since = None;
        self.needs_resync = true;
    }

    /// Aggiorna la velocità consigliata: proporzionale allo scarto tra audio in coda (media mobile) e ritardo voluto,
    /// limitata a `max_rate_offset` e arrotondata a passi di 0,05% per non cambiarla di continuo.
    fn update_rate(&mut self, buffered: i64, target: i64) {
        let avg = match self.buffered_avg {
            Some(a) => a * 0.98 + buffered as f64 * 0.02,
            None => buffered as f64,
        };
        self.buffered_avg = Some(avg);
        let error = avg - target as f64;
        let offset = (error / (f64::from(self.cfg.sample_rate) * self.cfg.rate_correction_s))
            .clamp(-self.cfg.max_rate_offset, self.cfg.max_rate_offset);
        self.rate = 1.0 + (offset / 0.0005).round() * 0.0005;
    }

    /// Un pacchetto audio (non di configurazione) arrivato dall'host. `now_us` è l'orologio locale.
    pub fn push(&mut self, now_us: u64, capture_ts_us: u64, data: Vec<u8>) {
        self.stats.received += 1;
        let base = *self.base_ts_us.get_or_insert(capture_ts_us);
        let frame_us = self.cfg.frame_us();
        let mut index = ((capture_ts_us as f64 - base as f64) / frame_us).round() as i64;

        let jump_frames = (self.cfg.discontinuity_us as f64 / frame_us) as i64;
        let reference = self.highest.or(self.next);
        if let Some(r) = reference {
            if index > r + jump_frames || index < r - jump_frames {
                // l'host ha riavviato la cattura o l'orologio ha fatto un salto: nuova linea del tempo
                self.restart_timeline(capture_ts_us);
                index = 0;
            }
        }

        if let Some(n) = self.next {
            if index < n && !self.pending.contains_key(&index) {
                if self.recently_skipped.contains(&index) {
                    // lo si tiene comunque: una risincronizzazione che torna indietro lo può ancora usare
                    self.stats.late += 1;
                    self.late_at.push_back(now_us);
                    self.pending.insert(index, data);
                } else {
                    self.stats.duplicates += 1;
                }
                return;
            }
        }
        if self.pending.contains_key(&index) {
            self.stats.duplicates += 1;
            return;
        }
        self.pending.insert(index, data);
        self.highest = Some(self.highest.map_or(index, |h| h.max(index)));
        while self.pending.len() > self.cfg.max_pending {
            self.pending.pop_first();
        }
    }

    /// Chiede i pacchetti da accodare. `playhead_samples` è la posizione attuale del riproduttore (`None` finché
    /// non è partito). Se la risposta ha `resync_playhead`, l'app svuota il riproduttore e imposta il suo orologio lì.
    pub fn poll(&mut self, now_us: u64, playhead_samples: Option<i64>) -> JitterPoll {
        let mut out = JitterPoll::default();
        let Some(highest) = self.highest else { return out };

        while self.late_at.front().is_some_and(|&t| now_us.saturating_sub(t) > self.cfg.late_window_us) {
            self.late_at.pop_front();
        }

        let delay_samples = self.cfg.us_to_samples(self.delay_us);
        let newest_end = self.frame_pts(highest + 1);
        let mut playhead = playhead_samples;

        // 1) troppi ritardatari: il ritardo non basta, si allunga di un passo e si riparte da lì
        if !self.needs_resync && self.late_at.len() >= self.cfg.late_threshold && self.delay_us < self.cfg.max_delay_us
        {
            self.delay_us = (self.delay_us + self.cfg.delay_step_us).min(self.cfg.max_delay_us);
            self.late_at.clear();
            self.needs_resync = true;
        }

        // 2) troppo audio in coda per troppo tempo (orologi che divergono, raffica dopo un blocco): si torna al ritardo
        if let (false, Some(p)) = (self.needs_resync, playhead) {
            let buffered = newest_end - p;
            let limit = delay_samples + self.cfg.us_to_samples(self.cfg.excess_us);
            if buffered > limit {
                let since = *self.excess_since.get_or_insert(now_us);
                if now_us.saturating_sub(since) >= self.cfg.excess_hold_us {
                    self.needs_resync = true;
                }
            } else {
                self.excess_since = None;
            }
            // sono arrivati pacchetti nuovi ma il riproduttore è già oltre: lo stream si era fermato (rete bloccata),
            // si riparte dai nuovi. Se invece non arriva più niente (fine dello stream) non si fa nulla.
            if buffered < -self.cfg.us_to_samples(self.cfg.skip_margin_us) && self.next.is_some_and(|n| n <= highest) {
                self.needs_resync = true;
            }
        }

        if self.needs_resync || playhead.is_none() {
            let delay_samples = self.cfg.us_to_samples(self.delay_us);
            let frame = i64::from(self.cfg.frame_samples);
            // il riproduttore riparte `delay` prima della fine del pacchetto più nuovo, allineato all'inizio di un
            // pacchetto (anche prima del primo: è il silenzio iniziale che lascia ai pacchetti il tempo di arrivare)
            let start_playhead = (newest_end - delay_samples).div_euclid(frame) * frame;
            let first_kept = self.pending.keys().next().copied().unwrap_or(highest);
            let start_index = (start_playhead / frame).max(first_kept);
            self.next = Some(start_index);
            self.excess_since = None;
            self.needs_resync = false;
            self.buffered_avg = None;
            self.rate = 1.0;
            self.stats.resyncs += 1;
            out.resync_playhead = Some(start_playhead);
            playhead = Some(start_playhead);
        }

        let p = playhead.unwrap_or(0);
        if out.resync_playhead.is_none() {
            self.update_rate(newest_end - p, self.cfg.us_to_samples(self.delay_us));
        }
        out.playback_rate = self.rate;
        let margin = self.cfg.us_to_samples(self.cfg.skip_margin_us);
        let Some(mut next) = self.next else { return out };
        while next <= highest {
            if self.frame_pts(next + 1) <= p {
                // è già tutto nel passato del riproduttore: accodarlo non serve, si conta come buco
                self.stats.skipped += 1;
                next += 1;
            } else if let Some(data) = self.pending.get(&next) {
                out.frames.push(ReadyAudio { pts_samples: self.frame_pts(next), data: data.clone() });
                next += 1;
            } else if p + margin >= self.frame_pts(next) {
                // non è arrivato e la sua ora è quasi arrivata: si salta
                self.stats.skipped += 1;
                self.recently_skipped.push_back(next);
                if self.recently_skipped.len() > 64 {
                    self.recently_skipped.pop_front();
                }
                next += 1;
            } else {
                break;
            }
        }
        self.next = Some(next);

        // si tengono i pacchetti già consegnati finché non sono stati suonati: una risincronizzazione che torna
        // indietro (ritardo più lungo) li può riconsegnare
        let played_before = p.div_euclid(i64::from(self.cfg.frame_samples)) - 1;
        while self.pending.first_key_value().is_some_and(|(&k, _)| k < played_before && k < next) {
            self.pending.pop_first();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn ts(index: i64) -> u64 {
        1_000_000_000 + (index as f64 * 1024.0 * 1_000_000.0 / f64::from(SR)).round() as u64
    }

    fn pts(index: i64) -> i64 {
        index * 1024
    }

    fn aligned(samples: i64) -> i64 {
        samples.div_euclid(1024) * 1024
    }

    fn indices(poll: &JitterPoll) -> Vec<i64> {
        poll.frames.iter().map(|f| f.pts_samples / 1024).collect()
    }

    /// Il primo pacchetto avvia la riproduzione con il ritardo iniziale; i successivi escono in ordine.
    #[test]
    fn starts_with_initial_delay_and_delivers_in_order() {
        let cfg = JitterConfig::aac(SR);
        let mut j = AudioJitter::new(cfg);
        assert_eq!(j.poll(0, None), JitterPoll::default(), "vuoto: niente da fare");
        j.push(0, ts(0), vec![0]);
        let first = j.poll(0, None);
        let delay = cfg.us_to_samples(cfg.initial_delay_us);
        // un solo pacchetto: il riproduttore parte da prima del pacchetto, così suona dopo il ritardo iniziale
        assert_eq!(first.resync_playhead, Some(aligned(pts(1) - delay)));
        assert!(first.resync_playhead.unwrap_or(0) <= pts(1) - delay);
        assert_eq!(indices(&first), vec![0]);

        for i in 1..5 {
            j.push(i as u64 * 21_000, ts(i), vec![i as u8]);
        }
        let poll = j.poll(100_000, Some(pts(1)));
        assert_eq!(poll.resync_playhead, None);
        assert_eq!(indices(&poll), vec![1, 2, 3, 4]);
    }

    /// Con un buffer già pieno all'avvio la riproduzione parte `initial_delay` prima dell'ultimo pacchetto.
    #[test]
    fn initial_playhead_leaves_the_configured_delay() {
        let cfg = JitterConfig::aac(SR);
        let mut j = AudioJitter::new(cfg);
        for i in 0..10 {
            j.push(0, ts(i), vec![0]);
        }
        let poll = j.poll(0, None);
        let expected = aligned(pts(10) - cfg.us_to_samples(cfg.initial_delay_us));
        assert_eq!(poll.resync_playhead, Some(expected));
        assert_eq!(poll.frames.first().map(|f| f.pts_samples / 1024), Some(expected / 1024));
        assert_eq!(poll.frames.last().map(|f| f.pts_samples / 1024), Some(9));
    }

    /// Disordine e doppioni (la copia di ridondanza) non cambiano la linea del tempo.
    #[test]
    fn reorders_and_drops_duplicates() {
        let mut j = AudioJitter::new(JitterConfig::aac(SR));
        j.push(0, ts(0), vec![0]);
        let _ = j.poll(0, None);
        j.push(1, ts(2), vec![2]);
        j.push(2, ts(1), vec![1]);
        j.push(3, ts(1), vec![1]);
        j.push(4, ts(2), vec![2]);
        let poll = j.poll(5, Some(0));
        assert_eq!(indices(&poll), vec![1, 2]);
        assert_eq!(j.stats().duplicates, 2);
        // copia tardiva di un pacchetto già consegnato: doppione, non ritardatario
        j.push(6, ts(1), vec![1]);
        assert_eq!(j.stats().duplicates, 3);
        assert_eq!(j.stats().late, 0);
    }

    /// Un pacchetto mancante si aspetta finché c'è tempo, poi si salta; la ridondanza arrivata in tempo lo salva.
    #[test]
    fn waits_for_missing_packet_then_skips_it() {
        let cfg = JitterConfig::aac(SR);
        let mut j = AudioJitter::new(cfg);
        j.push(0, ts(0), vec![0]);
        let _ = j.poll(0, None);
        j.push(1, ts(2), vec![2]);
        let poll = j.poll(1, Some(0));
        assert!(poll.frames.is_empty(), "il pacchetto 1 manca ma c'è ancora tempo: si aspetta");

        // la copia di ridondanza arriva in tempo
        j.push(2, ts(1), vec![1]);
        assert_eq!(indices(&j.poll(3, Some(100))), vec![1, 2]);

        // ora manca il 3 e il riproduttore è quasi arrivato alla sua ora: si salta
        j.push(4, ts(4), vec![4]);
        let near = pts(3) - cfg.us_to_samples(cfg.skip_margin_us) + 1;
        assert_eq!(indices(&j.poll(5, Some(near))), vec![4]);
        assert_eq!(j.stats().skipped, 1);

        // il 3 arriva quando il suo posto è già passato: ritardatario
        j.push(6, ts(3), vec![3]);
        assert_eq!(j.stats().late, 1);
    }

    /// Troppi ritardatari allungano il ritardo e fanno ripartire il riproduttore più indietro.
    #[test]
    fn repeated_late_packets_increase_delay() {
        let cfg = JitterConfig::aac(SR);
        let mut j = AudioJitter::new(cfg);
        j.push(0, ts(0), vec![0]);
        let _ = j.poll(0, None);
        let mut playhead = 0;
        let mut index = 1;
        for round in 0..3 {
            // manca `index`, arriva `index + 1`; il riproduttore supera `index`, che arriva dopo
            j.push(10 + round, ts(index + 1), vec![1]);
            playhead = pts(index) + 1;
            let _ = j.poll(10 + round, Some(playhead));
            j.push(10 + round, ts(index), vec![0]);
            index += 2;
        }
        assert_eq!(j.stats().late, 3);
        let poll = j.poll(20, Some(playhead));
        assert!(poll.resync_playhead.is_some(), "si riparte");
        assert_eq!(j.stats().delay_us, cfg.initial_delay_us + cfg.delay_step_us);
        let expected = aligned(pts(index) - cfg.us_to_samples(j.stats().delay_us));
        assert_eq!(poll.resync_playhead, Some(expected));
        // i ritardatari tenuti da parte tornano utili: si riparte da un punto in cui ci sono di nuovo tutti
        assert!(!poll.frames.is_empty());
    }

    /// Un buffer che resta troppo pieno (orologi che divergono) viene riportato al ritardo giusto.
    #[test]
    fn excess_buffer_is_trimmed_after_hold_time() {
        let cfg = JitterConfig::aac(SR);
        let mut j = AudioJitter::new(cfg);
        for i in 0..40 {
            j.push(0, ts(i), vec![0]);
        }
        let first = j.poll(0, None);
        assert!(first.resync_playhead.is_some());
        // il riproduttore è rimasto fermo all'inizio mentre il buffer contiene quasi un secondo
        let behind = Some(0);
        assert!(j.poll(1_000, behind).resync_playhead.is_none(), "eccesso appena visto: si aspetta");
        let poll = j.poll(1_000 + cfg.excess_hold_us, behind);
        let expected = aligned(pts(40) - cfg.us_to_samples(cfg.initial_delay_us));
        assert_eq!(poll.resync_playhead, Some(expected));
    }

    /// Un salto enorme nell'istante di cattura (host riavviato) fa ripartire la linea del tempo.
    #[test]
    fn discontinuity_restarts_timeline() {
        let mut j = AudioJitter::new(JitterConfig::aac(SR));
        j.push(0, ts(0), vec![0]);
        let _ = j.poll(0, None);
        j.push(1, ts(0) + 60_000_000, vec![9]);
        let poll = j.poll(2, Some(0));
        assert_eq!(poll.resync_playhead, Some(aligned(pts(1) - JitterConfig::aac(SR).us_to_samples(80_000))));
        assert_eq!(indices(&poll), vec![0]);
        assert_eq!(poll.frames[0].data, vec![9]);
    }

    /// Istanti di cattura un po' imprecisi (millisecondi) finiscono comunque al posto giusto.
    #[test]
    fn jittery_capture_timestamps_snap_to_frame_grid() {
        let mut j = AudioJitter::new(JitterConfig::aac(SR));
        j.push(0, ts(0), vec![0]);
        let _ = j.poll(0, None);
        j.push(1, ts(1) + 4_000, vec![1]);
        j.push(2, ts(2) - 6_000, vec![2]);
        j.push(3, ts(3) + 9_000, vec![3]);
        assert_eq!(indices(&j.poll(4, Some(0))), vec![1, 2, 3]);
        assert_eq!(j.stats().duplicates, 0);
    }

    /// Simulazione di 30 s di Wi-Fi disturbato: ritardi variabili fino a ~70 ms, 5% di perdite per copia, ogni
    /// pacchetto spedito due volte (la seconda insieme al successivo). Il riproduttore avanza a velocità reale.
    #[test]
    fn noisy_network_simulation_keeps_audio_continuous() {
        let cfg = JitterConfig::aac(SR);
        let mut j = AudioJitter::new(cfg);
        let period_us = 1024.0 * 1_000_000.0 / f64::from(SR);
        let mut seed: u64 = 0x5eed_1234_abcd_0001;
        let mut rnd = move || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let frames = 1_400; // ~30 s
        let mut arrivals: Vec<(u64, i64)> = Vec::new();
        for i in 0..frames {
            let sent = i as f64 * period_us;
            for copy in 0..2 {
                if rnd() < 0.05 {
                    continue;
                }
                // ritardo base 4 ms, coda esponenziale (media 12 ms) limitata a 70 ms
                let jitter = (-12_000.0 * (1.0 - rnd()).ln()).min(66_000.0);
                let t = sent + f64::from(copy) * period_us + 4_000.0 + jitter;
                arrivals.push((t as u64, i));
            }
        }
        arrivals.sort();

        let mut anchor: Option<(u64, i64)> = None; // (istante locale, posizione)
        let mut enqueued: Vec<i64> = Vec::new();
        let mut a = 0;
        let end = (frames as f64 * period_us) as u64 + 300_000;
        let mut now = 0u64;
        while now < end {
            while a < arrivals.len() && arrivals[a].0 <= now {
                j.push(arrivals[a].0, ts(arrivals[a].1), vec![0]);
                a += 1;
            }
            let playhead = anchor.map(|(t0, p0)| p0 + ((now - t0) as i64 * i64::from(SR) / 1_000_000));
            let poll = j.poll(now, playhead);
            if let Some(p) = poll.resync_playhead {
                anchor = Some((now, p));
                enqueued.clear();
            }
            enqueued.extend(poll.frames.iter().map(|f| f.pts_samples / 1024));
            now += 5_000;
        }
        let st = j.stats();
        assert!(enqueued.windows(2).all(|w| w[0] < w[1]), "pacchetti in ordine, senza doppioni");
        // con il 5% di perdite per copia, perdere entrambe le copie capita ~0,25% delle volte
        assert!(st.skipped * 100 <= frames as u64, "troppi buchi: {st:?}");
        assert!(st.resyncs <= 3, "troppe risincronizzazioni: {st:?}");
        assert!(st.duplicates > (frames as u64) / 2, "la ridondanza arriva e viene scartata: {st:?}");
        assert!(st.delay_us <= cfg.max_delay_us);
    }

    /// Mezzo secondo senza pacchetti. Se nel frattempo i pacchetti sono andati persi (la linea del tempo dell'host
    /// è andata avanti) si saltano i posti vuoti senza risincronizzare; se invece la cattura dell'host si era fermata
    /// (i nuovi pacchetti sono «nel passato» del riproduttore) si riparte una volta dai nuovi.
    #[test]
    fn stall_handling_depends_on_host_timeline() {
        let cfg = JitterConfig::aac(SR);
        let start_stream = |j: &mut AudioJitter| {
            for i in 0..5 {
                j.push(0, ts(i), vec![0]);
            }
            let start = j.poll(0, None).resync_playhead.expect("avvio");
            for step in 1..=10 {
                let p = start + step * 2_400;
                assert!(j.poll(step as u64 * 50_000, Some(p)).resync_playhead.is_none(), "niente di nuovo: si aspetta");
            }
            start + 24_000
        };

        // A) pacchetti persi nel blocco: la linea del tempo dell'host coincide con quella del riproduttore
        let mut j = AudioJitter::new(cfg);
        let playhead = start_stream(&mut j);
        for i in 25..30 {
            j.push(600_000, ts(i), vec![1]);
        }
        let poll = j.poll(600_000, Some(playhead));
        assert_eq!(poll.resync_playhead, None);
        assert_eq!(indices(&poll), vec![25, 26, 27, 28, 29]);
        assert_eq!(j.stats().resyncs, 1);

        // B) cattura dell'host ferma: i pacchetti nuovi sono dietro al riproduttore
        let mut j = AudioJitter::new(cfg);
        let playhead = start_stream(&mut j);
        for i in 5..10 {
            j.push(600_000, ts(i), vec![1]);
        }
        let poll = j.poll(600_000, Some(playhead));
        assert_eq!(poll.resync_playhead, Some(aligned(pts(10) - cfg.us_to_samples(cfg.initial_delay_us))));
        assert!(!poll.frames.is_empty());
        assert_eq!(j.stats().resyncs, 2);
    }

    /// Orologi di Android e iPhone diversi dello 0,3%: in 90 s sarebbero 270 ms di audio in più in coda (una
    /// risincronizzazione con salto udibile). Con la velocità consigliata il riproduttore li riassorbe piano.
    #[test]
    fn clock_drift_is_absorbed_by_playback_rate() {
        let cfg = JitterConfig::aac(SR);
        let period_us = 1024.0 * 1_000_000.0 / f64::from(SR);
        let host_faster = 1.003;
        let mut j = AudioJitter::new(cfg);
        let frames = (90.0 * 1_000_000.0 / period_us) as i64;
        let mut anchor: Option<(u64, f64)> = None; // (istante locale, posizione in campioni)
        let mut next_packet = 0i64;
        let mut now = 0u64;
        let mut max_rate: f64 = 1.0;
        let mut last_buffered = 0.0;
        while next_packet < frames {
            // i pacchetti arrivano secondo l'orologio dell'host, che corre più veloce
            while next_packet < frames && (next_packet as f64 * period_us / host_faster) as u64 <= now {
                j.push(now, ts(next_packet), vec![0]);
                next_packet += 1;
            }
            let playhead = anchor.map(|(_, p0)| p0 as i64);
            let poll = j.poll(now, playhead);
            if let Some(p) = poll.resync_playhead {
                anchor = Some((now, p as f64));
            }
            let rate = poll.playback_rate;
            max_rate = max_rate.max(rate);
            if let Some(h) = j.highest {
                if let Some((_, p)) = anchor {
                    last_buffered = (pts(h + 1) as f64 - p) / f64::from(SR) * 1_000.0;
                }
            }
            // il riproduttore avanza di 5 ms alla velocità consigliata
            if let Some((t0, p)) = anchor {
                anchor = Some((t0, p + 5_000.0 * f64::from(SR) / 1_000_000.0 * rate));
            }
            now += 5_000;
        }
        let st = j.stats();
        assert_eq!(st.resyncs, 1, "solo l'avvio: {st:?}");
        assert!(max_rate > 1.0015, "il riproduttore ha accelerato: {max_rate}");
        assert!(max_rate <= 1.0 + cfg.max_rate_offset + 1e-9);
        let target_ms = cfg.initial_delay_us as f64 / 1_000.0;
        assert!(
            (last_buffered - target_ms).abs() < 60.0,
            "coda finale {last_buffered:.0} ms, voluta {target_ms:.0} ms"
        );
        assert!(st.skipped <= 2, "{st:?}");
    }
}
