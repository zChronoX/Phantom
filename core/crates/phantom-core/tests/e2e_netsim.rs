//! Prove end-to-end sul simulatore di rete: un host che manda video a 120 fps e un client che lo riceve, con
//! sincronia degli orologi, ritrasmissioni, parità, richieste di keyframe e tocco a 240 Hz.
//!
//! Tutto a tempo virtuale: ogni prova dura millisecondi e dà sempre lo stesso risultato.

use phantom_core::phantom_netsim::{Burst, Dir, LinkConfig, NetSim, Rng};
use phantom_core::phantom_proto::Finger;
use phantom_core::{Client, Host, SessionConfig};

/// Orologio dell'host: 5 s avanti rispetto al client e con 20 ppm di deriva.
const HOST_OFFSET_US: i64 = 5_000_000;
const HOST_DRIFT_PPM: f64 = 20.0;

fn host_clock(real_us: u64) -> u64 {
    ((real_us as f64) * (1.0 + HOST_DRIFT_PPM * 1e-6)) as u64 + HOST_OFFSET_US as u64
}

struct Result {
    sent: u64,
    delivered: u64,
    latencies_ms: Vec<f64>, // latenza vera (tempo reale)
    measured_err_ms: Vec<f64>,
    keyframe_requests: u64,
    nacks: u64,
    fec_frames: u64,
    retransmitted: u64,
    touch_stale_ms: Vec<f64>,
    clock_err_us: i64,
    final_bitrate_bps: u64,
    last_third_ratio: f64,
    /// Datagrammi video primari inviati (ritrasmissioni escluse).
    datagrams: u64,
    bitrate_decreases: u64,
    /// Bitrate richiesto all'encoder, medio nell'ultimo terzo della prova (bit/s).
    avg_bitrate_last_third_bps: f64,
    /// Istanti (ms, tempo reale) in cui è stato consegnato ogni frame.
    delivery_times_ms: Vec<f64>,
}

fn pct(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[((s.len() - 1) as f64 * q).round() as usize]
}

/// Frame sintetici: ~20 Mbps a 120 fps (circa 20 KB per frame, ±50%), un keyframe da 120 KB ogni 2 s.
/// `scale` = bitrate richiesto dall'host / 20 Mbps: l'encoder simulato produce frame più piccoli quando gli si chiede di meno.
fn frame_data(rng: &mut Rng, id: u32, scale: f64, host_wants_key: bool) -> (Vec<u8>, bool) {
    // keyframe periodico ogni 2 s, oppure su richiesta dell'host: l'encoder simulato produce davvero un frame grosso
    let key = id % 240 == 0 || host_wants_key;
    let base = if key { 120_000.0 } else { 10_000.0 + rng.up_to(20_000) as f64 };
    let len = ((base * scale) as usize).max(200);
    let data: Vec<u8> = (0..len).map(|i| (i as u32).wrapping_mul(2654435761).wrapping_add(id) as u8).collect();
    (data, key)
}

fn run(seed: u64, a_to_b: LinkConfig, b_to_a: LinkConfig, cfg: SessionConfig, seconds: u64) -> Result {
    run_with(seed, a_to_b, b_to_a, cfg, seconds, &[])
}

/// Come `run`, con modifiche del collegamento a istanti prestabiliti: `(istante µs, verso, nuova configurazione)`.
fn run_with(
    seed: u64,
    a_to_b: LinkConfig,
    b_to_a: LinkConfig,
    cfg: SessionConfig,
    seconds: u64,
    events: &[(u64, Dir, LinkConfig)],
) -> Result {
    let mut sim = NetSim::new(seed, a_to_b, b_to_a); // A = host, B = client: AtoB = video (host → client)
    let mut pending_events: Vec<&(u64, Dir, LinkConfig)> = events.iter().collect();
    pending_events.sort_by_key(|e| e.0);
    let mut next_event = 0usize;
    let mut delivery_times_ms: Vec<f64> = Vec::new();
    let mut host = Host::new(cfg.clone());
    let mut client = Client::new(cfg);
    let mut rng = Rng::new(seed ^ 0xABCD);

    let frame_us = 1_000_000 / 120;
    let end = seconds * 1_000_000;
    let mut next_frame = 100_000; // il video parte a 0,1 s, dopo i primi ping
    let mut frame_id = 0u32;
    let mut capture_real: Vec<u64> = Vec::new(); // istante reale di cattura di ogni frame
    let mut true_lat = Vec::new();
    let mut meas_err = Vec::new();
    let mut touch_stale = Vec::new();
    let mut bitrate_trace: Vec<(u64, u64)> = Vec::new();
    let timeline = std::env::var_os("PHANTOM_E2E_TIMELINE").is_some();
    let mut last_sec = 0u64;
    let mut last_delivered = 0usize;
    let mut last_kreq = 0u64;
    let mut client_key_reqs = 0u64;
    let mut last_client_key_reqs = 0u64;
    let mut seen = 0usize;
    let fingers = [Finger { id: 0, x: 30_000, y: 40_000, pressure: 120 }];

    let mut t = 0u64;
    while t < end {
        while next_event < pending_events.len() && pending_events[next_event].0 <= t {
            let (_, dir, c) = pending_events[next_event];
            sim.set_config(*dir, c.clone());
            next_event += 1;
        }
        sim.advance_to(t);

        // host: nuovo frame
        while t >= next_frame {
            let bps = host.target_bitrate_bps();
            if frame_id % 12 == 0 {
                bitrate_trace.push((next_frame, bps));
            }
            let wants_key = host.wants_keyframe(host_clock(next_frame));
            let (data, key) = frame_data(&mut rng, frame_id, bps as f64 / 20_000_000.0, wants_key);
            capture_real.push(next_frame);
            for d in host.send_frame(frame_id, key, host_clock(next_frame), &data) {
                sim.send(Dir::AtoB, d);
            }
            frame_id += 1;
            next_frame += frame_us;
        }

        // client → rete: ping, timer video, tocco
        for d in client.tick(t) {
            if matches!(
                phantom_core::phantom_proto::decode(&d),
                Ok((_, phantom_core::phantom_proto::Message::KeyframeRequest { .. }))
            ) {
                client_key_reqs += 1;
            }
            sim.send(Dir::BtoA, d);
        }
        if let Some(d) = client.touch_tick(t, &fingers) {
            sim.send(Dir::BtoA, d);
        }

        // rete → host
        while let Some((arr, d)) = sim.recv(Dir::BtoA) {
            for r in host.on_datagram(host_clock(arr), &d) {
                sim.send(Dir::AtoB, r);
            }
            if let Some((_, t_client, _)) = &host.last_touch {
                // età dello stato del tocco appena arrivato: tempo reale di arrivo − campione (orologio client = reale)
                touch_stale.push((arr as i64 - *t_client as i64) as f64 / 1000.0);
            }
        }
        // rete → client
        while let Some((arr, d)) = sim.recv(Dir::AtoB) {
            for r in client.on_datagram(arr, &d) {
                if matches!(
                    phantom_core::phantom_proto::decode(&r),
                    Ok((_, phantom_core::phantom_proto::Message::KeyframeRequest { .. }))
                ) {
                    client_key_reqs += 1;
                }
                sim.send(Dir::BtoA, r);
            }
        }
        // latenze dei frame nuovi
        while seen < client.delivered.len() {
            let d = &client.delivered[seen];
            seen += 1;
            delivery_times_ms.push(t as f64 / 1000.0);
            let cap = capture_real[d.frame.frame_id as usize];
            let lat = (t - cap) as f64 / 1000.0;
            true_lat.push(lat);
            if let Some(m) = d.latency_us {
                meas_err.push((m as f64 / 1000.0 - lat).abs());
            }
        }
        if timeline && t / 1_000_000 != last_sec {
            last_sec = t / 1_000_000;
            let vs = client.video_stats();
            println!(
                "  [t={:>2}s] consegnati nel secondo {:>3} · keyframe: richiesti dal client {} arrivati all'host {} · bitrate {:.1} Mbps · accodati {} NACK {} scartati: timeout {} attesa-key {} vecchi {} sovraccarico {}",
                last_sec,
                client.delivered.len() - last_delivered,
                client_key_reqs - last_client_key_reqs,
                host.stats().keyframe_requests - last_kreq,
                host.target_bitrate_bps() as f64 / 1e6,
                vs.fragments,
                vs.nacks,
                vs.dropped_timeout,
                vs.dropped_waiting_key,
                vs.dropped_stale,
                vs.dropped_overflow
            );
            last_delivered = client.delivered.len();
            last_kreq = host.stats().keyframe_requests;
            last_client_key_reqs = client_key_reqs;
        }
        t += 250;
    }

    let vs = client.video_stats();
    // consegne e invii nell'ultimo terzo della prova: dopo che il controllo del bitrate ha avuto il tempo di assestarsi
    let third_start = (frame_id as usize) * 2 / 3;
    let last_third_ratio = client.delivered.iter().filter(|d| d.frame.frame_id as usize >= third_start).count() as f64
        / (frame_id as usize - third_start).max(1) as f64;
    let clock_err = client.clock.offset_at(end).map(|o| o - (host_clock(end) as i64 - end as i64)).unwrap_or(i64::MAX);
    Result {
        sent: u64::from(frame_id),
        delivered: client.delivered.len() as u64,
        latencies_ms: true_lat,
        measured_err_ms: meas_err,
        keyframe_requests: host.stats().keyframe_requests,
        nacks: vs.nacks,
        fec_frames: vs.fec_recovered_frames,
        retransmitted: host.stats().retransmitted,
        touch_stale_ms: touch_stale,
        clock_err_us: clock_err,
        final_bitrate_bps: host.target_bitrate_bps(),
        last_third_ratio,
        datagrams: host.stats().datagrams,
        bitrate_decreases: host.stats().bitrate_decreases,
        delivery_times_ms,
        avg_bitrate_last_third_bps: {
            let tail: Vec<f64> =
                bitrate_trace.iter().filter(|(ts, _)| *ts >= end * 2 / 3).map(|(_, b)| *b as f64).collect();
            tail.iter().sum::<f64>() / tail.len().max(1) as f64
        },
    }
}

fn report(name: &str, r: &Result) {
    println!(
        "{name}: inviati {} · consegnati {} ({:.1}%) · latenza p50 {:.1} p95 {:.1} p99 {:.1} max {:.1} ms · keyframe chiesti {} · NACK {} · ritrasmessi {} · frame salvati dalla parità {} · tocco p95 {:.1} ms · errore orologio {} µs",
        r.sent,
        r.delivered,
        100.0 * r.delivered as f64 / r.sent as f64,
        pct(&r.latencies_ms, 0.5),
        pct(&r.latencies_ms, 0.95),
        pct(&r.latencies_ms, 0.99),
        pct(&r.latencies_ms, 1.0),
        r.keyframe_requests,
        r.nacks,
        r.retransmitted,
        r.fec_frames,
        pct(&r.touch_stale_ms, 0.95),
        r.clock_err_us,
    );
    println!(
        "    bitrate: finale {:.1} Mbps, medio nell'ultimo terzo {:.1} Mbps, riduzioni {} · consegne nell'ultimo terzo {:.0}% · ritrasmessi/primari {:.0}%",
        r.final_bitrate_bps as f64 / 1e6,
        r.avg_bitrate_last_third_bps / 1e6,
        r.bitrate_decreases,
        100.0 * r.last_third_ratio,
        100.0 * r.retransmitted as f64 / r.datagrams.max(1) as f64
    );
}

#[test]
fn good_wifi_is_fast_and_lossless() {
    let r = run(1, LinkConfig::good_wifi(), LinkConfig::good_wifi(), SessionConfig::default(), 6);
    report("wifi buono", &r);
    assert!(r.delivered as f64 >= r.sent as f64 * 0.99, "consegnati {} su {}", r.delivered, r.sent);
    assert!(pct(&r.latencies_ms, 0.5) < 9.0, "p50 {}", pct(&r.latencies_ms, 0.5));
    assert!(pct(&r.latencies_ms, 0.95) < 16.0, "p95 {}", pct(&r.latencies_ms, 0.95));
    assert!(r.clock_err_us.abs() < 1_500, "errore dell'orologio {} µs", r.clock_err_us);
    assert!(
        pct(&r.measured_err_ms, 0.95) < 2.0,
        "la latenza misurata con l'orologio sincronizzato si discosta dal vero di {} ms",
        pct(&r.measured_err_ms, 0.95)
    );
    assert!(pct(&r.touch_stale_ms, 0.95) < 12.0, "tocco p95 {}", pct(&r.touch_stale_ms, 0.95));
}

#[test]
fn bad_wifi_stays_usable() {
    let r = run(2, LinkConfig::bad_wifi(), LinkConfig::bad_wifi(), SessionConfig::default(), 10);
    report("wifi disturbato", &r);
    assert!(r.delivered as f64 >= r.sent as f64 * 0.85, "consegnati {} su {}", r.delivered, r.sent);
    assert!(pct(&r.latencies_ms, 0.5) < 25.0, "p50 {}", pct(&r.latencies_ms, 0.5));
    assert!(pct(&r.latencies_ms, 0.99) < 120.0, "p99 {}", pct(&r.latencies_ms, 0.99));
    assert!(r.fec_frames > 0 || r.nacks > 0, "con perdite devono intervenire parità o ritrasmissioni");
    assert!(
        r.avg_bitrate_last_third_bps > 15_000_000.0,
        "il jitter non deve scambiarsi per coda: bitrate medio {:.1} Mbps",
        r.avg_bitrate_last_third_bps / 1e6
    );
    assert!(r.clock_err_us.abs() < 4_000, "errore dell'orologio {} µs", r.clock_err_us);
    assert!(pct(&r.touch_stale_ms, 0.95) < 40.0, "tocco p95 {}", pct(&r.touch_stale_ms, 0.95));
}

#[test]
fn fec_reduces_dependence_on_retransmission() {
    // 2% di perdita casuale, collegamento veloce: la parità recupera molti frammenti senza aspettare il giro dei NACK
    let link = LinkConfig {
        base_delay_us: 4_000,
        jitter_us: 1_000,
        loss: 0.02,
        bandwidth_bps: Some(300_000_000),
        queue_bytes: 512 * 1024,
        ..LinkConfig::ideal()
    };
    let with = run(3, link.clone(), link.clone(), SessionConfig { fec_group: 6, ..SessionConfig::default() }, 8);
    let without = run(3, link.clone(), link, SessionConfig { fec_group: 0, ..SessionConfig::default() }, 8);
    report("con parità 1:6", &with);
    report("senza parità", &without);
    assert!(with.fec_frames > 50, "la parità deve salvare molti frame: {}", with.fec_frames);
    assert!(with.nacks < without.nacks, "con la parità servono meno NACK ({} contro {})", with.nacks, without.nacks);
    assert!(
        pct(&with.latencies_ms, 0.99) <= pct(&without.latencies_ms, 0.99) + 1.0,
        "la parità non deve peggiorare la coda della latenza"
    );
    assert!(with.delivered >= without.delivered, "né le consegne");
}

#[test]
fn burst_loss_triggers_keyframe_recovery() {
    // raffiche lunghe: i NACK non bastano, il client deve chiedere keyframe e ripartire.
    // Nove seed: quanti frame arrivano dipende da dove cadono le raffiche (da 0,69 a 0,92), quindi si guardano le
    // medie invece di una singola realizzazione, che può finire vicino alla soglia per puro caso.
    let link = LinkConfig {
        base_delay_us: 4_000,
        jitter_us: 2_000,
        burst: Some(Burst { mean_good_us: 400_000, mean_bad_us: 40_000, loss_in_bad: 0.95 }),
        bandwidth_bps: Some(300_000_000),
        queue_bytes: 512 * 1024,
        ..LinkConfig::ideal()
    };
    let mut ratios = Vec::new();
    let mut last_thirds = Vec::new();
    let mut bitrates = Vec::new();
    for seed in 4..=12 {
        let r = run(seed, link.clone(), LinkConfig::good_wifi(), SessionConfig::default(), 12);
        report(&format!("raffiche di perdite (seed {seed})"), &r);
        assert!(r.keyframe_requests > 0, "seed {seed}: dopo una raffica lunga si chiede un keyframe");
        assert!(
            r.delivered as f64 >= r.sent as f64 * 0.65,
            "seed {seed}: il sistema si riprende: consegnati {} su {}",
            r.delivered,
            r.sent
        );
        // le perdite a raffica su un collegamento scarico non sono congestione: il bitrate non crolla
        assert!(
            r.avg_bitrate_last_third_bps > 12_000_000.0,
            "seed {seed}: bitrate medio {:.1} Mbps",
            r.avg_bitrate_last_third_bps / 1e6
        );
        ratios.push(r.delivered as f64 / r.sent as f64);
        last_thirds.push(r.last_third_ratio);
        bitrates.push(r.avg_bitrate_last_third_bps);
    }
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
    last_thirds.sort_by(|a, b| a.partial_cmp(b).unwrap());
    bitrates.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    // su 30 seed con raffiche al 95% arrivano in media l'81% dei frame, l'80% nell'ultimo terzo
    let ratio_mean = mean(&ratios);
    let last_third_mean = mean(&last_thirds);
    let bitrate_mean = mean(&bitrates);
    assert!(ratio_mean >= 0.75, "media dei frame consegnati {ratio_mean:.3}");
    assert!(last_third_mean >= 0.75, "media dell'ultimo terzo {last_third_mean:.3}");
    assert!(bitrate_mean > 15_000_000.0, "bitrate medio {:.1} Mbps", bitrate_mean / 1e6);
}

#[test]
fn congested_link_converges_by_lowering_bitrate() {
    // 12 Mbps contro ~22 Mbps offerti all'inizio (video + parità): la coda si riempie e i frame non arrivano.
    // Il client lo riferisce, l'host abbassa il bitrate e il flusso si ristabilisce sotto la capacità del collegamento.
    let slow = LinkConfig {
        base_delay_us: 4_000,
        jitter_us: 1_000,
        bandwidth_bps: Some(12_000_000),
        queue_bytes: 64 * 1024,
        ..LinkConfig::ideal()
    };
    let r = run(5, slow, LinkConfig::good_wifi(), SessionConfig::default(), 20);
    report("collegamento saturo", &r);
    // capacità 12 Mbps meno parità (10%) e intestazioni: circa 10,5 Mbps di video utile
    let avg = r.avg_bitrate_last_third_bps / 1e6;
    assert!((6.0..=11.0).contains(&avg), "il bitrate si assesta sotto la capacità: {avg:.1} Mbps");
    assert!(r.last_third_ratio > 0.9, "dopo l'assestamento arrivano più di 9 frame su 10: {:.2}", r.last_third_ratio);
    assert!(r.bitrate_decreases <= 8, "e senza oscillare di continuo: {} riduzioni", r.bitrate_decreases);
    assert!(r.keyframe_requests < 25, "i keyframe su richiesta restano limitati: {}", r.keyframe_requests);
}

#[test]
fn retransmissions_never_amplify_congestion() {
    // senza controllo del bitrate (max = min = 20 Mbps) il collegamento resta saturo: le ritrasmissioni non devono
    // peggiorarlo oltre il budget
    let slow = LinkConfig {
        base_delay_us: 4_000,
        jitter_us: 1_000,
        bandwidth_bps: Some(12_000_000),
        queue_bytes: 64 * 1024,
        ..LinkConfig::ideal()
    };
    let cfg = SessionConfig { min_bitrate_bps: 20_000_000, ..SessionConfig::default() };
    let r = run(5, slow, LinkConfig::good_wifi(), cfg, 8);
    report("saturo senza adattamento", &r);
    assert!(r.retransmitted as f64 <= r.sent as f64 * 25.0, "ritrasmissioni {} per {} frame", r.retransmitted, r.sent);
    assert!(
        r.keyframe_requests <= 8 * 1_000_000 / 300_000 + 2,
        "i keyframe su richiesta sono limitati nel tempo: {}",
        r.keyframe_requests
    );
}

#[test]
fn stream_resumes_after_an_outage() {
    // il collegamento muore per 1,5 s in entrambi i versi (cambio di access point, ascensore) e poi torna
    let good = LinkConfig::good_wifi();
    let dead = LinkConfig { loss: 1.0, ..LinkConfig::good_wifi() };
    let events = [
        (4_000_000, Dir::AtoB, dead.clone()),
        (4_000_000, Dir::BtoA, dead),
        (5_500_000, Dir::AtoB, good.clone()),
        (5_500_000, Dir::BtoA, good.clone()),
    ];
    let r = run_with(6, good.clone(), good, SessionConfig::default(), 10, &events);
    report("interruzione di 1,5 s", &r);
    // niente arriva durante l'interruzione (più il ritardo di rete già in volo)
    let during = r.delivery_times_ms.iter().filter(|t| (4_100.0..5_500.0).contains(*t)).count();
    assert_eq!(during, 0, "durante l'interruzione non può arrivare nulla: {during}");
    // dopo il ripristino la prima immagine arriva in meno di 1 s
    let first_after = r.delivery_times_ms.iter().copied().find(|t| *t >= 5_500.0).expect("il flusso riparte");
    assert!(first_after - 5_500.0 < 1_000.0, "ripartenza dopo {:.0} ms", first_after - 5_500.0);
    // e gli ultimi 3 s sono regolari
    let tail = r.delivery_times_ms.iter().filter(|t| **t >= 7_000.0).count();
    assert!(tail as f64 > 0.9 * 360.0, "ultimi 3 s: {tail} frame su 360");
    assert!(r.keyframe_requests >= 1);
}

#[test]
fn hostile_reordering_is_survivable_and_bounded() {
    // jitter indipendente per pacchetto e molto riordino: non è un collegamento reale, ma il ricevitore non deve
    // impazzire né ritrasmettere senza freni
    let hostile = LinkConfig {
        base_delay_us: 5_000,
        jitter_us: 10_000,
        fifo: false,
        reorder: 0.05,
        reorder_extra_us: 10_000,
        bandwidth_bps: Some(200_000_000),
        queue_bytes: 256 * 1024,
        ..LinkConfig::ideal()
    };
    let r = run(7, hostile.clone(), hostile, SessionConfig::default(), 8);
    report("riordino ostile", &r);
    assert!(r.delivered as f64 >= r.sent as f64 * 0.9, "consegnati {} su {}", r.delivered, r.sent);
    assert!(
        r.retransmitted as f64 <= r.datagrams as f64 * 0.55,
        "le ritrasmissioni restano nel budget: {} su {} primari",
        r.retransmitted,
        r.datagrams
    );
}

#[test]
fn same_seed_same_result() {
    let a = run(9, LinkConfig::bad_wifi(), LinkConfig::bad_wifi(), SessionConfig::default(), 4);
    let b = run(9, LinkConfig::bad_wifi(), LinkConfig::bad_wifi(), SessionConfig::default(), 4);
    assert_eq!(
        (a.sent, a.delivered, a.keyframe_requests, a.nacks),
        (b.sent, b.delivered, b.keyframe_requests, b.nacks)
    );
    assert_eq!(a.latencies_ms, b.latencies_ms, "prove ripetibili al microsecondo");
}
