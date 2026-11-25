//! Simulatore di rete deterministico a tempo virtuale.
//!
//! Serve a provare protocollo, trasporto e correzione degli errori senza rete vera: ogni prova è ripetibile (stesso
//! seme = stessi pacchetti persi, stessi ritardi), non dorme mai (il tempo è un numero che si fa avanzare) ed è
//! quindi veloce e identica su ogni macchina e su ogni piattaforma della CI.
//!
//! Modella un collegamento bidirezionale con, per ogni verso: ritardo base, jitter, perdita casuale, perdita a
//! raffica (modello di Gilbert-Elliott), riordino, duplicazione, banda limitata con coda finita (perdita per coda
//! piena, come un router che si riempie).

pub mod transport;

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

/// Generatore pseudocasuale (splitmix64): piccolo, veloce, identico ovunque. Non è crittografico.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// Crea il generatore da un seme.
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }
    /// Prossimo valore a 64 bit.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Valore uniforme in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Vero con probabilità `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        p > 0.0 && self.next_f64() < p
    }
    /// Intero uniforme in [0, n] (n incluso).
    pub fn up_to(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % (n + 1)
        }
    }
}

/// Perdite a raffica (Gilbert-Elliott a tempo continuo): due stati, «buono» e «cattivo», con perdita diversa in ciascuno.
///
/// Le durate sono in tempo reale e non in numero di pacchetti: un'interferenza sul Wi-Fi dura quanto dura, qualunque sia la
/// quantità di dati che si sta mandando. Con un modello a pacchetti, abbassare il bitrate allungherebbe le raffiche e
/// falserebbe ogni prova sul controllo della velocità.
#[derive(Clone, Debug)]
pub struct Burst {
    /// Durata media del periodo «buono» tra due raffiche (µs).
    pub mean_good_us: u64,
    /// Durata media di una raffica (µs).
    pub mean_bad_us: u64,
    /// Probabilità di perdere un pacchetto durante una raffica (fuori si usa solo `loss`).
    pub loss_in_bad: f64,
}

impl Burst {
    /// Frazione di tempo passata nello stato cattivo, a regime.
    pub fn bad_fraction(&self) -> f64 {
        let (g, b) = (self.mean_good_us.max(1) as f64, self.mean_bad_us.max(1) as f64);
        b / (g + b)
    }
}

/// Parametri di un verso del collegamento.
#[derive(Clone, Debug)]
pub struct LinkConfig {
    /// Ritardo di propagazione fisso (µs).
    pub base_delay_us: u64,
    /// Ritardo casuale aggiunto, uniforme in [0, jitter_us] (µs).
    pub jitter_us: u64,
    /// `true` (predefinito): il jitter non fa sorpassare i pacchetti, come su una coda reale (un pacchetto in ritardo
    /// ritarda anche quelli dietro di lui). `false`: ogni pacchetto ha il suo ritardo indipendente e quindi arrivano
    /// scombinati — una prova di stress, non un collegamento realistico. Il riordino «vero» si regola con `reorder`.
    pub fifo: bool,
    /// Probabilità di perdita casuale indipendente per pacchetto.
    pub loss: f64,
    /// Perdite a raffica, se presenti.
    pub burst: Option<Burst>,
    /// Probabilità che un pacchetto sia trattenuto di più (e arrivi dopo quelli successivi).
    pub reorder: f64,
    /// Ritardo aggiuntivo di un pacchetto riordinato (µs).
    pub reorder_extra_us: u64,
    /// Probabilità che un pacchetto arrivi due volte.
    pub duplicate: f64,
    /// Banda del collo di bottiglia in bit al secondo (`None` = illimitata).
    pub bandwidth_bps: Option<u64>,
    /// Capacità della coda del collo di bottiglia in byte (oltre, i pacchetti si perdono).
    pub queue_bytes: usize,
}

impl LinkConfig {
    /// Collegamento ideale: nessun ritardo, nessuna perdita.
    pub fn ideal() -> Self {
        LinkConfig {
            base_delay_us: 0,
            jitter_us: 0,
            fifo: true,
            loss: 0.0,
            burst: None,
            reorder: 0.0,
            reorder_extra_us: 0,
            duplicate: 0.0,
            bandwidth_bps: None,
            queue_bytes: usize::MAX,
        }
    }

    /// Wi-Fi di casa in buone condizioni: circa 3 ms di ritardo, 3 ms di jitter, poca perdita.
    pub fn good_wifi() -> Self {
        LinkConfig {
            base_delay_us: 3_000,
            jitter_us: 3_000,
            loss: 0.001,
            bandwidth_bps: Some(300_000_000),
            queue_bytes: 256 * 1024,
            ..Self::ideal()
        }
    }

    /// Wi-Fi disturbato: ritardo e jitter alti, 3% di perdita a raffica, un po' di riordino.
    pub fn bad_wifi() -> Self {
        LinkConfig {
            base_delay_us: 6_000,
            jitter_us: 12_000,
            burst: Some(Burst { mean_good_us: 200_000, mean_bad_us: 10_000, loss_in_bad: 0.5 }),
            loss: 0.005,
            reorder: 0.02,
            reorder_extra_us: 8_000,
            duplicate: 0.002,
            bandwidth_bps: Some(100_000_000),
            queue_bytes: 128 * 1024,
            ..Self::ideal()
        }
    }
}

/// Verso di un pacchetto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Dal lato A al lato B.
    AtoB = 0,
    /// Dal lato B al lato A.
    BtoA = 1,
}

impl Dir {
    /// Il verso opposto.
    pub fn rev(self) -> Dir {
        match self {
            Dir::AtoB => Dir::BtoA,
            Dir::BtoA => Dir::AtoB,
        }
    }
}

/// Contatori di un verso.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// Pacchetti inviati.
    pub sent: u64,
    /// Pacchetti consegnati (duplicati compresi).
    pub delivered: u64,
    /// Persi per perdita casuale o a raffica.
    pub lost: u64,
    /// Persi per coda piena.
    pub queue_drops: u64,
    /// Duplicati.
    pub duplicated: u64,
    /// Trattenuti di più (riordinati).
    pub reordered: u64,
}

#[derive(Debug)]
struct Link {
    cfg: LinkConfig,
    free_at_us: u64,
    /// Ultimo arrivo programmato nel verso (per non far sorpassare i pacchetti quando `fifo`).
    last_arrival_us: u64,
    in_bad_state: bool,
    /// Istante dell'ultimo aggiornamento dello stato a raffica (`None` = mai: si parte dalla distribuzione a regime).
    last_state_us: Option<u64>,
    stats: LinkStats,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct InFlight {
    arrival_us: u64,
    order: u64,
    data: Vec<u8>,
}

/// Il simulatore.
#[derive(Debug)]
pub struct NetSim {
    now_us: u64,
    rng: Rng,
    order: u64,
    links: [Link; 2],
    flying: [BinaryHeap<Reverse<InFlight>>; 2],
    inbox: [VecDeque<(u64, Vec<u8>)>; 2],
}

/// Byte di intestazione IP + UDP contati nella banda.
const IP_UDP_OVERHEAD: usize = 28;

impl NetSim {
    /// Crea il simulatore con un seme e la configurazione dei due versi.
    pub fn new(seed: u64, a_to_b: LinkConfig, b_to_a: LinkConfig) -> Self {
        let mk = |cfg| Link {
            cfg,
            free_at_us: 0,
            last_arrival_us: 0,
            in_bad_state: false,
            last_state_us: None,
            stats: LinkStats::default(),
        };
        NetSim {
            now_us: 0,
            rng: Rng::new(seed),
            order: 0,
            links: [mk(a_to_b), mk(b_to_a)],
            flying: [BinaryHeap::new(), BinaryHeap::new()],
            inbox: [VecDeque::new(), VecDeque::new()],
        }
    }

    /// Stessa configurazione nei due versi.
    pub fn symmetric(seed: u64, cfg: LinkConfig) -> Self {
        Self::new(seed, cfg.clone(), cfg)
    }

    /// Istante corrente (µs).
    pub fn now_us(&self) -> u64 {
        self.now_us
    }

    /// Cambia la configurazione di un verso mentre la prova è in corso (un'interruzione, un cambio di cella, un
    /// utente che si allontana dal router). I pacchetti già in volo non cambiano; la coda del collo di bottiglia e lo
    /// stato delle raffiche restano quelli che erano.
    pub fn set_config(&mut self, dir: Dir, cfg: LinkConfig) {
        self.links[dir as usize].cfg = cfg;
    }

    /// Contatori di un verso.
    pub fn stats(&self, dir: Dir) -> LinkStats {
        self.links[dir as usize].stats
    }

    /// Invia un pacchetto nel verso indicato, all'istante corrente.
    pub fn send(&mut self, dir: Dir, data: Vec<u8>) {
        let i = dir as usize;
        let now = self.now_us;
        let size = data.len() + IP_UDP_OVERHEAD;
        let link = &mut self.links[i];
        link.stats.sent += 1;

        // coda e banda del collo di bottiglia
        let depart_us = if let Some(bps) = link.cfg.bandwidth_bps {
            let backlog_us = link.free_at_us.saturating_sub(now);
            let backlog_bytes = (u128::from(backlog_us) * u128::from(bps) / 8 / 1_000_000) as usize;
            if backlog_bytes.saturating_add(size) > link.cfg.queue_bytes {
                link.stats.queue_drops += 1;
                return;
            }
            let start = now.max(link.free_at_us);
            let tx_us = (size as u128 * 8 * 1_000_000 / u128::from(bps)) as u64;
            link.free_at_us = start + tx_us;
            link.free_at_us
        } else {
            now
        };

        // perdita: casuale e a raffica (il pacchetto ha comunque occupato la banda)
        let mut p_loss = link.cfg.loss;
        if let Some(b) = link.cfg.burst.clone() {
            // catena a due stati a tempo continuo, campionata agli istanti dei pacchetti: la probabilità di trovarsi nello
            // stato cattivo dopo `dt` ha una forma chiusa, quindi il risultato non dipende da quanti pacchetti passano
            let pi_bad = b.bad_fraction();
            let p_bad = match link.last_state_us {
                None => pi_bad,
                Some(last) => {
                    let dt = now.saturating_sub(last) as f64;
                    let rate = 1.0 / b.mean_good_us.max(1) as f64 + 1.0 / b.mean_bad_us.max(1) as f64;
                    let decay = (-rate * dt).exp();
                    if link.in_bad_state {
                        pi_bad + (1.0 - pi_bad) * decay
                    } else {
                        pi_bad * (1.0 - decay)
                    }
                }
            };
            link.last_state_us = Some(now);
            link.in_bad_state = self.rng.chance(p_bad);
            if link.in_bad_state {
                p_loss = p_loss.max(b.loss_in_bad);
            }
        }
        if self.rng.chance(p_loss) {
            link.stats.lost += 1;
            return;
        }

        let copies = if self.rng.chance(link.cfg.duplicate) {
            link.stats.duplicated += 1;
            2
        } else {
            1
        };
        for _ in 0..copies {
            let mut arrival = depart_us + link.cfg.base_delay_us + self.rng.up_to(link.cfg.jitter_us);
            if self.rng.chance(link.cfg.reorder) {
                // trattenuto di più: arriva dopo quelli che lo seguono e non li costringe ad aspettarlo
                arrival += link.cfg.reorder_extra_us;
                link.stats.reordered += 1;
            } else {
                if link.cfg.fifo {
                    arrival = arrival.max(link.last_arrival_us);
                }
                link.last_arrival_us = arrival;
            }
            self.order += 1;
            self.flying[i].push(Reverse(InFlight { arrival_us: arrival, order: self.order, data: data.clone() }));
        }
    }

    /// Fa avanzare il tempo a `t_us` (mai indietro) e consegna ciò che è arrivato.
    pub fn advance_to(&mut self, t_us: u64) {
        if t_us > self.now_us {
            self.now_us = t_us;
        }
        for i in 0..2 {
            while let Some(Reverse(top)) = self.flying[i].peek() {
                if top.arrival_us > self.now_us {
                    break;
                }
                if let Some(Reverse(p)) = self.flying[i].pop() {
                    self.links[i].stats.delivered += 1;
                    self.inbox[i].push_back((p.arrival_us, p.data));
                }
            }
        }
    }

    /// Prossimo pacchetto arrivato nel verso indicato, con il suo istante di arrivo (µs).
    pub fn recv(&mut self, dir: Dir) -> Option<(u64, Vec<u8>)> {
        self.inbox[dir as usize].pop_front()
    }

    /// Istante del prossimo arrivo ancora in volo (per saltare il tempo morto nei test).
    pub fn next_arrival_us(&self) -> Option<u64> {
        let a = self.flying[0].peek().map(|Reverse(p)| p.arrival_us);
        let b = self.flying[1].peek().map(|Reverse(p)| p.arrival_us);
        match (a, b) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (x, y) => x.or(y),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cfg: LinkConfig, seed: u64, n: usize, gap_us: u64) -> (NetSim, Vec<(u64, Vec<u8>)>) {
        let mut sim = NetSim::new(seed, cfg, LinkConfig::ideal());
        let mut got = Vec::new();
        for i in 0..n {
            sim.advance_to(i as u64 * gap_us);
            sim.send(Dir::AtoB, (i as u32).to_le_bytes().to_vec());
            while let Some(p) = sim.recv(Dir::AtoB) {
                got.push(p);
            }
        }
        sim.advance_to(n as u64 * gap_us + 10_000_000);
        while let Some(p) = sim.recv(Dir::AtoB) {
            got.push(p);
        }
        (sim, got)
    }

    #[test]
    fn rng_is_deterministic_and_uniform() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(1);
        assert!((0..100).all(|_| a.next_u64() == b.next_u64()));
        let mut r = Rng::new(7);
        let mean: f64 = (0..100_000).map(|_| r.next_f64()).sum::<f64>() / 100_000.0;
        assert!((mean - 0.5).abs() < 0.01, "media {mean}");
        let mut r = Rng::new(9);
        let hits = (0..100_000).filter(|_| r.chance(0.1)).count();
        assert!((9_500..10_500).contains(&hits), "{hits}");
    }

    #[test]
    fn same_seed_same_result() {
        let cfg = LinkConfig::bad_wifi();
        let (_, a) = run(cfg.clone(), 42, 2000, 1000);
        let (_, b) = run(cfg.clone(), 42, 2000, 1000);
        let (_, c) = run(cfg, 43, 2000, 1000);
        assert_eq!(a, b, "stesso seme, stessi arrivi");
        assert_ne!(a, c, "seme diverso, arrivi diversi");
    }

    #[test]
    fn delay_and_jitter_stay_in_bounds() {
        let cfg = LinkConfig { base_delay_us: 5_000, jitter_us: 2_000, ..LinkConfig::ideal() };
        let (_, got) = run(cfg, 1, 1000, 1_000);
        assert_eq!(got.len(), 1000);
        for (i, (arr, data)) in got.iter().enumerate() {
            let idx = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as u64;
            let sent = idx * 1_000;
            assert!(*arr >= sent + 5_000 && *arr <= sent + 7_000, "pacchetto {i}: ritardo {}", arr - sent);
        }
    }

    #[test]
    fn random_loss_matches_configuration() {
        let cfg = LinkConfig { loss: 0.05, ..LinkConfig::ideal() };
        let (sim, got) = run(cfg, 5, 100_000, 10);
        let rate = 1.0 - got.len() as f64 / 100_000.0;
        assert!((rate - 0.05).abs() < 0.005, "perdita {rate}");
        let s = sim.stats(Dir::AtoB);
        assert_eq!(s.sent, 100_000);
        assert_eq!(s.lost + s.delivered, 100_000);
    }

    /// Lunghezza di ogni serie di pacchetti consecutivi persi, dato l'insieme dei pacchetti arrivati.
    fn loss_runs(got: &[(u64, Vec<u8>)], n: usize) -> Vec<usize> {
        let mut seen = vec![false; n];
        for (_, d) in got {
            seen[u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as usize] = true;
        }
        let (mut runs, mut cur) = (Vec::new(), 0);
        for s in &seen {
            if *s {
                if cur > 0 {
                    runs.push(cur);
                }
                cur = 0;
            } else {
                cur += 1;
            }
        }
        if cur > 0 {
            runs.push(cur);
        }
        runs
    }

    fn longest_run(got: &[(u64, Vec<u8>)], n: usize) -> usize {
        loss_runs(got, n).into_iter().max().unwrap_or(0)
    }

    fn mean_run(got: &[(u64, Vec<u8>)], n: usize) -> f64 {
        let runs = loss_runs(got, n);
        runs.iter().sum::<usize>() as f64 / runs.len().max(1) as f64
    }

    #[test]
    fn burst_loss_comes_in_runs() {
        // un pacchetto ogni 10 µs; raffiche medie di 500 µs (circa 50 pacchetti), una ogni 5 ms
        let cfg = LinkConfig {
            burst: Some(Burst { mean_good_us: 5_000, mean_bad_us: 500, loss_in_bad: 0.9 }),
            ..LinkConfig::ideal()
        };
        let (_, got) = run(cfg, 11, 50_000, 10);
        assert!(
            longest_run(&got, 50_000) >= 8,
            "con perdite a raffica ci devono essere serie lunghe di pacchetti persi"
        );
    }

    #[test]
    fn burst_loss_fraction_matches_configuration() {
        let burst = Burst { mean_good_us: 40_000, mean_bad_us: 10_000, loss_in_bad: 0.8 };
        let expected = burst.bad_fraction() * 0.8; // 20% del tempo × 80%
        let cfg = LinkConfig { burst: Some(burst), ..LinkConfig::ideal() };
        let (_, got) = run(cfg, 21, 200_000, 100);
        let rate = 1.0 - got.len() as f64 / 200_000.0;
        assert!((rate - expected).abs() < 0.03, "perdita {rate:.3}, attesa {expected:.3}");
    }

    #[test]
    fn burst_is_clocked_by_time_not_by_packets() {
        // stessa raffica media (2 ms) vista con 10 µs e con 1 ms tra un pacchetto e l'altro: a tempo, le serie di perdite
        // sono in media di circa 200 pacchetti nel primo caso e di 2-3 nel secondo; con un modello a pacchetti sarebbero uguali
        let burst = Burst { mean_good_us: 20_000, mean_bad_us: 2_000, loss_in_bad: 1.0 };
        let fast = LinkConfig { burst: Some(burst.clone()), ..LinkConfig::ideal() };
        let slow = LinkConfig { burst: Some(burst), ..LinkConfig::ideal() };
        let (_, got_fast) = run(fast, 5, 40_000, 10);
        let (_, got_slow) = run(slow, 5, 40_000, 1_000);
        let (lf, ls) = (mean_run(&got_fast, 40_000), mean_run(&got_slow, 40_000));
        assert!(lf >= 100.0, "a 10 µs una raffica da 2 ms sono 200 pacchetti: {lf:.1}");
        assert!(ls <= 6.0, "a 1 ms una raffica da 2 ms sono 2 pacchetti: {ls:.1}");
        // la perdita complessiva, invece, è la stessa
        let (rf, rs) = (1.0 - got_fast.len() as f64 / 40_000.0, 1.0 - got_slow.len() as f64 / 40_000.0);
        assert!((rf - rs).abs() < 0.03 && (rf - 0.0909).abs() < 0.03, "perdita veloce {rf:.3}, lenta {rs:.3}");
    }

    #[test]
    fn reordering_and_duplication_happen() {
        let cfg = LinkConfig {
            base_delay_us: 1_000,
            reorder: 0.2,
            reorder_extra_us: 5_000,
            duplicate: 0.1,
            ..LinkConfig::ideal()
        };
        let (sim, got) = run(cfg, 3, 5_000, 1_000);
        let idx: Vec<u32> = got.iter().map(|(_, d)| u32::from_le_bytes([d[0], d[1], d[2], d[3]])).collect();
        assert!(idx.windows(2).any(|w| w[1] < w[0]), "almeno un pacchetto arriva fuori ordine");
        assert!(got.len() > 5_000, "con duplicati arrivano più pacchetti di quelli inviati");
        assert!(sim.stats(Dir::AtoB).duplicated > 0 && sim.stats(Dir::AtoB).reordered > 0);
    }

    #[test]
    fn bandwidth_limits_throughput_and_drops_when_queue_is_full() {
        // 8 Mbps = 1 MB/s; pacchetti da 972 byte + 28 di intestazione = 1000 byte → 1000 pacchetti al secondo al massimo
        let cfg = LinkConfig { bandwidth_bps: Some(8_000_000), queue_bytes: 20_000, ..LinkConfig::ideal() };
        let mut sim = NetSim::new(1, cfg, LinkConfig::ideal());
        // si offrono 2000 pacchetti/s per 1 s
        for i in 0..2000u64 {
            sim.advance_to(i * 500);
            sim.send(Dir::AtoB, vec![0u8; 972]);
        }
        sim.advance_to(5_000_000);
        let mut n = 0;
        let mut last = 0;
        while let Some((t, _)) = sim.recv(Dir::AtoB) {
            n += 1;
            last = t;
        }
        let s = sim.stats(Dir::AtoB);
        assert!(s.queue_drops > 800, "il collo di bottiglia deve scartare quasi la metà: {}", s.queue_drops);
        assert!((1000..1200).contains(&n), "consegnati {n}");
        assert!(last > 900_000, "la consegna dura circa un secondo: {last}");
    }

    #[test]
    fn serialization_delay_adds_latency() {
        // 1 Mbps: 1250 byte (1222 + 28) = 10 ms di trasmissione
        let cfg = LinkConfig { bandwidth_bps: Some(1_000_000), base_delay_us: 2_000, ..LinkConfig::ideal() };
        let mut sim = NetSim::new(1, cfg, LinkConfig::ideal());
        sim.send(Dir::AtoB, vec![0; 1222]);
        sim.advance_to(1_000_000);
        let (t, _) = sim.recv(Dir::AtoB).unwrap();
        assert_eq!(t, 12_000);
    }

    #[test]
    fn directions_are_independent() {
        let mut sim = NetSim::new(
            1,
            LinkConfig { base_delay_us: 1_000, ..LinkConfig::ideal() },
            LinkConfig { base_delay_us: 9_000, ..LinkConfig::ideal() },
        );
        sim.send(Dir::AtoB, vec![1]);
        sim.send(Dir::BtoA, vec![2]);
        sim.advance_to(2_000);
        assert_eq!(sim.recv(Dir::AtoB).map(|p| p.1), Some(vec![1]));
        assert_eq!(sim.recv(Dir::BtoA), None);
        sim.advance_to(10_000);
        assert_eq!(sim.recv(Dir::BtoA).map(|p| p.1), Some(vec![2]));
    }

    #[test]
    fn time_never_goes_backwards() {
        let mut sim = NetSim::symmetric(1, LinkConfig::ideal());
        sim.advance_to(100);
        sim.advance_to(50);
        assert_eq!(sim.now_us(), 100);
    }
    /// Quante volte un pacchetto arriva dopo uno inviato più tardi.
    fn inversions(got: &[(u64, Vec<u8>)]) -> usize {
        let idx: Vec<u32> = got.iter().map(|(_, d)| u32::from_le_bytes([d[0], d[1], d[2], d[3]])).collect();
        idx.windows(2).filter(|w| w[1] < w[0]).count()
    }

    #[test]
    fn fifo_jitter_never_reorders_but_independent_jitter_does() {
        let base = LinkConfig { base_delay_us: 2_000, jitter_us: 5_000, ..LinkConfig::ideal() };
        let (_, fifo) = run(base.clone(), 8, 5_000, 100);
        assert_eq!(fifo.len(), 5_000);
        assert_eq!(inversions(&fifo), 0, "con fifo il jitter ritarda ma non fa sorpassare");
        let (_, free) = run(LinkConfig { fifo: false, ..base }, 8, 5_000, 100);
        assert!(inversions(&free) > 500, "senza fifo il jitter indipendente scombina l'ordine: {}", inversions(&free));
    }

    #[test]
    fn explicit_reorder_works_even_with_fifo() {
        let cfg = LinkConfig { base_delay_us: 1_000, reorder: 0.05, reorder_extra_us: 5_000, ..LinkConfig::ideal() };
        let (_, got) = run(cfg, 4, 5_000, 1_000);
        assert!(inversions(&got) > 100, "il riordino esplicito resta: {}", inversions(&got));
    }
    #[test]
    fn config_can_change_mid_run() {
        let mut sim = NetSim::symmetric(1, LinkConfig::ideal());
        sim.send(Dir::AtoB, vec![1]);
        sim.advance_to(10);
        assert!(sim.recv(Dir::AtoB).is_some());
        sim.set_config(Dir::AtoB, LinkConfig { loss: 1.0, ..LinkConfig::ideal() });
        sim.send(Dir::AtoB, vec![2]);
        sim.send(Dir::BtoA, vec![3]);
        sim.advance_to(20);
        assert!(sim.recv(Dir::AtoB).is_none(), "interruzione in un verso");
        assert!(sim.recv(Dir::BtoA).is_some(), "l'altro verso non è toccato");
        sim.set_config(Dir::AtoB, LinkConfig::ideal());
        sim.send(Dir::AtoB, vec![4]);
        sim.advance_to(30);
        assert_eq!(sim.recv(Dir::AtoB).map(|(_, d)| d), Some(vec![4]), "ripristino");
    }
}
