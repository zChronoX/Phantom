//! Controllo del bitrate video (versione 0, prudente).
//!
//! Il client riferisce ogni 200 ms tre cose: quanti frame ha perso per colpa della rete, quanti byte ha ricevuto
//! (da cui l'host ricava la velocità con cui il collegamento consegna davvero) e quanta coda c'è sul collegamento (RTT
//! recente meno RTT di riferimento, misurato con i ping, che viaggiano nella stessa coda del video). Da sole le perdite
//! non bastano: un Wi-Fi disturbato perde pacchetti a raffiche anche quando è scarico, e abbassare il bitrate non lo
//! cura — si perderebbe qualità per nulla. Quindi:
//!
//! - **congestione** = perdite *con* coda alta, oppure coda altissima da sola (latenza che sale). Si scende subito a
//!   poco meno della velocità con cui il collegamento sta consegnando, e si ricorda un tetto lì vicino;
//! - **perdite pesanti** = perdite catastrofiche senza una coda alta a spiegarle (o senza modo di misurarla): si scende
//!   per prudenza di un passo fisso, senza tetto, perché potrebbe essere solo una raffica;
//! - **tutto bene** = coda misurata e bassa: si può salire, anche se c'è qualche perdita;
//! - altrimenti si resta dove si è.
//!
//! Si parte basso e si sale in fretta («startup») finché non c'è congestione, così il RTT di riferimento viene misurato
//! a collegamento scarico; poi si sale piano e si scende in fretta. Il tetto serve perché sondare di continuo oltre la
//! capacità costerebbe caro: ogni frame perso obbliga a un keyframe e a circa 300 ms di immagine ferma. Si rialza piano
//! ogni qualche secondo, per accorgersi se la capacità è aumentata, e si dimentica dopo un lungo periodo tranquillo.
//!
//! Il controllo completo (stima della banda, sonde, equità con altro traffico) è previsto nella fase 5.

use phantom_proto::{Stats, QUEUE_DELAY_UNKNOWN};

/// Perdite (frazione di frame) oltre le quali, con coda alta, si parla di congestione.
pub const LOSS_WITH_QUEUE: f64 = 0.04;
/// Perdite oltre le quali si scende comunque, anche senza una coda alta.
pub const LOSS_ALONE: f64 = 0.30;
/// Coda oltre la quale le perdite contano (µs).
pub const QUEUE_HIGH_US: u32 = 15_000;
/// Coda oltre la quale si scende anche senza perdite (µs): la latenza sta già salendo.
pub const QUEUE_ALONE_US: u32 = 60_000;
/// Coda sotto la quale si può salire (µs).
pub const QUEUE_LOW_US: u32 = 8_000;
/// Fattore applicato al bitrate a ogni riduzione per perdite pesanti, o per congestione se la velocità di consegna non è nota.
pub const DECREASE_FACTOR: f64 = 0.75;
/// Al massimo, in un colpo solo, si scende a questa frazione del bitrate (protegge da misure della velocità sporche).
pub const MAX_CUT_FACTOR: f64 = 0.5;
/// In congestione si scende a questa frazione della velocità con cui il collegamento sta consegnando. Il bitrate
/// dell'encoder è più basso del traffico reale (parità, intestazioni), e serve un margine perché la coda si svuoti.
pub const RX_RATE_FACTOR: f64 = 0.82;
/// Il tetto dopo una congestione è questa frazione della velocità di consegna (o, se non misurabile, del bitrate di allora).
pub const CEILING_RX_FACTOR: f64 = 0.82;
/// Come sopra, rispetto al bitrate quando la velocità di consegna non è nota.
pub const CEILING_FACTOR: f64 = 0.90;
/// Distanza minima tra due riduzioni (µs): la coda ha bisogno di tempo per svuotarsi e per farlo vedere nei ping.
pub const DECREASE_GAP_US: u64 = 300_000;
/// Dopo una riduzione, attesa prima di risalire (µs).
pub const INCREASE_AFTER_US: u64 = 1_000_000;
/// Crescita per finestra durante lo startup.
pub const STARTUP_GROWTH: f64 = 1.25;
/// Crescita per finestra a regime (più un piccolo passo fisso).
pub const STEADY_GROWTH: f64 = 1.04;
/// Passo fisso a regime (bit/s).
pub const STEADY_STEP_BPS: u64 = 50_000;
/// Frame minimi in una finestra perché valga la pena giudicare.
pub const MIN_FRAMES: u32 = 6;
/// Finestra minima per ricavare una velocità di consegna (µs).
pub const MIN_RATE_WINDOW_US: u64 = 100_000;
/// Ogni quanto il tetto si rialza per sondare una capacità maggiore (µs), la prima volta.
pub const CEILING_PROBE_EVERY_US: u64 = 4_000_000;
/// Se una sonda fallisce (nuova congestione con il tetto ancora in vigore) l'intervallo raddoppia, fino a questo massimo (µs).
pub const CEILING_PROBE_MAX_US: u64 = 32_000_000;
/// Di quanto si rialza.
pub const CEILING_PROBE_FACTOR: f64 = 1.10;
/// Il tetto si dimentica dopo questo tempo senza congestioni, ma mai prima di due intervalli di sonda (µs).
pub const CEILING_FORGET_US: u64 = 60_000_000;

/// Fase del controllo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Salita rapida iniziale.
    Startup,
    /// Salita lenta e discesa rapida.
    Steady,
}

/// Giudizio su una finestra di statistiche.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Il collegamento è congestionato, con la coda a dimostrarlo.
    Congested,
    /// Perdite catastrofiche senza una coda alta a spiegarle (o senza modo di misurarla): si scende per prudenza, ma
    /// non si ricorda un tetto, perché potrebbe essere solo una raffica del Wi-Fi.
    HeavyLoss,
    /// Va tutto bene: si può salire.
    Fine,
    /// Né l'uno né l'altro: si resta.
    Hold,
}

/// Giudica una finestra: `loss` è la frazione di frame persi, `queue_us` la coda misurata (`None` = non misurabile).
///
/// Una coda misurata e bassa vince su tutto: se la coda è vuota, le perdite non vengono da un collegamento intasato e
/// abbassare il bitrate non le cura (una raffica del Wi-Fi può far perdere anche metà di una finestra).
pub fn judge(loss: f64, queue_us: Option<u32>) -> Verdict {
    match queue_us {
        // senza misura della coda ci si fida solo di perdite catastrofiche
        None if loss >= LOSS_ALONE => Verdict::HeavyLoss,
        None => Verdict::Hold,
        Some(q) if q >= QUEUE_ALONE_US => Verdict::Congested,
        Some(q) if q >= QUEUE_HIGH_US && loss >= LOSS_WITH_QUEUE => Verdict::Congested,
        // coda a metà e perdite catastrofiche: un buffer corto con un forte eccesso di traffico
        Some(q) if q >= QUEUE_LOW_US && loss >= LOSS_ALONE => Verdict::HeavyLoss,
        Some(q) if q < QUEUE_LOW_US => Verdict::Fine,
        Some(_) => Verdict::Hold,
    }
}

/// Limiti del bitrate.
#[derive(Clone, Copy, Debug)]
pub struct RateConfig {
    /// Bitrate di partenza (bit/s).
    pub start_bps: u64,
    /// Minimo (bit/s).
    pub min_bps: u64,
    /// Massimo (bit/s).
    pub max_bps: u64,
}

/// Contatori del client all'ultima statistica.
#[derive(Clone, Copy, Debug)]
struct Prev {
    ok: u32,
    lost: u32,
    bytes: u32,
    at_us: u64,
}

/// Tetto ricordato dopo una congestione.
#[derive(Clone, Copy, Debug)]
struct Ceiling {
    bps: u64,
    /// Ultima volta che è stato fissato o rialzato (µs).
    last_move_us: u64,
    /// Ultima congestione (µs).
    last_congestion_us: u64,
    /// Ogni quanto si rialza (µs): raddoppia a ogni sonda fallita.
    probe_every_us: u64,
}

/// Il controllo.
#[derive(Clone, Debug)]
pub struct RateController {
    cfg: RateConfig,
    /// Massimo fissato alla creazione: [`RateController::set_max_bps`] non lo supera mai.
    hard_max_bps: u64,
    bitrate_bps: u64,
    phase: Phase,
    last_decrease_us: Option<u64>,
    prev: Option<Prev>,
    decreases: u32,
    last_verdict: Verdict,
    last_rx_rate_bps: Option<u64>,
    ceiling: Option<Ceiling>,
}

impl RateController {
    /// Crea il controllo, che parte da `cfg.start_bps` (portato nei limiti).
    pub fn new(cfg: RateConfig) -> Self {
        let max = cfg.max_bps.max(cfg.min_bps);
        let bitrate_bps = cfg.start_bps.clamp(cfg.min_bps, max);
        RateController {
            cfg: RateConfig { max_bps: max, ..cfg },
            hard_max_bps: max,
            bitrate_bps,
            phase: Phase::Startup,
            last_decrease_us: None,
            prev: None,
            decreases: 0,
            last_verdict: Verdict::Hold,
            last_rx_rate_bps: None,
            ceiling: None,
        }
    }

    /// Bitrate che l'encoder dovrebbe usare adesso (bit/s).
    pub fn bitrate_bps(&self) -> u64 {
        self.bitrate_bps
    }

    /// Fase corrente.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Numero di riduzioni fatte finora.
    pub fn decreases(&self) -> u32 {
        self.decreases
    }

    /// Ultimo giudizio.
    pub fn last_verdict(&self) -> Verdict {
        self.last_verdict
    }

    /// Tetto attuale (bit/s), se c'è.
    pub fn ceiling_bps(&self) -> Option<u64> {
        self.ceiling.map(|c| c.bps)
    }

    /// Ultima velocità di consegna misurata (bit/s), se la finestra era abbastanza lunga.
    pub fn last_rx_rate_bps(&self) -> Option<u64> {
        self.last_rx_rate_bps
    }

    /// Massimo attuale (bit/s).
    pub fn max_bps(&self) -> u64 {
        self.cfg.max_bps
    }

    /// Cambia il massimo durante la sessione (bit/s), entro `[min, massimo iniziale]`. Serve all'host per non far
    /// salire l'obiettivo molto oltre il bitrate che l'encoder può usare con profitto: un obiettivo di 32 Mbit/s con un
    /// encoder fermo a 15 rende inutili le riduzioni (si taglia l'obiettivo, ma l'encoder non cambia).
    pub fn set_max_bps(&mut self, max_bps: u64) {
        let max = max_bps.clamp(self.cfg.min_bps, self.hard_max_bps);
        self.cfg.max_bps = max;
        self.bitrate_bps = self.bitrate_bps.min(max);
        if let Some(c) = &mut self.ceiling {
            c.bps = c.bps.min(max);
        }
    }

    /// La coda di invio dell'host ha superato la soglia e lo scarto alla sorgente ha dovuto svuotarla (frame saltati e
    /// un keyframe per ripartire). È congestione vista dal lato host, prima ancora che il client misuri perdite o coda:
    /// si scende di un passo fisso e si fissa un tetto. Se succede di nuovo a breve, la sonda del tetto si allontana
    /// (come per ogni congestione), così uno stallo periodico del Wi-Fi porta il bitrate dove lo stallo entra nella
    /// coda invece di provocare un keyframe ogni pochi secondi. La velocità di consegna non si usa: misurata durante
    /// uno stallo è vicina a zero e taglierebbe troppo.
    pub fn on_send_stall(&mut self, now_us: u64) -> Verdict {
        let rx = self.last_rx_rate_bps.take();
        let v = self.apply(now_us, Verdict::Congested);
        self.last_rx_rate_bps = rx;
        v
    }

    /// Elabora una statistica del client (contatori cumulativi). `now_us` è l'orologio dell'host.
    pub fn on_stats(&mut self, now_us: u64, s: &Stats) -> Verdict {
        let cur = Prev { ok: s.frames_ok, lost: s.frames_lost, bytes: s.rx_bytes, at_us: now_us };
        let Some(prev) = self.prev.replace(cur) else { return Verdict::Hold };
        let ok = cur.ok.wrapping_sub(prev.ok);
        let lost = cur.lost.wrapping_sub(prev.lost);
        let dt = now_us.saturating_sub(prev.at_us);
        self.last_rx_rate_bps =
            (dt >= MIN_RATE_WINDOW_US).then(|| u64::from(cur.bytes.wrapping_sub(prev.bytes)) * 8 * 1_000_000 / dt);
        let queue = (s.queue_delay_us != QUEUE_DELAY_UNKNOWN).then_some(s.queue_delay_us);
        let total = ok.saturating_add(lost);
        let v = if total < MIN_FRAMES {
            // finestra troppo povera di frame, ma una coda altissima conta lo stesso
            if queue.is_some_and(|q| q >= QUEUE_ALONE_US) {
                Verdict::Congested
            } else {
                Verdict::Hold
            }
        } else {
            judge(f64::from(lost) / f64::from(total), queue)
        };
        self.apply(now_us, v)
    }

    fn apply(&mut self, now_us: u64, v: Verdict) -> Verdict {
        self.last_verdict = v;
        let since_decrease = self.last_decrease_us.map(|t| now_us.saturating_sub(t));
        match v {
            Verdict::Congested | Verdict::HeavyLoss => {
                self.phase = Phase::Steady;
                if since_decrease.is_none_or(|d| d >= DECREASE_GAP_US) {
                    // con la coda a dimostrare che il collegamento è saturo, la velocità di consegna è la sua capacità:
                    // si scende a poco meno di quella (ma al massimo della metà in un colpo, se la misura fosse
                    // sporca); senza, o per perdite pesanti, di un passo fisso
                    let rx = if v == Verdict::Congested { self.last_rx_rate_bps } else { None };
                    let target = match rx {
                        Some(r) => {
                            ((r as f64 * RX_RATE_FACTOR) as u64).max((self.bitrate_bps as f64 * MAX_CUT_FACTOR) as u64)
                        }
                        None => (self.bitrate_bps as f64 * DECREASE_FACTOR) as u64,
                    };
                    let next = target.min(self.bitrate_bps).max(self.cfg.min_bps);
                    if next < self.bitrate_bps {
                        self.decreases += 1;
                    }
                    if v == Verdict::Congested {
                        let bps = rx.map_or((self.bitrate_bps as f64 * CEILING_FACTOR) as u64, |r| {
                            (r as f64 * CEILING_RX_FACTOR) as u64
                        });
                        let bps = bps.max(next).min(self.cfg.max_bps);
                        // un tetto ancora in vigore e una nuova congestione: la sonda è fallita, la prossima più in là
                        let probe_every_us = self
                            .ceiling
                            .map_or(CEILING_PROBE_EVERY_US, |c| (c.probe_every_us * 2).min(CEILING_PROBE_MAX_US));
                        self.ceiling =
                            Some(Ceiling { bps, last_move_us: now_us, last_congestion_us: now_us, probe_every_us });
                    }
                    self.bitrate_bps = next;
                    self.last_decrease_us = Some(now_us);
                }
            }
            Verdict::Fine => {
                if since_decrease.is_none_or(|d| d >= INCREASE_AFTER_US) {
                    let next = match self.phase {
                        Phase::Startup => (self.bitrate_bps as f64 * STARTUP_GROWTH) as u64,
                        Phase::Steady => (self.bitrate_bps as f64 * STEADY_GROWTH) as u64 + STEADY_STEP_BPS,
                    };
                    let mut limit = self.cfg.max_bps;
                    if let Some(c) = &mut self.ceiling {
                        if now_us.saturating_sub(c.last_congestion_us) >= CEILING_FORGET_US.max(2 * c.probe_every_us) {
                            self.ceiling = None;
                        } else {
                            if now_us.saturating_sub(c.last_move_us) >= c.probe_every_us && self.bitrate_bps >= c.bps {
                                c.bps = (c.bps as f64 * CEILING_PROBE_FACTOR) as u64;
                                c.last_move_us = now_us;
                            }
                            limit = limit.min(c.bps);
                        }
                    }
                    self.bitrate_bps = next.min(limit);
                    if self.bitrate_bps >= self.cfg.max_bps {
                        self.phase = Phase::Steady;
                    }
                }
            }
            Verdict::Hold => {}
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Una finestra di statistiche da 200 ms: frame ok/persi, coda, velocità con cui il collegamento ha consegnato.
    struct Feeder {
        t: u64,
        ok: u32,
        lost: u32,
        bytes: u32,
    }
    impl Feeder {
        fn new() -> Self {
            Feeder { t: 0, ok: 0, lost: 0, bytes: 0 }
        }
        fn feed(&mut self, c: &mut RateController, d_ok: u32, d_lost: u32, queue_us: u32, rx_bps: u64) -> Verdict {
            self.t += 200_000;
            self.ok += d_ok;
            self.lost += d_lost;
            self.bytes = self.bytes.wrapping_add((rx_bps / 8 / 5) as u32);
            let s = Stats {
                rx_bytes: self.bytes,
                frames_ok: self.ok,
                frames_lost: self.lost,
                frames_fec: 0,
                queue_delay_us: queue_us,
                rtt_us: 20_000,
            };
            c.on_stats(self.t, &s)
        }
    }

    #[test]
    fn judge_ignores_loss_without_queue() {
        // Wi-Fi disturbato ma scarico: nemmeno perdite catastrofiche con coda bassa sono congestione
        for loss in [0.0, 0.05, 0.15, 0.25, 0.6, 0.95] {
            assert_eq!(judge(loss, Some(2_000)), Verdict::Fine, "perdita {loss}: con coda bassa si può anche salire");
        }
    }

    #[test]
    fn judge_flags_queue_with_loss_or_huge_queue() {
        assert_eq!(judge(0.05, Some(20_000)), Verdict::Congested);
        assert_eq!(judge(0.0, Some(20_000)), Verdict::Hold, "coda media senza perdite: si resta");
        assert_eq!(judge(0.0, Some(70_000)), Verdict::Congested, "coda altissima anche senza perdite");
        assert_eq!(judge(0.5, None), Verdict::HeavyLoss, "perdite catastrofiche senza misura della coda");
        assert_eq!(judge(0.5, Some(10_000)), Verdict::HeavyLoss, "perdite catastrofiche con coda a metà: buffer corto");
        assert_eq!(judge(0.5, Some(20_000)), Verdict::Congested, "con la coda alta è congestione vera");
        assert_eq!(judge(0.05, None), Verdict::Hold, "senza misura della coda e con perdite moderate non si sa");
        assert_eq!(judge(0.1, Some(10_000)), Verdict::Hold, "coda a metà e perdite moderate: si resta");
    }

    #[test]
    fn startup_climbs_then_congestion_drops_to_the_delivery_rate() {
        let mut c = RateController::new(RateConfig { start_bps: 4_000_000, min_bps: 1_000_000, max_bps: 30_000_000 });
        let mut f = Feeder::new();
        f.feed(&mut c, 24, 0, 1_000, 4_000_000); // la prima è solo di riferimento
        assert_eq!(c.phase(), Phase::Startup);
        for _ in 0..5 {
            let now_rate = c.bitrate_bps();
            f.feed(&mut c, 24, 0, 1_000, now_rate);
        }
        assert!(c.bitrate_bps() > 9_000_000, "in startup sale in fretta: {}", c.bitrate_bps());
        // il collegamento regge 8 Mbps: coda e perdite → scende in un colpo solo a poco meno di quella velocità
        let before = c.bitrate_bps();
        f.feed(&mut c, 12, 12, 30_000, 8_000_000);
        assert_eq!(c.last_verdict(), Verdict::Congested);
        assert_eq!(c.phase(), Phase::Steady);
        assert_eq!(c.decreases(), 1);
        let expected = (8_000_000.0 * RX_RATE_FACTOR) as u64;
        assert!(
            c.bitrate_bps() <= expected + 1 && c.bitrate_bps() > expected - 1_000,
            "{} → {}",
            before,
            c.bitrate_bps()
        );
        let tetto = c.ceiling_bps().expect("una congestione con la coda a dimostrarlo fissa un tetto");
        assert!(tetto >= c.bitrate_bps() && tetto < 8_000_000, "tetto {tetto}");
        // subito dopo (200 ms) non scende ancora
        let low = c.bitrate_bps();
        f.feed(&mut c, 12, 12, 30_000, 8_000_000);
        assert_eq!(c.decreases(), 1, "distanza minima tra due riduzioni");
        assert_eq!(c.bitrate_bps(), low);
        // coda svuotata: per 1 s dalla riduzione non sale, poi sì ma piano
        for _ in 0..3 {
            f.feed(&mut c, 24, 0, 500, low);
        }
        assert_eq!(c.bitrate_bps(), low, "per 1 s dopo la riduzione non sale");
        for _ in 0..5 {
            let now_rate = c.bitrate_bps();
            f.feed(&mut c, 24, 0, 500, now_rate);
        }
        assert!(
            c.bitrate_bps() >= low && c.bitrate_bps() <= tetto,
            "a regime non supera il tetto: {} → {} (tetto {})",
            low,
            c.bitrate_bps(),
            tetto
        );
    }

    #[test]
    fn a_second_congested_window_does_not_cut_below_the_delivery_rate() {
        // la coda si vede con ritardo: la finestra dopo la riduzione dice ancora «congestione», ma il collegamento
        // consegna ancora a 10 Mbps e il bitrate è già sotto: non si scende oltre
        let mut c = RateController::new(RateConfig { start_bps: 20_000_000, min_bps: 1_000_000, max_bps: 30_000_000 });
        let mut f = Feeder::new();
        f.feed(&mut c, 24, 0, 1_000, 20_000_000);
        f.feed(&mut c, 12, 12, 40_000, 10_000_000);
        let after_first = c.bitrate_bps();
        for _ in 0..6 {
            f.feed(&mut c, 12, 12, 40_000, 10_000_000);
        }
        assert!(
            c.bitrate_bps() >= (10_000_000.0 * RX_RATE_FACTOR) as u64 - 1,
            "non oltre la velocità di consegna: {} (prima {})",
            c.bitrate_bps(),
            after_first
        );
        assert!(c.decreases() <= 2);
    }

    #[test]
    fn bursty_loss_on_an_idle_link_does_not_lower_the_rate() {
        let mut c = RateController::new(RateConfig { start_bps: 20_000_000, min_bps: 2_000_000, max_bps: 20_000_000 });
        let mut f = Feeder::new();
        for i in 0..100 {
            // una finestra su due ha una raffica che costa 5 frame su 24, la coda resta bassa
            let d_lost = if i % 2 == 0 { 5 } else { 0 };
            f.feed(&mut c, 24 - d_lost, d_lost, 1_500, 18_000_000);
        }
        assert_eq!(c.bitrate_bps(), 20_000_000);
        assert_eq!(c.decreases(), 0);
    }

    #[test]
    fn unknown_queue_never_lets_the_rate_climb() {
        let mut c = RateController::new(RateConfig { start_bps: 4_000_000, min_bps: 1_000_000, max_bps: 20_000_000 });
        let mut f = Feeder::new();
        for _ in 0..20 {
            f.feed(&mut c, 24, 0, QUEUE_DELAY_UNKNOWN, 4_000_000);
        }
        assert_eq!(c.bitrate_bps(), 4_000_000, "senza misura della coda si resta fermi");
    }

    #[test]
    fn rate_stays_within_limits() {
        let mut c = RateController::new(RateConfig { start_bps: 3_000_000, min_bps: 2_000_000, max_bps: 5_000_000 });
        let mut f = Feeder::new();
        for _ in 0..50 {
            let now_rate = c.bitrate_bps();
            f.feed(&mut c, 24, 0, 500, now_rate);
        }
        assert_eq!(c.bitrate_bps(), 5_000_000);
        for _ in 0..50 {
            f.feed(&mut c, 5, 19, 80_000, 500_000);
        }
        assert_eq!(c.bitrate_bps(), 2_000_000);
    }

    #[test]
    fn heavy_loss_without_queue_evidence_lowers_the_rate_but_sets_no_ceiling() {
        let mut c = RateController::new(RateConfig { start_bps: 10_000_000, min_bps: 1_000_000, max_bps: 20_000_000 });
        let mut f = Feeder::new();
        f.feed(&mut c, 24, 0, QUEUE_DELAY_UNKNOWN, 10_000_000);
        f.feed(&mut c, 10, 14, QUEUE_DELAY_UNKNOWN, 3_000_000);
        assert_eq!(c.last_verdict(), Verdict::HeavyLoss);
        assert_eq!(
            c.bitrate_bps(),
            7_500_000,
            "un passo fisso, senza fidarsi della velocità di consegna (potrebbe essere una raffica)"
        );
        assert_eq!(c.ceiling_bps(), None, "e una raffica non diventa un tetto");
    }

    #[test]
    fn byte_counter_wraparound_is_harmless() {
        let mut c = RateController::new(RateConfig { start_bps: 10_000_000, min_bps: 1_000_000, max_bps: 20_000_000 });
        let mut f = Feeder::new();
        f.bytes = u32::MAX - 100_000;
        f.feed(&mut c, 24, 0, 1_000, 10_000_000);
        f.feed(&mut c, 24, 0, 1_000, 10_000_000); // qui il contatore dei byte avvolge
        let rate = c.last_rx_rate_bps().expect("misurata");
        assert!((9_500_000..=10_500_000).contains(&rate), "{rate}");
    }

    /// Modello a fluido: il collegamento regge fino a `capacity_bps` (parità compresa, +10%); il giudizio arriva con
    /// `lag` finestre di ritardo (la coda si accorge dell'eccesso dopo un po'). Restituisce (congestioni, bitrate medio
    /// nell'ultima metà).
    fn simulate(use_ceiling: bool, capacity_bps: u64, windows: usize, lag: usize) -> (u32, f64) {
        let mut c = RateController::new(RateConfig { start_bps: 4_000_000, min_bps: 1_000_000, max_bps: 30_000_000 });
        let mut f = Feeder::new();
        let mut history: Vec<u64> = vec![c.bitrate_bps(); lag + 1];
        let mut events = 0;
        let mut tail = Vec::new();
        for w in 0..windows {
            let seen = history[history.len() - 1 - lag];
            let offered = (seen as f64 * 1.1) as u64;
            let over = offered > capacity_bps;
            let (d_ok, d_lost, q, rx) = if over { (14, 10, 40_000, capacity_bps) } else { (24, 0, 1_000, offered) };
            let before = c.decreases();
            f.feed(&mut c, d_ok, d_lost, q, rx);
            if !use_ceiling {
                c.ceiling = None;
            }
            if c.decreases() > before {
                events += 1;
            }
            history.push(c.bitrate_bps());
            if w >= windows / 2 {
                tail.push(c.bitrate_bps() as f64);
            }
        }
        (events, tail.iter().sum::<f64>() / tail.len() as f64)
    }

    #[test]
    fn ceiling_cuts_the_number_of_overshoots_and_keeps_the_rate_near_capacity() {
        let capacity = 10_000_000;
        // 5 minuti di finestre da 200 ms, giudizio con 3 finestre di ritardo
        let (with, avg_with) = simulate(true, capacity, 1_500, 3);
        let (without, avg_without) = simulate(false, capacity, 1_500, 3);
        let usable = capacity as f64 / 1.1; // il bitrate video che entra nel collegamento con la parità
        eprintln!(
            "FLUID con tetto: {with} congestioni, {:.2} Mbps medi · senza: {without}, {:.2} Mbps (utile {:.2})",
            avg_with / 1e6,
            avg_without / 1e6,
            usable / 1e6
        );
        assert!(with * 2 < without, "con il tetto le congestioni calano a meno della metà: {with} contro {without}");
        assert!(with <= 40, "meno di una ogni 7 s in media: {with}");
        assert!(
            avg_with > 0.8 * usable && avg_with <= usable * 1.02,
            "bitrate medio vicino alla capacità utile: {:.2} Mbps su {:.2}",
            avg_with / 1e6,
            usable / 1e6
        );
        assert!(
            avg_without > 0.7 * usable,
            "controllo: anche senza tetto resta nei paraggi ({:.2} Mbps)",
            avg_without / 1e6
        );
    }

    #[test]
    fn ceiling_rises_when_capacity_grows() {
        // la capacità passa da 6 a 18 Mbps dopo un minuto: il tetto si rialza (a gradini, poi si dimentica) e il bitrate segue
        let mut c = RateController::new(RateConfig { start_bps: 4_000_000, min_bps: 1_000_000, max_bps: 30_000_000 });
        let mut f = Feeder::new();
        let mut history: Vec<u64> = vec![c.bitrate_bps(); 4];
        let (mut at_40s, mut before_change) = (0, Vec::new());
        let mut last_100s = Vec::new();
        for w in 0..1_500 {
            let capacity: u64 = if w < 300 { 6_000_000 } else { 18_000_000 };
            let offered = (history[history.len() - 4] as f64 * 1.1) as u64;
            let over = offered > capacity;
            let (d_ok, d_lost, q, rx) = if over { (14, 10, 40_000, capacity) } else { (24, 0, 1_000, offered) };
            f.feed(&mut c, d_ok, d_lost, q, rx);
            history.push(c.bitrate_bps());
            if (150..300).contains(&w) {
                before_change.push(c.bitrate_bps() as f64);
            }
            if w == 300 + 200 {
                at_40s = c.bitrate_bps();
            }
            if w >= 1_000 {
                last_100s.push(c.bitrate_bps() as f64);
            }
        }
        let avg_before = before_change.iter().sum::<f64>() / before_change.len() as f64;
        let avg_after = last_100s.iter().sum::<f64>() / last_100s.len() as f64;
        assert!(
            avg_before > 4_300_000.0 && avg_before < 5_600_000.0,
            "prima del cambio sta vicino ai 6/1,1 Mbps: {:.2}",
            avg_before / 1e6
        );
        assert!(at_40s as f64 > avg_before * 1.1, "dopo 40 s dal cambio è già salito: {:.1} Mbps", at_40s as f64 / 1e6);
        assert!(
            avg_after > 0.75 * 18_000_000.0 / 1.1,
            "a regime segue la nuova capacità: {:.1} Mbps di media",
            avg_after / 1e6
        );
    }

    #[test]
    fn send_stall_cuts_and_sets_a_ceiling() {
        let mut c = RateController::new(RateConfig { start_bps: 14_000_000, min_bps: 3_000_000, max_bps: 32_000_000 });
        assert_eq!(c.on_send_stall(1_000_000), Verdict::Congested);
        assert_eq!(c.bitrate_bps(), 10_500_000, "passo fisso, non la velocità di consegna");
        assert_eq!(c.ceiling_bps(), Some(12_600_000));
        assert_eq!(c.phase(), Phase::Steady);
        // un secondo stallo subito dopo non taglia di nuovo (la coda ha bisogno di tempo per svuotarsi)
        c.on_send_stall(1_100_000);
        assert_eq!(c.bitrate_bps(), 10_500_000);
        // uno stallo più tardi taglia ancora e allontana la sonda
        c.on_send_stall(1_500_000);
        assert_eq!(c.bitrate_bps(), 7_875_000);
        assert_eq!(c.decreases(), 2);
    }

    #[test]
    fn send_stall_ignores_a_delivery_rate_measured_during_the_stall() {
        let mut c = RateController::new(RateConfig { start_bps: 12_000_000, min_bps: 3_000_000, max_bps: 32_000_000 });
        let mut f = Feeder::new();
        f.feed(&mut c, 12, 0, 2_000, 12_000_000);
        f.feed(&mut c, 12, 0, 2_000, 200_000); // finestra di stallo: quasi niente consegnato
        let before = c.bitrate_bps();
        c.on_send_stall(f.t + 1);
        assert_eq!(c.bitrate_bps(), (before as f64 * DECREASE_FACTOR) as u64);
        assert_eq!(c.last_rx_rate_bps(), Some(200_000), "la misura resta per le statistiche");
    }

    #[test]
    fn max_can_be_lowered_and_raised_within_the_initial_limit() {
        let mut c = RateController::new(RateConfig { start_bps: 30_000_000, min_bps: 3_000_000, max_bps: 32_000_000 });
        c.set_max_bps(15_000_000);
        assert_eq!(c.max_bps(), 15_000_000);
        assert_eq!(c.bitrate_bps(), 15_000_000);
        let mut f = Feeder::new();
        for _ in 0..50 {
            f.feed(&mut c, 12, 0, 1_000, 40_000_000);
        }
        assert_eq!(c.bitrate_bps(), 15_000_000, "non supera il nuovo massimo");
        c.set_max_bps(100_000_000);
        assert_eq!(c.max_bps(), 32_000_000, "mai oltre il massimo iniziale");
        c.set_max_bps(1);
        assert_eq!(c.max_bps(), 3_000_000, "mai sotto il minimo");
    }
}
