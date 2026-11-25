//! Modello del mittente QUIC per le prove: coda dei datagrammi, finestra di congestione e ritmo d'invio alla BBR.
//!
//! Il simulatore di rete da solo spedisce ogni pacchetto nell'istante in cui l'host lo produce: la coda sta tutta nel
//! collegamento. Nell'app invece i datagrammi aspettano nella coda di invio di QUIC finché il controllo di congestione
//! (BBR) non li lascia partire, ed è quella coda che lo scarto alla sorgente dell'host guarda. Senza modellarla le
//! prove non vedevano la trappola osservata su 5G nei test del 25/11: dopo un periodo con poco traffico (l'app in
//! background, solo audio) BBR stima poca banda, il primo keyframe resta in coda, la coda supera la soglia, l'host
//! salta frame e abbassa il bitrate, e con poco traffico la stima della banda non risale.
//!
//! Il modello è volutamente semplice, ma tiene le cose che contano qui:
//! - stima della banda = massimo dei campioni di velocità di consegna in una finestra di tempo ([`SenderConfig::bw_window_us`]);
//!   i campioni presi mentre il mittente non aveva nulla da spedire («app-limited») contano solo se alzano la stima,
//!   quindi con poco traffico la stima scade e scende al traffico reale;
//! - ritmo d'invio = guadagno × stima (avvio rapido all'inizio, poi un ciclo di sonda 1,25 / 0,9 / 1);
//! - finestra = 2 × stima × RTT minimo, mai sotto 4 pacchetti;
//! - perdita dichiarata quando la conferma non arriva entro un tempo legato al RTT; nessuna ritrasmissione
//!   (i datagrammi QUIC non si ritrasmettono).
//!
//! Le conferme le recapita chi usa il modello (con [`ModelSender::on_ack`]), di solito all'arrivo del pacchetto più il
//! ritardo del verso di ritorno.

use std::collections::{HashMap, VecDeque};

/// Parametri del mittente.
#[derive(Clone, Debug)]
pub struct SenderConfig {
    /// Capacità della coda dei datagrammi in uscita (byte): oltre, i più vecchi si buttano per far posto.
    pub queue_capacity: usize,
    /// Finestra iniziale (byte).
    pub initial_cwnd: usize,
    /// Finestra minima (byte).
    pub min_cwnd: usize,
    /// Durata della finestra del filtro della banda (µs).
    pub bw_window_us: u64,
    /// RTT ipotizzato prima della prima misura (µs).
    pub initial_rtt_us: u64,
}

impl SenderConfig {
    /// Come la connessione P2P dell'app (coda da 512 KiB).
    pub fn p2p() -> Self {
        SenderConfig {
            queue_capacity: 512 * 1024,
            initial_cwnd: 14_720,
            min_cwnd: 4 * 1200,
            bw_window_us: 5_000_000,
            initial_rtt_us: 100_000,
        }
    }

    /// Come la connessione LAN dell'app (coda da 2 MiB).
    pub fn lan() -> Self {
        SenderConfig { queue_capacity: 2 * 1024 * 1024, ..Self::p2p() }
    }
}

/// Fase del controllo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SenderPhase {
    /// Avvio: ritmo e finestra alti finché la stima della banda cresce.
    Startup,
    /// Regime: ciclo di sonda attorno alla stima.
    ProbeBw,
}

#[derive(Clone, Copy, Debug)]
struct Sent {
    size: usize,
    sent_at: u64,
    delivered_at_send: u64,
    delivered_time_at_send: u64,
    app_limited: bool,
}

/// Contatori del mittente.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SenderStats {
    /// Datagrammi messi sul collegamento.
    pub sent: u64,
    /// Confermati.
    pub acked: u64,
    /// Dichiarati persi.
    pub lost: u64,
    /// Datagrammi buttati dalla coda per far posto ai nuovi.
    pub displaced: u64,
}

/// Il mittente modellato.
#[derive(Debug)]
pub struct ModelSender {
    cfg: SenderConfig,
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    next_id: u64,
    in_flight: HashMap<u64, Sent>,
    inflight_bytes: usize,
    delivered: u64,
    delivered_time: u64,
    /// Campioni di velocità di consegna (istante, bit/s), dal più vecchio.
    bw_samples: VecDeque<(u64, u64)>,
    min_rtt_us: Option<u64>,
    srtt_us: Option<u64>,
    next_send_us: u64,
    phase: SenderPhase,
    full_bw: u64,
    full_bw_checks: u32,
    last_round_us: u64,
    cycle_start_us: u64,
    /// Fino a quando i pacchetti spediti contano come «app-limited» (in byte consegnati, come in BBR).
    app_limited_until: u64,
    /// Minimo temporaneo impostato dall'app (come `RampFloor` nell'app): ritmo in bit/s e scadenza.
    floor: Option<(u64, u64)>,
    stats: SenderStats,
}

impl ModelSender {
    /// Crea il mittente.
    pub fn new(cfg: SenderConfig) -> Self {
        ModelSender {
            cfg,
            queue: VecDeque::new(),
            queued_bytes: 0,
            next_id: 0,
            in_flight: HashMap::new(),
            inflight_bytes: 0,
            delivered: 0,
            delivered_time: 0,
            bw_samples: VecDeque::new(),
            min_rtt_us: None,
            srtt_us: None,
            next_send_us: 0,
            phase: SenderPhase::Startup,
            full_bw: 0,
            full_bw_checks: 0,
            last_round_us: 0,
            cycle_start_us: 0,
            app_limited_until: 0,
            floor: None,
            stats: SenderStats::default(),
        }
    }

    /// Accoda un datagramma. Restituisce `true` se per farlo entrare si sono dovuti buttare datagrammi più vecchi.
    pub fn enqueue(&mut self, data: Vec<u8>) -> bool {
        let mut displaced = false;
        while !self.queue.is_empty() && self.queued_bytes + data.len() > self.cfg.queue_capacity {
            if let Some(old) = self.queue.pop_front() {
                self.queued_bytes -= old.len();
                self.stats.displaced += 1;
                displaced = true;
            }
        }
        self.queued_bytes += data.len();
        self.queue.push_back(data);
        displaced
    }

    /// Byte in coda (non ancora spediti).
    pub fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }

    /// Capacità della coda (byte).
    pub fn capacity(&self) -> usize {
        self.cfg.queue_capacity
    }

    /// Come `RampFloor::set` nell'app: fino a `until_us` ritmo almeno `rate_bps` e finestra almeno 2 × ritmo × RTT.
    pub fn set_floor(&mut self, rate_bps: u64, until_us: u64) {
        self.floor = Some((rate_bps, until_us));
    }

    fn floor_at(&self, now_us: u64) -> Option<u64> {
        self.floor.filter(|&(_, until)| now_us < until).map(|(rate, _)| rate)
    }

    fn floor_window(&self, now_us: u64) -> usize {
        let rtt = self.min_rtt_us.unwrap_or(self.cfg.initial_rtt_us).max(30_000);
        self.floor_at(now_us).map_or(0, |rate| (rate / 8 * rtt * 2 / 1_000_000) as usize)
    }

    /// Finestra di congestione attuale (byte).
    pub fn cwnd(&self) -> usize {
        let rtt = self.min_rtt_us.unwrap_or(self.cfg.initial_rtt_us);
        match self.bw_estimate_bps() {
            None => self.cfg.initial_cwnd,
            Some(bw) => {
                let gain = if self.phase == SenderPhase::Startup { 2.89 } else { 2.0 };
                let bdp = (bw as f64 / 8.0 * rtt as f64 / 1e6 * gain) as usize;
                let floor = if self.phase == SenderPhase::Startup { self.cfg.initial_cwnd } else { self.cfg.min_cwnd };
                bdp.max(floor)
            }
        }
    }

    /// Stima della banda (bit/s), se c'è almeno un campione nella finestra.
    pub fn bw_estimate_bps(&self) -> Option<u64> {
        self.bw_samples.iter().map(|&(_, b)| b).max()
    }

    /// Fase del controllo.
    pub fn phase(&self) -> SenderPhase {
        self.phase
    }

    /// Contatori.
    pub fn stats(&self) -> SenderStats {
        self.stats
    }

    /// RTT minimo misurato (µs).
    pub fn min_rtt_us(&self) -> Option<u64> {
        self.min_rtt_us
    }

    fn pacing_gain(&self, now_us: u64) -> f64 {
        if self.phase == SenderPhase::Startup {
            return 2.89;
        }
        let rtt = self.min_rtt_us.unwrap_or(self.cfg.initial_rtt_us).max(1);
        let t = now_us.saturating_sub(self.cycle_start_us);
        if t < rtt {
            1.25
        } else if t < 2 * rtt {
            0.9
        } else {
            1.0
        }
    }

    fn pacing_rate_bps(&self, now_us: u64) -> u64 {
        let base = self.bw_estimate_bps().unwrap_or_else(|| {
            let rtt = self.cfg.initial_rtt_us.max(1);
            (self.cfg.initial_cwnd as u64 * 8 * 1_000_000) / rtt
        });
        ((base as f64 * self.pacing_gain(now_us)) as u64).max(100_000).max(self.floor_at(now_us).unwrap_or(0))
    }

    /// Fa uscire i datagrammi che la finestra e il ritmo consentono adesso: ognuno con il suo identificativo, da
    /// passare a [`ModelSender::on_ack`] quando la conferma arriva.
    pub fn poll(&mut self, now_us: u64) -> Vec<(u64, Vec<u8>)> {
        self.detect_losses(now_us);
        self.expire_samples(now_us);
        if now_us.saturating_sub(self.cycle_start_us) >= 2_500_000 {
            self.cycle_start_us = now_us;
        }
        let mut out = Vec::new();
        let cwnd = self.cwnd().max(self.floor_window(now_us));
        while let Some(front) = self.queue.front() {
            let size = front.len();
            if self.inflight_bytes + size > cwnd || now_us < self.next_send_us {
                break;
            }
            let Some(data) = self.queue.pop_front() else { break };
            self.queued_bytes -= size;
            let id = self.next_id;
            self.next_id += 1;
            let app_limited = self.app_limited_until > self.delivered;
            self.in_flight.insert(
                id,
                Sent {
                    size,
                    sent_at: now_us,
                    delivered_at_send: self.delivered,
                    delivered_time_at_send: if self.delivered_time == 0 { now_us } else { self.delivered_time },
                    app_limited,
                },
            );
            self.inflight_bytes += size;
            self.stats.sent += 1;
            let tx_us = (size as u64 * 8 * 1_000_000) / self.pacing_rate_bps(now_us).max(1);
            self.next_send_us = self.next_send_us.max(now_us.saturating_sub(tx_us)) + tx_us;
            out.push((id, data));
        }
        if self.queue.is_empty() && self.inflight_bytes < cwnd {
            // niente da spedire con la finestra libera: i campioni dei prossimi pacchetti non misurano la banda
            self.app_limited_until = (self.delivered + self.inflight_bytes as u64).max(1);
        }
        out
    }

    /// Conferma di un datagramma arrivata all'istante `now_us`.
    pub fn on_ack(&mut self, id: u64, now_us: u64) {
        let Some(p) = self.in_flight.remove(&id) else { return };
        self.inflight_bytes -= p.size;
        self.stats.acked += 1;
        self.delivered += p.size as u64;
        self.delivered_time = now_us;
        let rtt = now_us.saturating_sub(p.sent_at).max(1);
        self.min_rtt_us = Some(self.min_rtt_us.map_or(rtt, |m| m.min(rtt)));
        self.srtt_us = Some(self.srtt_us.map_or(rtt, |s| (s * 7 + rtt) / 8));
        let interval = now_us.saturating_sub(p.delivered_time_at_send).max(rtt / 2).max(1);
        let rate = (self.delivered - p.delivered_at_send) * 8 * 1_000_000 / interval;
        self.expire_samples(now_us);
        let current = self.bw_estimate_bps().unwrap_or(0);
        if !p.app_limited || rate >= current {
            self.bw_samples.push_back((now_us, rate));
        }
        self.update_phase(now_us);
    }

    fn update_phase(&mut self, now_us: u64) {
        if self.phase != SenderPhase::Startup {
            return;
        }
        let rtt = self.min_rtt_us.unwrap_or(self.cfg.initial_rtt_us);
        if now_us.saturating_sub(self.last_round_us) < rtt {
            return;
        }
        self.last_round_us = now_us;
        let bw = self.bw_estimate_bps().unwrap_or(0);
        if bw as f64 >= self.full_bw as f64 * 1.25 {
            self.full_bw = bw;
            self.full_bw_checks = 0;
        } else {
            self.full_bw_checks += 1;
            if self.full_bw_checks >= 3 {
                self.phase = SenderPhase::ProbeBw;
                self.cycle_start_us = now_us;
            }
        }
    }

    /// I campioni più vecchi della finestra escono dal filtro. Con poco traffico restano solo quelli app-limited (che
    /// entrano quando superano il massimo rimasto): la stima scende al traffico reale, come nei log del 25/11 (finestra
    /// di 5–40 KB dopo ogni ritorno dal background).
    fn expire_samples(&mut self, now_us: u64) {
        while self.bw_samples.front().is_some_and(|&(t, _)| now_us.saturating_sub(t) > self.cfg.bw_window_us) {
            self.bw_samples.pop_front();
        }
    }

    fn detect_losses(&mut self, now_us: u64) {
        let rtt = self.srtt_us.unwrap_or(self.cfg.initial_rtt_us);
        let timeout = rtt * 3 / 2 + 20_000;
        let lost: Vec<u64> = self
            .in_flight
            .iter()
            .filter(|(_, p)| now_us.saturating_sub(p.sent_at) > timeout)
            .map(|(&id, _)| id)
            .collect();
        for id in lost {
            if let Some(p) = self.in_flight.remove(&id) {
                self.inflight_bytes -= p.size;
                self.stats.lost += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collegamento ideale a banda fissa: il mittente porta la stima vicino alla banda e la finestra vicino a 2 × BDP.
    fn run_link(sender: &mut ModelSender, bw_bps: u64, rtt_us: u64, offered_bps: u64, from: u64, to: u64) {
        let mut acks: VecDeque<(u64, u64)> = VecDeque::new();
        let mut link_free = from;
        let mut t = from;
        let mut produced = 0u64;
        while t < to {
            // l'applicazione produce `offered_bps`
            while produced * 8 * 1_000_000 < offered_bps * (t - from) {
                sender.enqueue(vec![0u8; 1200]);
                produced += 1200;
            }
            for (id, d) in sender.poll(t) {
                let start = link_free.max(t);
                link_free = start + d.len() as u64 * 8 * 1_000_000 / bw_bps;
                acks.push_back((link_free + rtt_us, id));
            }
            while acks.front().is_some_and(|&(at, _)| at <= t) {
                if let Some((at, id)) = acks.pop_front() {
                    sender.on_ack(id, at);
                }
            }
            t += 250;
        }
    }

    #[test]
    fn saturated_sender_finds_the_bandwidth() {
        let mut s = ModelSender::new(SenderConfig::p2p());
        run_link(&mut s, 20_000_000, 60_000, 40_000_000, 0, 5_000_000);
        let bw = s.bw_estimate_bps().expect("stima");
        assert!((16_000_000..=26_000_000).contains(&bw), "stima {bw}");
        assert_eq!(s.phase(), SenderPhase::ProbeBw);
    }

    /// Con poco traffico (solo audio) la stima della banda scade e scende al traffico reale: è la trappola del ritorno
    /// dal background vista su 5G.
    #[test]
    fn estimate_decays_when_app_limited() {
        let mut s = ModelSender::new(SenderConfig::p2p());
        run_link(&mut s, 20_000_000, 60_000, 10_000_000, 0, 4_000_000);
        let before = s.bw_estimate_bps().expect("stima");
        assert!(before >= 8_000_000, "prima {before}");
        run_link(&mut s, 20_000_000, 60_000, 150_000, 4_000_000, 20_000_000);
        let after = s.bw_estimate_bps().expect("stima");
        assert!(after < 1_000_000, "dopo 16 s di solo audio la stima è {after}");
        assert!(s.cwnd() < 20_000, "finestra {}", s.cwnd());
    }

    #[test]
    fn queue_overflow_displaces_the_oldest() {
        let mut s = ModelSender::new(SenderConfig { queue_capacity: 3000, ..SenderConfig::p2p() });
        assert!(!s.enqueue(vec![1; 1200]));
        assert!(!s.enqueue(vec![2; 1200]));
        assert!(s.enqueue(vec![3; 1200]));
        assert_eq!(s.queued_bytes(), 2400);
        assert_eq!(s.stats().displaced, 1);
    }
}
