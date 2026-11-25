//! Controllo di congestione di Phantom per il P2P: BBR v3 di `noq` con un minimo temporaneo ([`RampFloor`]).

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use noq_proto::congestion::{Bbr3Config, Controller, ControllerFactory, ControllerMetrics};
use noq_proto::RttEstimator;

/// Minimo temporaneo della finestra di congestione e del ritmo d'invio, impostato dall'app.
///
/// Dopo un periodo con poco traffico (l'iPhone in background: passa solo l'audio) BBR dimentica la banda: al ritorno
/// la finestra è di pochi KB e il primo keyframe resta in coda per secondi, con il video fermo (test del 25/11, banco
/// `e2e_transport`). Al ritorno l'host imposta qui un minimo pari al bitrate che aveva prima della pausa, per pochi
/// secondi: intanto BBR riscopre la banda con il traffico vero, poi il minimo scade e decide solo lui.
#[derive(Debug, Default)]
pub struct RampFloor {
    window_bytes: AtomicU64,
    rate_bytes_per_s: AtomicU64,
    until_ms: AtomicU64,
}

fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

impl RampFloor {
    /// Imposta il minimo: ritmo `rate_bps` (bit/s) e finestra pari a 2 × ritmo × `rtt` (almeno 30 ms), per `duration`.
    pub fn set(&self, rate_bps: u64, rtt: Duration, duration: Duration) {
        let rate = rate_bps / 8;
        let rtt_us = (rtt.as_micros() as u64).max(30_000);
        let window = rate.saturating_mul(rtt_us).saturating_mul(2) / 1_000_000;
        self.window_bytes.store(window, Ordering::Relaxed);
        self.rate_bytes_per_s.store(rate, Ordering::Relaxed);
        self.until_ms.store(now_ms() + duration.as_millis() as u64, Ordering::Relaxed);
    }

    /// Toglie il minimo.
    pub fn clear(&self) {
        self.until_ms.store(0, Ordering::Relaxed);
    }

    /// Minimo in vigore: (finestra in byte, ritmo in byte/s), se non è scaduto.
    pub fn active(&self) -> Option<(u64, u64)> {
        if now_ms() >= self.until_ms.load(Ordering::Relaxed) {
            return None;
        }
        Some((self.window_bytes.load(Ordering::Relaxed), self.rate_bytes_per_s.load(Ordering::Relaxed)))
    }
}

/// Fabbrica del controllo: BBR v3 con il minimo condiviso dall'endpoint.
#[derive(Debug)]
pub(crate) struct FloorFactory {
    pub(crate) floor: Arc<RampFloor>,
}

impl ControllerFactory for FloorFactory {
    fn build(self: Arc<Self>, now: Instant, current_mtu: u16) -> Box<dyn Controller> {
        let inner = Arc::new(Bbr3Config::default()).build(now, current_mtu);
        Box::new(FloorController { inner, floor: Arc::clone(&self.floor) })
    }
}

/// BBR v3 più il minimo: finestra e ritmo d'invio (che `noq` prende dalle metriche del controllo) non scendono sotto
/// quelli del minimo in vigore.
#[derive(Debug)]
struct FloorController {
    inner: Box<dyn Controller>,
    floor: Arc<RampFloor>,
}

impl Controller for FloorController {
    fn on_sent(&mut self, now: Instant, bytes: u64, largest_pn: u64) {
        self.inner.on_sent(now, bytes, largest_pn);
    }

    fn on_packet_sent(&mut self, now: Instant, bytes: u16, pn: u64) {
        self.inner.on_packet_sent(now, bytes, pn);
    }

    fn on_cwnd_limited(&mut self) {
        self.inner.on_cwnd_limited();
    }

    fn on_ack(&mut self, now: Instant, sent: Instant, bytes: u64, pn: u64, app_limited: bool, rtt: &RttEstimator) {
        self.inner.on_ack(now, sent, bytes, pn, app_limited, rtt);
    }

    fn on_end_acks(&mut self, now: Instant, in_flight: u64, app_limited: bool, largest: Option<u64>) {
        self.inner.on_end_acks(now, in_flight, app_limited, largest);
    }

    fn on_congestion_event(
        &mut self,
        now: Instant,
        sent: Instant,
        is_persistent_congestion: bool,
        is_ecn: bool,
        lost_bytes: u64,
        largest_lost_pn: u64,
    ) {
        self.inner.on_congestion_event(now, sent, is_persistent_congestion, is_ecn, lost_bytes, largest_lost_pn);
    }

    fn on_packet_lost(&mut self, lost_bytes: u16, pn: u64, now: Instant) {
        self.inner.on_packet_lost(lost_bytes, pn, now);
    }

    fn on_spurious_congestion_event(&mut self) {
        self.inner.on_spurious_congestion_event();
    }

    fn on_mtu_update(&mut self, new_mtu: u16) {
        self.inner.on_mtu_update(new_mtu);
    }

    fn on_ack_frequency_update(&mut self, ack_eliciting_threshold: u64, requested_max_ack_delay: Duration) {
        self.inner.on_ack_frequency_update(ack_eliciting_threshold, requested_max_ack_delay);
    }

    fn window(&self) -> u64 {
        let w = self.inner.window();
        self.floor.active().map_or(w, |(floor, _)| w.max(floor))
    }

    fn metrics(&self) -> ControllerMetrics {
        let mut m = self.inner.metrics();
        if let Some((window, rate)) = self.floor.active() {
            m.congestion_window = m.congestion_window.max(window);
            m.pacing_rate = m.pacing_rate.map(|p| p.max(rate));
        }
        m
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(FloorController { inner: self.inner.clone_box(), floor: Arc::clone(&self.floor) })
    }

    fn initial_window(&self) -> u64 {
        self.inner.initial_window()
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_window_and_pacing_respect_the_floor() {
        let floor = Arc::new(RampFloor::default());
        let c = Arc::new(FloorFactory { floor: Arc::clone(&floor) }).build(Instant::now(), 1200);
        let base = c.window();
        floor.set(80_000_000, Duration::from_millis(100), Duration::from_secs(5));
        assert_eq!(c.window(), 2_000_000.max(base));
        let m = c.metrics();
        assert_eq!(m.congestion_window, c.window());
        if let Some(p) = m.pacing_rate {
            assert!(p >= 10_000_000, "ritmo {p}");
        }
        floor.clear();
        assert_eq!(c.window(), base);
    }
}
