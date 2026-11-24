//! Scarto alla sorgente quando il collegamento non regge (lato host).
//!
//! Se il collegamento consegna meno di quanto l'encoder produce, i datagrammi si accumulano nella coda di invio del
//! trasporto: la latenza sale di secondi e, a coda piena, il trasporto butta via i pacchetti più vecchi. Prima l'host
//! chiedeva un keyframe a ogni pacchetto buttato: il keyframe è il frame più grosso, la coda si riempiva ancora di più
//! e si finiva in una tempesta di keyframe (centinaia in pochi minuti su un P2P lento).
//!
//! Qui invece, quando la coda supera una soglia di tempo (`limit`), i frame nuovi **non entrano nemmeno in coda**:
//! l'immagine si ferma un attimo, la coda si svuota, e solo allora si chiede **un** keyframe e si riparte da quello.
//! I frame non-key tra la fine dello svuotamento e il keyframe si saltano anche loro, perché farebbero riferimento a
//! frame mai spediti (immagine rovinata). Il client non vede buchi nella numerazione: per lui è solo una pausa.

/// Decisione per un frame appena uscito dall'encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameDecision {
    /// Spedirlo.
    Send,
    /// Non spedirlo (coda troppo piena, o si aspetta il keyframe dopo uno svuotamento).
    Skip,
}

/// Esito di [`SourceDropper::decide`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropOutcome {
    /// Cosa fare del frame.
    pub decision: FrameDecision,
    /// La coda si è appena svuotata: chiedere un keyframe all'encoder (una volta sola per svuotamento).
    pub request_keyframe: bool,
}

/// Stato dello scarto alla sorgente.
#[derive(Clone, Debug, Default)]
pub struct SourceDropper {
    draining: bool,
    waiting_key: bool,
    last_key_bytes: usize,
    skipped: u64,
    drains: u64,
}

impl SourceDropper {
    /// Crea lo stato iniziale (si spedisce tutto).
    pub fn new() -> Self {
        Self::default()
    }

    /// Frame saltati finora.
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Svuotamenti avviati finora.
    pub fn drains(&self) -> u64 {
        self.drains
    }

    /// Soglia della coda (byte) per un bitrate obiettivo: `window_ms` di video, mai meno di 48 KiB, mai più di
    /// `max_bytes`; almeno il doppio dell'ultimo keyframe, così un keyframe appena spedito non fa ripartire subito lo
    /// svuotamento.
    ///
    /// La finestra dipende dal collegamento. In LAN conviene larga (~600 ms): gli stalli del Wi-Fi durano qualche
    /// centinaio di ms e poi la radio smaltisce l'arretrato in pochi ms, quindi è molto meglio tenerlo in coda che
    /// buttarlo e ripartire da un keyframe. Su P2P/relay la banda è vicina al bitrate e l'arretrato si smaltirebbe
    /// lentamente: lì resta stretta (~250 ms).
    pub fn limit_bytes(&self, target_bps: u64, window_ms: u64, max_bytes: usize) -> usize {
        let window = (target_bps / 8).saturating_mul(window_ms) / 1000;
        (window as usize).max(48 * 1024).max(self.last_key_bytes.saturating_mul(2)).min(max_bytes)
    }

    /// Decide per il prossimo frame. `queued_bytes` è quanto c'è ora nella coda di invio, `limit_bytes` la soglia
    /// ([`SourceDropper::limit_bytes`]), `key` se il frame è un keyframe.
    pub fn decide(&mut self, queued_bytes: usize, limit_bytes: usize, key: bool) -> DropOutcome {
        let mut request_keyframe = false;
        if self.draining && queued_bytes <= limit_bytes / 4 {
            // svuotata: si riparte da un keyframe
            self.draining = false;
            self.waiting_key = true;
            request_keyframe = true;
        }
        if !self.draining && queued_bytes > limit_bytes {
            self.start_drain();
        }
        let decision = if self.draining {
            FrameDecision::Skip
        } else if self.waiting_key {
            if key {
                self.waiting_key = false;
                FrameDecision::Send
            } else {
                FrameDecision::Skip
            }
        } else {
            FrameDecision::Send
        };
        if decision == FrameDecision::Skip {
            self.skipped += 1;
        }
        DropOutcome { decision, request_keyframe }
    }

    /// Il trasporto ha dovuto buttare datagrammi già in coda (il frame precedente arriverà rotto): si svuota e si
    /// riparte da un keyframe, invece di chiederne uno subito.
    pub fn on_queue_overflow(&mut self) {
        if !self.draining {
            self.start_drain();
        }
    }

    /// Un keyframe è stato spedito: la sua dimensione serve a [`SourceDropper::limit_bytes`].
    pub fn on_keyframe_sent(&mut self, bytes: usize) {
        self.last_key_bytes = bytes;
    }

    fn start_drain(&mut self) {
        self.draining = true;
        self.waiting_key = false;
        self.drains += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 100_000;

    #[test]
    fn sends_everything_while_queue_is_short() {
        let mut d = SourceDropper::new();
        for _ in 0..100 {
            let o = d.decide(20_000, LIMIT, false);
            assert_eq!(o, DropOutcome { decision: FrameDecision::Send, request_keyframe: false });
        }
        assert_eq!(d.skipped(), 0);
    }

    /// Coda oltre la soglia: si salta finché non scende a un quarto, poi un solo keyframe e si riparte da quello.
    #[test]
    fn drains_then_restarts_from_a_single_keyframe() {
        let mut d = SourceDropper::new();
        assert_eq!(d.decide(150_000, LIMIT, false).decision, FrameDecision::Skip);
        assert_eq!(d.decide(90_000, LIMIT, false).decision, FrameDecision::Skip, "sotto soglia ma non ancora svuotata");
        let o = d.decide(20_000, LIMIT, false);
        assert!(o.request_keyframe, "svuotata: si chiede il keyframe");
        assert_eq!(o.decision, FrameDecision::Skip, "il frame non-key farebbe riferimento a frame mai spediti");
        let o = d.decide(10_000, LIMIT, false);
        assert_eq!(o, DropOutcome { decision: FrameDecision::Skip, request_keyframe: false }, "una richiesta sola");
        assert_eq!(d.decide(10_000, LIMIT, true).decision, FrameDecision::Send, "il keyframe riparte");
        assert_eq!(d.decide(15_000, LIMIT, false).decision, FrameDecision::Send);
        assert_eq!(d.skipped(), 4);
        assert_eq!(d.drains(), 1);
    }

    /// Un pacchetto buttato dal trasporto non chiede subito un keyframe: prima si svuota.
    #[test]
    fn overflow_starts_a_drain_instead_of_a_keyframe_storm() {
        let mut d = SourceDropper::new();
        d.on_queue_overflow();
        d.on_queue_overflow();
        assert_eq!(d.drains(), 1, "un solo svuotamento anche con più pacchetti buttati");
        assert_eq!(d.decide(60_000, LIMIT, true).decision, FrameDecision::Skip);
        let o = d.decide(0, LIMIT, false);
        assert!(o.request_keyframe);
        assert_eq!(d.decide(0, LIMIT, true).decision, FrameDecision::Send);
    }

    #[test]
    fn limit_scales_with_bitrate_and_last_keyframe() {
        let mut d = SourceDropper::new();
        let max = 400 * 1024;
        assert_eq!(d.limit_bytes(1_000_000, 250, max), 48 * 1024, "minimo a basso bitrate");
        assert_eq!(d.limit_bytes(8_000_000, 250, max), 250_000, "250 ms a 8 Mbit/s");
        assert_eq!(d.limit_bytes(40_000_000, 250, max), max, "tetto");
        assert_eq!(d.limit_bytes(14_000_000, 600, 2 * 1024 * 1024), 1_050_000, "600 ms a 14 Mbit/s in LAN");
        d.on_keyframe_sent(120_000);
        assert_eq!(d.limit_bytes(1_000_000, 250, max), 240_000, "almeno due keyframe");
    }
}
