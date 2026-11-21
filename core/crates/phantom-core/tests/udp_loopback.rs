//! Prove end-to-end su socket UDP veri (loopback), con tempo reale e thread: la stessa logica del simulatore, ma con
//! il sistema operativo in mezzo. Servono a scoprire ciò che il simulatore non può: dimensioni dei datagrammi
//! accettate dai socket, ordine di invio e ricezione, timer veri, orologi di processi diversi.
//!
//! Le soglie sono larghe di proposito: su macchine di CI sotto carico (macOS e Windows in particolare) lo scheduler
//! introduce pause di decine di millisecondi. Non misurano la qualità, verificano che tutto funzioni davvero.

use phantom_core::phantom_netsim::Rng;
use phantom_core::phantom_proto::Finger;
use phantom_core::{Client, Host, SessionConfig};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Orologio dell'host: 7 s avanti rispetto a quello del client (la sincronia deve scoprirlo da sola).
const HOST_OFFSET_US: u64 = 7_000_000;

fn us_since(epoch: Instant) -> u64 {
    epoch.elapsed().as_micros() as u64
}

fn bind_loopback() -> UdpSocket {
    let s = UdpSocket::bind("127.0.0.1:0").expect("bind");
    s.set_nonblocking(true).expect("nonblocking");
    s
}

struct HostReport {
    frames_sent: u32,
    touch_states: u64,
    final_bitrate_bps: u64,
}

struct ClientReport {
    delivered: usize,
    latencies_ms: Vec<f64>,
    /// Errore della stima dell'offset tra gli orologi alla fine (µs).
    clock_err_us: i64,
}

fn frame_bytes(rng: &mut Rng, id: u32, scale: f64, host_wants_key: bool) -> (Vec<u8>, bool) {
    let key = id.is_multiple_of(240) || host_wants_key;
    let base = if key { 120_000.0 } else { 10_000.0 + rng.up_to(20_000) as f64 };
    let len = ((base * scale) as usize).max(200);
    ((0..len).map(|i| (i as u32).wrapping_mul(2654435761).wrapping_add(id) as u8).collect(), key)
}

fn host_thread(
    sock: UdpSocket,
    peer: SocketAddr,
    epoch: Instant,
    stop: Arc<AtomicBool>,
    cfg: SessionConfig,
) -> HostReport {
    let mut host = Host::new(cfg);
    let mut rng = Rng::new(99);
    let host_clock = |real_us: u64| real_us + HOST_OFFSET_US;
    let frame_dur = Duration::from_micros(1_000_000 / 120);
    let mut next_frame = Instant::now() + Duration::from_millis(100);
    let mut id = 0u32;
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        while let Ok((n, _)) = sock.recv_from(&mut buf) {
            let now = us_since(epoch);
            for r in host.on_datagram(host_clock(now), &buf[..n]) {
                let _ = sock.send_to(&r, peer);
            }
        }
        while Instant::now() >= next_frame {
            let capture = host_clock(us_since(epoch));
            let wants_key = host.wants_keyframe(capture);
            let (data, key) = frame_bytes(&mut rng, id, host.target_bitrate_bps() as f64 / 20_000_000.0, wants_key);
            for d in host.send_frame(id, key, capture, &data) {
                let _ = sock.send_to(&d, peer);
            }
            id += 1;
            next_frame += frame_dur;
        }
        thread::sleep(Duration::from_micros(200));
    }
    HostReport {
        frames_sent: id,
        touch_states: host.stats().touch_states,
        final_bitrate_bps: host.target_bitrate_bps(),
    }
}

fn client_thread(
    sock: UdpSocket,
    peer: SocketAddr,
    epoch: Instant,
    stop: Arc<AtomicBool>,
    cfg: SessionConfig,
) -> ClientReport {
    let mut client = Client::new(cfg);
    let fingers = [Finger { id: 0, x: 20_000, y: 30_000, pressure: 100 }];
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        while let Ok((n, _)) = sock.recv_from(&mut buf) {
            let now = us_since(epoch);
            for r in client.on_datagram(now, &buf[..n]) {
                let _ = sock.send_to(&r, peer);
            }
        }
        let now = us_since(epoch);
        for d in client.tick(now) {
            let _ = sock.send_to(&d, peer);
        }
        if let Some(d) = client.touch_tick(now, &fingers) {
            let _ = sock.send_to(&d, peer);
        }
        thread::sleep(Duration::from_micros(200));
    }
    let end = us_since(epoch);
    let clock_err_us = client.clock.offset_at(end).map_or(i64::MAX, |o| o - HOST_OFFSET_US as i64);
    let latencies_ms = client.delivered.iter().filter_map(|d| d.latency_us).map(|l| l as f64 / 1000.0).collect();
    ClientReport { delivered: client.delivered.len(), latencies_ms, clock_err_us }
}

/// Inoltra i datagrammi tra host e client perdendone una frazione nel verso host → client.
fn relay_thread(
    host_side: UdpSocket,
    client_side: UdpSocket,
    host: SocketAddr,
    client: SocketAddr,
    loss: f64,
    stop: Arc<AtomicBool>,
) {
    let mut rng = Rng::new(5);
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        let mut idle = true;
        while let Ok((n, _)) = host_side.recv_from(&mut buf) {
            idle = false;
            if !rng.chance(loss) {
                let _ = client_side.send_to(&buf[..n], client);
            }
        }
        while let Ok((n, _)) = client_side.recv_from(&mut buf) {
            idle = false;
            let _ = host_side.send_to(&buf[..n], host);
        }
        if idle {
            thread::sleep(Duration::from_micros(100));
        }
    }
}

fn pct(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[((s.len() - 1) as f64 * q).round() as usize]
}

/// Esegue una sessione di `seconds` secondi; con `loss` > 0 si interpone un relay che perde pacchetti.
fn session(seconds: u64, loss: f64) -> (HostReport, ClientReport) {
    let epoch = Instant::now();
    let host_sock = bind_loopback();
    let client_sock = bind_loopback();
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = SessionConfig::default();

    let (host_peer, client_peer, relay): (SocketAddr, SocketAddr, Option<_>) = if loss > 0.0 {
        let rh = bind_loopback();
        let rc = bind_loopback();
        let (rh_addr, rc_addr) = (rh.local_addr().unwrap(), rc.local_addr().unwrap());
        let (h_addr, c_addr) = (host_sock.local_addr().unwrap(), client_sock.local_addr().unwrap());
        let st = stop.clone();
        let handle = thread::spawn(move || relay_thread(rh, rc, h_addr, c_addr, loss, st));
        (rh_addr, rc_addr, Some(handle))
    } else {
        (client_sock.local_addr().unwrap(), host_sock.local_addr().unwrap(), None)
    };

    let (st_h, cfg_h) = (stop.clone(), cfg.clone());
    let h = thread::spawn(move || host_thread(host_sock, host_peer, epoch, st_h, cfg_h));
    let (st_c, cfg_c) = (stop.clone(), cfg);
    let c = thread::spawn(move || client_thread(client_sock, client_peer, epoch, st_c, cfg_c));

    thread::sleep(Duration::from_secs(seconds));
    stop.store(true, Ordering::Relaxed);
    let (hr, cr) = (h.join().expect("host"), c.join().expect("client"));
    if let Some(r) = relay {
        r.join().expect("relay");
    }
    (hr, cr)
}

#[test]
fn clean_loopback_delivers_everything_and_syncs_clocks() {
    let (h, c) = session(4, 0.0);
    println!(
        "loopback pulito: inviati {} · consegnati {} · latenza p50 {:.1} p95 {:.1} ms · errore orologio {} µs · stati del tocco {} · bitrate {:.1} Mbps",
        h.frames_sent,
        c.delivered,
        pct(&c.latencies_ms, 0.5),
        pct(&c.latencies_ms, 0.95),
        c.clock_err_us,
        h.touch_states,
        h.final_bitrate_bps as f64 / 1e6
    );
    assert!(h.frames_sent > 400, "l'host ha mandato frame: {}", h.frames_sent);
    assert!(c.delivered as f64 >= h.frames_sent as f64 * 0.95, "consegnati {} su {}", c.delivered, h.frames_sent);
    assert!(pct(&c.latencies_ms, 0.5) < 60.0, "latenza mediana {:.1} ms", pct(&c.latencies_ms, 0.5));
    assert!(c.clock_err_us.abs() < 5_000, "sincronia degli orologi: errore {} µs", c.clock_err_us);
    assert!(h.touch_states > 300, "il tocco a 240 Hz arriva all'host: {}", h.touch_states);
    assert!(
        h.final_bitrate_bps >= 6_000_000,
        "il bitrate non scende su un collegamento pulito: {}",
        h.final_bitrate_bps
    );
}

#[test]
fn lossy_loopback_recovers_with_parity_and_retransmission() {
    let (h, c) = session(4, 0.03);
    println!(
        "loopback con 3% di perdita: inviati {} · consegnati {} ({:.1}%) · latenza p50 {:.1} p95 {:.1} ms · bitrate {:.1} Mbps",
        h.frames_sent,
        c.delivered,
        100.0 * c.delivered as f64 / h.frames_sent as f64,
        pct(&c.latencies_ms, 0.5),
        pct(&c.latencies_ms, 0.95),
        h.final_bitrate_bps as f64 / 1e6
    );
    assert!(c.delivered as f64 >= h.frames_sent as f64 * 0.8, "consegnati {} su {}", c.delivered, h.frames_sent);
    assert!(pct(&c.latencies_ms, 0.5) < 80.0, "latenza mediana {:.1} ms", pct(&c.latencies_ms, 0.5));
    assert!(h.touch_states > 300);
}
