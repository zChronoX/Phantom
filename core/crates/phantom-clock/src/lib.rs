//! Sincronia degli orologi tra host (Android) e client (iPhone).
//!
//! Serve a due cose: misurare la latenza vera frame per frame (l'host marca ogni frame con l'ora di cattura sul
//! *suo* orologio; il client deve riportarla sul proprio) e datare gli eventi di input. Si usano quattro timestamp
//! come in NTP:
//!
//! ```text
//! client  t0 ─────────────────────────────► t3
//!               ╲                         ╱
//! host              t1 ──────────► t2
//! ```
//!
//! * `rtt    = (t3 − t0) − (t2 − t1)`
//! * `offset = ((t1 − t0) + (t2 − t3)) / 2`   (orologio host − orologio client)
//!
//! L'errore dell'offset è al massimo metà dell'asimmetria tra andata e ritorno; la rete locale è quasi simmetrica,
//! ma il jitter sporca i campioni: si tengono gli ultimi N e si usano solo quelli con il RTT più basso (i meno
//! disturbati dalle code), prendendone la mediana. Dalla loro pendenza nel tempo si stima anche la deriva.

use std::collections::VecDeque;

/// Un giro completo di richiesta e risposta, in microsecondi sui rispettivi orologi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Partenza della richiesta (orologio client).
    pub t0: u64,
    /// Arrivo sull'host (orologio host).
    pub t1: u64,
    /// Partenza della risposta (orologio host).
    pub t2: u64,
    /// Arrivo sul client (orologio client).
    pub t3: u64,
}

/// Un giro di ping più lungo di così non è una misura ma una risposta vecchia o inventata: si scarta (µs).
pub const MAX_SAMPLE_SPAN_US: u64 = 60_000_000;

fn saturate(v: i128) -> i64 {
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

impl Sample {
    /// Tempo di andata e ritorno in rete, senza il tempo di elaborazione dell'host (µs). Negativo = campione non valido.
    /// I conti sono a 128 bit: i valori dell'host arrivano dalla rete e possono essere qualsiasi cosa.
    pub fn rtt_us(&self) -> i64 {
        saturate((i128::from(self.t3) - i128::from(self.t0)) - (i128::from(self.t2) - i128::from(self.t1)))
    }
    /// Orologio host − orologio client (µs).
    pub fn offset_us(&self) -> i64 {
        saturate(((i128::from(self.t1) - i128::from(self.t0)) + (i128::from(self.t2) - i128::from(self.t3))) / 2)
    }
    /// Un campione ha senso solo se il tempo scorre in avanti su entrambi gli orologi, il RTT non è negativo e il giro
    /// non è durato più di [`MAX_SAMPLE_SPAN_US`].
    pub fn is_valid(&self) -> bool {
        self.t3 >= self.t0 && self.t3 - self.t0 <= MAX_SAMPLE_SPAN_US && self.t2 >= self.t1 && self.rtt_us() >= 0
    }
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    rtt: i64,
    offset: i64,
    at: u64, // t3, sul client
}

/// Stimatore dell'offset e della deriva tra i due orologi.
#[derive(Clone, Debug)]
pub struct ClockSync {
    window: VecDeque<Entry>,
    cap: usize,
}

impl Default for ClockSync {
    fn default() -> Self {
        Self::new()
    }
}

impl ClockSync {
    /// Stimatore con una finestra di 64 campioni.
    pub fn new() -> Self {
        Self::with_window(64)
    }

    /// Stimatore con una finestra di `n` campioni (almeno 4).
    pub fn with_window(n: usize) -> Self {
        ClockSync { window: VecDeque::new(), cap: n.max(4) }
    }

    /// Aggiunge un campione; quelli non validi vengono ignorati. Restituisce `true` se è stato accettato.
    pub fn add(&mut self, s: Sample) -> bool {
        if !s.is_valid() {
            return false;
        }
        if self.window.len() == self.cap {
            self.window.pop_front();
        }
        self.window.push_back(Entry { rtt: s.rtt_us(), offset: s.offset_us(), at: s.t3 });
        true
    }

    /// Campioni nella finestra.
    pub fn len(&self) -> usize {
        self.window.len()
    }

    /// `true` se non c'è ancora nessun campione.
    pub fn is_empty(&self) -> bool {
        self.window.is_empty()
    }

    /// RTT minimo nella finestra (µs): la miglior stima del tempo di andata e ritorno senza code.
    pub fn min_rtt_us(&self) -> Option<u64> {
        self.window.iter().map(|e| e.rtt).min().map(|r| r.max(0) as u64)
    }

    /// RTT mediano nella finestra (µs).
    pub fn median_rtt_us(&self) -> Option<u64> {
        let mut v: Vec<i64> = self.window.iter().map(|e| e.rtt).collect();
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        Some(v[v.len() / 2].max(0) as u64)
    }

    /// RTT mediano degli ultimi `n` campioni (µs): la situazione «di adesso», da confrontare con [`Self::min_rtt_us`].
    pub fn recent_rtt_us(&self, n: usize) -> Option<u64> {
        let n = n.clamp(1, self.window.len().max(1));
        let mut v: Vec<i64> = self.window.iter().rev().take(n).map(|e| e.rtt).collect();
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        Some(v[v.len() / 2].max(0) as u64)
    }

    /// RTT al percentile `q` (0.0–1.0) dei campioni nella finestra (µs).
    pub fn rtt_quantile_us(&self, q: f64) -> Option<u64> {
        let mut v: Vec<i64> = self.window.iter().map(|e| e.rtt).collect();
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        let i = ((v.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
        Some(v[i].max(0) as u64)
    }

    /// Ritardo di coda stimato (µs): RTT recente (mediana degli ultimi `n` campioni) meno il 25° percentile della
    /// finestra. Il percentile e non il minimo: il jitter normale del collegamento (Wi-Fi, 4G) fa oscillare ogni RTT di
    /// qualche ms e il minimo lo scambierebbe per coda; il 25° lo assorbe, mentre una coda vera lo supera di molto e
    /// non lo sposta finché occupa meno di un quarto del tempo. Serve almeno `min_samples` campioni, altrimenti `None`.
    pub fn queue_delay_us(&self, n: usize, min_samples: usize) -> Option<u64> {
        if self.window.len() < min_samples.max(1) {
            return None;
        }
        Some(self.recent_rtt_us(n)?.saturating_sub(self.rtt_quantile_us(0.25)?))
    }

    /// I campioni «buoni»: RTT entro il minimo + max(25% del minimo, 500 µs).
    fn good(&self) -> Vec<Entry> {
        let Some(min) = self.window.iter().map(|e| e.rtt).min() else { return Vec::new() };
        let tol = (min / 4).max(500);
        self.window.iter().copied().filter(|e| e.rtt <= min + tol).collect()
    }

    /// Pendenza dell'offset nel tempo (µs di offset per µs di tempo), se i campioni buoni coprono almeno 5 s.
    fn slope(&self) -> Option<(f64, f64, f64)> {
        let g = self.good();
        if g.len() < 6 {
            return None;
        }
        let t_min = g.iter().map(|e| e.at).min()?;
        let t_max = g.iter().map(|e| e.at).max()?;
        if t_max - t_min < 5_000_000 {
            return None;
        }
        let n = g.len() as f64;
        let mean_x = g.iter().map(|e| (e.at - t_min) as f64).sum::<f64>() / n;
        let mean_y = g.iter().map(|e| e.offset as f64).sum::<f64>() / n;
        let sxx: f64 = g.iter().map(|e| ((e.at - t_min) as f64 - mean_x).powi(2)).sum();
        let sxy: f64 = g.iter().map(|e| ((e.at - t_min) as f64 - mean_x) * (e.offset as f64 - mean_y)).sum();
        if sxx <= 0.0 {
            return None;
        }
        Some((sxy / sxx, mean_y - sxy / sxx * mean_x, t_min as f64))
    }

    /// Deriva stimata dell'orologio host rispetto a quello client, in parti per milione. `None` finché i campioni
    /// non coprono abbastanza tempo.
    pub fn drift_ppm(&self) -> Option<f64> {
        self.slope().map(|(s, _, _)| s * 1e6)
    }

    /// Offset (host − client) all'istante `at_client_us` dell'orologio client, in µs. `None` senza campioni.
    pub fn offset_at(&self, at_client_us: u64) -> Option<i64> {
        if let Some((slope, intercept, t0)) = self.slope() {
            return Some((intercept + slope * (at_client_us as f64 - t0)).round() as i64);
        }
        let mut offs: Vec<i64> = self.good().iter().map(|e| e.offset).collect();
        if offs.is_empty() {
            return None;
        }
        offs.sort_unstable();
        Some(offs[offs.len() / 2])
    }

    /// Converte un istante dell'orologio client in uno dell'orologio host.
    pub fn client_to_host(&self, t_client_us: u64) -> Option<u64> {
        let off = self.offset_at(t_client_us)?;
        Some((i128::from(t_client_us) + i128::from(off)).clamp(0, i128::from(u64::MAX)) as u64)
    }

    /// Converte un istante dell'orologio host in uno dell'orologio client (usa l'offset stimato a quell'istante).
    pub fn host_to_client(&self, t_host_us: u64) -> Option<u64> {
        // l'offset varia pianissimo: una iterazione di punto fisso basta
        let first =
            (i128::from(t_host_us) - i128::from(self.offset_at(t_host_us)?)).clamp(0, i128::from(u64::MAX)) as u64;
        let off = self.offset_at(first)?;
        Some((i128::from(t_host_us) - i128::from(off)).clamp(0, i128::from(u64::MAX)) as u64)
    }

    /// Da quanto tempo (µs) è passato `t_host_us`, un istante dell'orologio host, visto dal client all'istante
    /// `now_client_us`. Può essere negativo se l'host dichiara un istante nel futuro. Satura invece di andare in
    /// overflow.
    pub fn age_of_host_time(&self, now_client_us: u64, t_host_us: u64) -> Option<i64> {
        let t = self.host_to_client(t_host_us)?;
        Some(saturate(i128::from(now_client_us) - i128::from(t)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_netsim::{Dir, LinkConfig, NetSim, Rng};

    /// Orologio host: in ritardo/anticipo di `offset` e con una deriva in ppm rispetto al tempo reale. Gli orologi veri
    /// non sono mai negativi: per provare un offset negativo il client parte da `client_base_us`.
    struct Clocks {
        offset_us: i64,
        drift_ppm: f64,
        client_base_us: u64,
    }
    impl Clocks {
        fn client(&self, real_us: u64) -> u64 {
            real_us + self.client_base_us
        }
        fn host(&self, real_us: u64) -> u64 {
            let drifted = (real_us as f64 * (1.0 + self.drift_ppm * 1e-6)) as i128;
            u64::try_from(drifted + i128::from(self.client_base_us) + i128::from(self.offset_us))
                .expect("orologio host negativo")
        }
    }

    /// Scambia `n` ping a intervalli di `every_us` attraverso il simulatore e restituisce i campioni.
    fn measure(clocks: &Clocks, mut sim: NetSim, n: usize, every_us: u64, host_proc_us: u64) -> Vec<Sample> {
        let mut out = Vec::new();
        let mut pending: Vec<(u64, u64, u64)> = Vec::new(); // (arrivo reale sull'host, t0, id) in attesa di risposta
        let mut t = 0u64;
        let end = n as u64 * every_us + 2_000_000;
        let mut next_ping = 0u64;
        let mut id = 0u64;
        while t < end {
            if t >= next_ping && (id as usize) < n {
                let t0 = clocks.client(t);
                sim.send(Dir::AtoB, [id.to_le_bytes(), t0.to_le_bytes()].concat());
                id += 1;
                next_ping += every_us;
            }
            sim.advance_to(t);
            while let Some((arr, d)) = sim.recv(Dir::AtoB) {
                let t0 = u64::from_le_bytes(d[8..16].try_into().unwrap());
                pending.push((arr, t0, u64::from_le_bytes(d[0..8].try_into().unwrap())));
            }
            // l'host risponde dopo `host_proc_us` dall'arrivo
            let mut keep = Vec::new();
            for (arr, t0, pid) in pending.drain(..) {
                if arr + host_proc_us <= t {
                    let t1 = clocks.host(arr);
                    let t2 = clocks.host(arr + host_proc_us);
                    sim.send(
                        Dir::BtoA,
                        [pid.to_le_bytes(), t0.to_le_bytes(), t1.to_le_bytes(), t2.to_le_bytes()].concat(),
                    );
                } else {
                    keep.push((arr, t0, pid));
                }
            }
            pending = keep;
            while let Some((arr, d)) = sim.recv(Dir::BtoA) {
                let t0 = u64::from_le_bytes(d[8..16].try_into().unwrap());
                let t1 = u64::from_le_bytes(d[16..24].try_into().unwrap());
                let t2 = u64::from_le_bytes(d[24..32].try_into().unwrap());
                out.push(Sample { t0, t1, t2, t3: clocks.client(arr) });
            }
            t += 500;
        }
        out
    }

    #[test]
    fn exact_on_symmetric_ideal_link() {
        let c = Clocks { offset_us: 5_000_000, drift_ppm: 0.0, client_base_us: 0 };
        let sim = NetSim::symmetric(1, LinkConfig { base_delay_us: 4_000, ..LinkConfig::ideal() });
        let samples = measure(&c, sim, 20, 100_000, 100);
        let mut cs = ClockSync::new();
        for s in samples {
            assert!(cs.add(s));
        }
        let off = cs.offset_at(1_000_000).unwrap();
        assert!((off - 5_000_000).abs() <= 1_000, "offset {off}");
        assert!((cs.min_rtt_us().unwrap() as i64 - 8_000).abs() <= 1_000, "rtt {:?}", cs.min_rtt_us());
    }

    #[test]
    fn jitter_is_filtered_by_minimum_rtt() {
        let c = Clocks { offset_us: -123_456_789, drift_ppm: 0.0, client_base_us: 1_000_000_000 };
        let cfg = LinkConfig { base_delay_us: 3_000, jitter_us: 6_000, ..LinkConfig::ideal() };
        let sim = NetSim::symmetric(7, cfg);
        let samples = measure(&c, sim, 80, 50_000, 50);
        let mut cs = ClockSync::new();
        let raw_errors: Vec<i64> = samples.iter().map(|s| (s.offset_us() + 123_456_789).abs()).collect();
        for s in &samples {
            cs.add(*s);
        }
        let est = cs.offset_at(c.client(3_000_000)).unwrap();
        let err = (est + 123_456_789).abs();
        let worst_raw = *raw_errors.iter().max().unwrap();
        assert!(err < 1_500, "errore dopo il filtro: {err} µs");
        assert!(worst_raw > err, "il filtro deve fare meglio del campione peggiore ({worst_raw} vs {err})");
    }

    #[test]
    fn asymmetry_bounds_the_error_by_half() {
        // andata 2 ms, ritorno 10 ms: l'offset sbaglia di (2 − 10) / 2 = −4 ms; è il limite teorico di questo metodo
        let c = Clocks { offset_us: 1_000_000, drift_ppm: 0.0, client_base_us: 0 };
        let sim = NetSim::new(
            1,
            LinkConfig { base_delay_us: 2_000, ..LinkConfig::ideal() },
            LinkConfig { base_delay_us: 10_000, ..LinkConfig::ideal() },
        );
        let samples = measure(&c, sim, 20, 100_000, 0);
        let mut cs = ClockSync::new();
        for s in samples {
            cs.add(s);
        }
        let err = cs.offset_at(1_000_000).unwrap() - 1_000_000;
        assert!((err + 4_000).abs() <= 1_000, "errore {err} µs, atteso circa −4000");
    }

    #[test]
    fn drift_is_estimated_and_corrected() {
        let c = Clocks { offset_us: 777_000, drift_ppm: 50.0, client_base_us: 0 };
        let cfg = LinkConfig { base_delay_us: 2_500, jitter_us: 2_000, ..LinkConfig::ideal() };
        let sim = NetSim::symmetric(3, cfg);
        // un ping al secondo per 80 secondi: la finestra da 64 campioni copre 64 s
        let samples = measure(&c, sim, 80, 1_000_000, 50);
        let mut cs = ClockSync::new();
        for s in &samples {
            cs.add(*s);
        }
        let d = cs.drift_ppm().expect("con 64 s di campioni la deriva si stima");
        assert!((d - 50.0).abs() < 15.0, "deriva stimata {d} ppm, vera 50");
        // conversione a 75 s: errore sotto i 300 µs nonostante i 3,75 ms accumulati dalla deriva
        let real = 75_000_000u64;
        let host_true = c.host(real);
        let est = cs.client_to_host(c.client(real)).unwrap();
        assert!((est as i64 - host_true as i64).abs() < 300, "errore {} µs", est as i64 - host_true as i64);
        let back = cs.host_to_client(host_true).unwrap();
        assert!((back as i64 - real as i64).abs() < 300);
    }

    #[test]
    fn invalid_samples_are_rejected() {
        let mut cs = ClockSync::new();
        assert!(!cs.add(Sample { t0: 100, t1: 50, t2: 40, t3: 200 }), "l'host non torna indietro nel tempo");
        assert!(!cs.add(Sample { t0: 200, t1: 0, t2: 0, t3: 100 }), "il client non torna indietro nel tempo");
        assert!(!cs.add(Sample { t0: 0, t1: 0, t2: 500, t3: 100 }), "RTT negativo");
        assert!(cs.is_empty());
        assert_eq!(cs.offset_at(0), None);
        assert_eq!(cs.client_to_host(5), None);
        assert!(cs.add(Sample { t0: 0, t1: 10, t2: 10, t3: 20 }));
        assert_eq!(cs.len(), 1);
    }

    #[test]
    fn window_evicts_oldest() {
        let mut cs = ClockSync::with_window(8);
        for i in 0..20u64 {
            cs.add(Sample { t0: i * 1000, t1: i * 1000 + 500, t2: i * 1000 + 500, t3: i * 1000 + 1000 });
        }
        assert_eq!(cs.len(), 8);
    }

    #[test]
    fn rng_in_tests_is_the_shared_one() {
        let mut r = Rng::new(1);
        assert!(r.next_u64() != r.next_u64());
    }
    #[test]
    fn queue_delay_follows_recent_rtt() {
        let mut c = ClockSync::new();
        let mut t = 0u64;
        let mut add = |c: &mut ClockSync, rtt: u64| {
            t += 100_000;
            // andata e ritorno simmetrici, host istantaneo
            assert!(c.add(Sample { t0: t, t1: t + rtt / 2, t2: t + rtt / 2, t3: t + rtt }));
        };
        assert_eq!(c.queue_delay_us(3, 1), None, "senza campioni non si stima");
        for _ in 0..10 {
            add(&mut c, 8_000);
        }
        assert_eq!(c.min_rtt_us(), Some(8_000));
        assert_eq!(c.queue_delay_us(3, 8), Some(0), "rete scarica: nessuna coda");
        // un solo campione alto non basta (mediana degli ultimi 3), due sì
        add(&mut c, 40_000);
        assert_eq!(c.queue_delay_us(3, 8), Some(0));
        add(&mut c, 42_000);
        assert_eq!(c.queue_delay_us(3, 8), Some(32_000), "mediana di 8, 40 e 42 ms meno il 25° percentile di 8");
        for _ in 0..3 {
            add(&mut c, 9_000);
        }
        assert_eq!(c.queue_delay_us(3, 8), Some(1_000), "la coda si svuota");
        assert_eq!(c.queue_delay_us(3, 100), None, "con pochi campioni il riferimento non è affidabile");
    }

    #[test]
    fn normal_jitter_is_not_mistaken_for_queue() {
        // RTT che oscilla tra 12 e 36 ms (Wi-Fi disturbato): la coda stimata resta piccola, mentre un vero aumento si vede
        let mut c = ClockSync::new();
        let mut rng = Rng::new(3);
        let mut t = 0u64;
        let mut worst = 0u64;
        for _ in 0..200 {
            t += 100_000;
            let rtt = 12_000 + rng.up_to(12_000) + rng.up_to(12_000);
            c.add(Sample { t0: t, t1: t + rtt / 2, t2: t + rtt / 2, t3: t + rtt });
            if let Some(q) = c.queue_delay_us(3, 8) {
                worst = worst.max(q);
            }
        }
        assert!(worst < 12_000, "il jitter da solo non deve sembrare una coda: {worst}");
        for _ in 0..3 {
            t += 100_000;
            c.add(Sample { t0: t, t1: t + 30_000, t2: t + 30_000, t3: t + 60_000 + 24_000 });
        }
        assert!(c.queue_delay_us(3, 8).unwrap() > 40_000, "una coda vera, +60 ms, si vede");
    }
    #[test]
    fn hostile_samples_never_overflow_or_poison_the_window() {
        let mut c = ClockSync::new();
        let extremes = [0u64, 1, u64::MAX, 1 << 63, (1 << 63) - 1, 1 << 62];
        for &t0 in &extremes {
            for &t1 in &extremes {
                for &t2 in &extremes {
                    for &t3 in &extremes {
                        let s = Sample { t0, t1, t2, t3 };
                        let _ = s.rtt_us();
                        let _ = s.offset_us();
                        let _ = s.is_valid();
                        let _ = c.add(s);
                    }
                }
            }
        }
        // risposta vecchia di 10 minuti: scartata
        assert!(!Sample { t0: 0, t1: 5, t2: 6, t3: 600_000_000 }.is_valid());
        // risposta normale: accettata
        assert!(Sample { t0: 1_000, t1: 6_000, t2: 6_100, t3: 2_100 }.is_valid());
        let _ = c.offset_at(u64::MAX);
        let _ = c.client_to_host(u64::MAX);
        let _ = c.host_to_client(u64::MAX);
        let _ = c.age_of_host_time(u64::MAX, u64::MAX);
        let _ = c.age_of_host_time(0, u64::MAX);
    }

    #[test]
    fn age_of_host_time_matches_a_known_offset() {
        let mut c = ClockSync::new();
        // host avanti di 5 s, giro di 2 ms
        for i in 0..20u64 {
            let t0 = 1_000_000 + i * 100_000;
            assert!(c.add(Sample { t0, t1: t0 + 1_000 + 5_000_000, t2: t0 + 1_000 + 5_000_000, t3: t0 + 2_000 }));
        }
        // un frame catturato sull'host quando il client era a 2 s, visto dal client a 2,03 s: 30 ms
        assert_eq!(c.age_of_host_time(2_030_000, 7_000_000), Some(30_000));
        // un istante nel futuro dichiarato dall'host: età negativa
        assert_eq!(c.age_of_host_time(2_030_000, 7_100_000), Some(-70_000));
    }
}
