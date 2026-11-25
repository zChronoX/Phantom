//! Prove end-to-end con il mittente QUIC modellato ([`phantom_netsim::transport::ModelSender`]): coda dei datagrammi,
//! finestra e ritmo d'invio alla BBR, scarto alla sorgente dell'host ([`Host::plan_frame`]) come nell'app.
//!
//! Il banco di `e2e_netsim` spedisce ogni pacchetto appena prodotto, quindi non vede la coda di invio dell'host né la
//! stima della banda del trasporto: non riproduceva la trappola del 5G dei test del 25/11 (bitrate fermo al minimo,
//! svuotamenti continui, qualità crollata dopo ogni ritorno dal background). Questo sì.
//!
//! Tutto a tempo virtuale; le prove con `#[ignore]` stampano i numeri per confrontare le versioni
//! (`cargo test --release -p phantom-core --test e2e_transport -- --ignored --nocapture`).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use phantom_core::pacing::LinkKind;
use phantom_core::phantom_netsim::transport::{ModelSender, SenderConfig};
use phantom_core::phantom_netsim::{Dir, LinkConfig, NetSim, Rng};
use phantom_core::phantom_proto::AudioCodec;
use phantom_core::{Client, Host, SessionConfig};

/// Uno scenario: collegamento, eventi, tipo di collegamento per l'host e parametri della sessione.
#[derive(Clone)]
struct Scenario {
    name: &'static str,
    link: LinkConfig,
    back: LinkConfig,
    events: Vec<(u64, LinkConfig)>,
    kind: LinkKind,
    fps: u64,
    seconds: u64,
    /// Bitrate di partenza, minimo e massimo (bit/s), come li imposta l'app.
    start_bps: u64,
    min_bps: u64,
    max_bps: u64,
    /// Finestre di background dell'iPhone: (inizio, fine) in µs.
    pauses: Vec<(u64, u64)>,
    /// Minimo temporaneo del trasporto al ritorno dal background.
    use_floor: bool,
}

/// Numeri di una prova.
#[derive(Debug, Default)]
struct Outcome {
    frames_encoded: u64,
    frames_delivered: u64,
    /// Bitrate obiettivo medio fuori dai primi 5 s e fuori dalle pause (bit/s).
    avg_target_bps: f64,
    /// Frazione del tempo (stessa finestra) con obiettivo ≥ 5,5 Mbit/s (soglia per salire a 720p 60 fps).
    share_above_720p: f64,
    drains: u64,
    skipped: u64,
    keyframes: u64,
    latency_p50_ms: f64,
    latency_p95_ms: f64,
    /// Somma dei buchi tra due frame mostrati oltre 200 ms (fuori dalle pause), in secondi.
    frozen_s: f64,
    /// Dopo ogni ritorno dal background: secondi prima del primo frame mostrato (media).
    resume_recovery_s: f64,
    /// Datagrammi video ritrasmessi su richiesta, rispetto ai primari.
    retransmit_ratio: f64,
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn in_pause(pauses: &[(u64, u64)], t: u64) -> bool {
    pauses.iter().any(|&(a, b)| t >= a && t < b + 2_000_000)
}

fn run(sc: &Scenario, seed: u64) -> Outcome {
    let mut sim = NetSim::new(seed, sc.link.clone(), sc.back.clone());
    let mut rng = Rng::new(seed ^ 0x5EED);
    let cfg = SessionConfig {
        start_bitrate_bps: sc.start_bps,
        min_bitrate_bps: sc.min_bps,
        max_bitrate_bps: sc.max_bps,
        audio_enabled: true,
        ..SessionConfig::default()
    };
    let mut host = Host::new(cfg.clone());
    let mut client = Client::new(cfg);
    let sender_cfg = if sc.kind == LinkKind::Lan { SenderConfig::lan() } else { SenderConfig::p2p() };
    let mut sender = ModelSender::new(sender_cfg);
    let mut acks: BinaryHeap<Reverse<(u64, u64)>> = BinaryHeap::new();

    let frame_us = 1_000_000 / sc.fps;
    let end = sc.seconds * 1_000_000;
    let mut next_frame = 300_000;
    let mut next_audio = 0u64;
    let mut frame_id = 0u32;
    let mut encoded = 0u64;
    let mut capture: HashMap<u32, u64> = HashMap::new();
    let mut latencies = Vec::new();
    let mut seen = 0usize;
    let mut last_shown: Option<u64> = None;
    let mut frozen_us = 0u64;
    let mut shown_at: Vec<u64> = Vec::new();
    let mut targets: Vec<(u64, u64)> = Vec::new();
    let mut paused_now = false;
    let mut last_pause_sent = 0u64;
    let mut next_event = 0usize;
    let mut events = sc.events.clone();
    let mut floor_for: Option<u64> = None;
    events.sort_by_key(|e| e.0);

    let mut t = 0u64;
    while t < end {
        while next_event < events.len() && events[next_event].0 <= t {
            sim.set_config(Dir::AtoB, events[next_event].1.clone());
            next_event += 1;
        }
        sim.advance_to(t);

        // iPhone: background e ritorno
        let should_pause = sc.pauses.iter().any(|&(a, b)| t >= a && t < b);
        if should_pause && (!paused_now || t - last_pause_sent >= 1_000_000) {
            if let Some(d) = client.video_pause(true) {
                sim.send(Dir::BtoA, d);
            }
            paused_now = true;
            last_pause_sent = t;
        } else if !should_pause && paused_now {
            for _ in 0..2 {
                if let Some(d) = client.video_pause(false) {
                    sim.send(Dir::BtoA, d);
                }
            }
            if let Some(d) = client.request_keyframe(t) {
                sim.send(Dir::BtoA, d);
            }
            paused_now = false;
        }

        // host: audio (sempre, anche in background) e video
        if t >= next_audio {
            if let Some(d) = host.send_audio(AudioCodec::AacLc, 48_000, 2, false, t, &[0u8; 300]) {
                sender.enqueue(d);
            }
            next_audio += 21_333;
        }
        // ritorno dal background: minimo temporaneo del trasporto, come `send_frame_via` nell'app
        if let Some(start) = host.resume_ramp(t) {
            if sc.use_floor && floor_for != Some(start) {
                floor_for = Some(start);
                sender.set_floor(host.target_bitrate_bps() * 5 / 4, start + phantom_core::RAMP_GRACE_US);
            }
        }
        while t >= next_frame {
            let bps = host.target_bitrate_bps();
            if !in_pause(&sc.pauses, next_frame) && next_frame >= 5_000_000 {
                targets.push((next_frame, bps));
            }
            let key = host.wants_keyframe(t);
            let avg = (bps / sc.fps / 8) as f64;
            let size = if key { (avg * 6.0).max(6_000.0) } else { avg * (0.6 + rng.next_f64() * 0.8) };
            let plan = host.plan_frame(t, key, sender.queued_bytes(), sender.capacity(), sc.kind);
            if !plan.paused {
                encoded += 1;
            }
            if !plan.paused && !plan.skip {
                let data = vec![0x5Au8; (size as usize).max(200)];
                capture.insert(frame_id, t);
                let mut bytes = 0usize;
                let mut overflows = 0u32;
                for d in host.send_frame(frame_id, key, t, &data) {
                    bytes += d.len();
                    if sender.enqueue(d) {
                        overflows += 1;
                    }
                }
                frame_id += 1;
                host.frame_sent(t, key, bytes, overflows, sc.kind);
            }
            next_frame += frame_us;
        }

        // trasporto: quello che la finestra e il ritmo lasciano partire
        for (id, d) in sender.poll(t) {
            let mut wire = id.to_be_bytes().to_vec();
            wire.extend_from_slice(&d);
            sim.send(Dir::AtoB, wire);
        }
        while let Some(&Reverse((at, id))) = acks.peek() {
            if at > t {
                break;
            }
            acks.pop();
            sender.on_ack(id, at);
        }

        // client
        for d in client.tick(t) {
            sim.send(Dir::BtoA, d);
        }
        while let Some((arr, wire)) = sim.recv(Dir::AtoB) {
            let id = u64::from_be_bytes(wire[..8].try_into().expect("id"));
            acks.push(Reverse((arr + sc.back.base_delay_us, id)));
            for r in client.on_datagram(arr, &wire[8..]) {
                sim.send(Dir::BtoA, r);
            }
        }
        while let Some((arr, d)) = sim.recv(Dir::BtoA) {
            for r in host.on_datagram(arr, &d) {
                sender.enqueue(r);
            }
        }
        while seen < client.delivered.len() {
            let f = &client.delivered[seen];
            seen += 1;
            if let Some(&cap) = capture.get(&f.frame.frame_id) {
                latencies.push((t - cap) as f64 / 1000.0);
            }
            if let Some(prev) = last_shown {
                let gap = t - prev;
                if gap > 200_000 && !in_pause(&sc.pauses, prev) && prev >= 2_000_000 {
                    frozen_us += gap - frame_us;
                }
            }
            last_shown = Some(t);
            shown_at.push(t);
        }
        if std::env::var_os("PHANTOM_TIMELINE").is_some() && t.is_multiple_of(1_000_000) && t > 0 {
            println!(
                "  t={:>3}s obiettivo {:>5.1} Mbit/s · stima trasporto {:>5.1} · finestra {:>6} B · coda {:>6} B · svuotamenti {} · saltati {} · consegnati {} · datagrammi {} ritrasmessi {} parità 1:{} · persi rete {}",
                t / 1_000_000,
                host.target_bitrate_bps() as f64 / 1e6,
                sender.bw_estimate_bps().unwrap_or(0) as f64 / 1e6,
                sender.cwnd(),
                sender.queued_bytes(),
                host.queue_drains(),
                host.frames_skipped(),
                client.delivered.len(),
                host.stats().datagrams,
                host.stats().retransmitted,
                host.fec_group(),
                sim.stats(Dir::AtoB).lost + sim.stats(Dir::AtoB).queue_drops
            );
        }
        t += 250;
    }

    // ritorno dal background: quanto passa prima del primo frame mostrato
    let mut recoveries = Vec::new();
    for &(_, b) in &sc.pauses {
        if b >= end {
            continue;
        }
        let first = shown_at.iter().find(|&&ts| ts >= b);
        recoveries.push(first.map_or((end - b) as f64 / 1e6, |ts| (ts - b) as f64 / 1e6));
    }

    let n = targets.len().max(1) as f64;
    Outcome {
        frames_encoded: encoded,
        frames_delivered: client.delivered.len() as u64,
        avg_target_bps: targets.iter().map(|(_, v)| *v as f64).sum::<f64>() / n,
        share_above_720p: targets.iter().filter(|(_, v)| *v >= 5_500_000).count() as f64 / n,
        drains: host.queue_drains(),
        skipped: host.frames_skipped(),
        keyframes: host.stats().keyframes_sent,
        latency_p50_ms: pct(&mut latencies.clone(), 0.5),
        latency_p95_ms: pct(&mut latencies, 0.95),
        frozen_s: frozen_us as f64 / 1e6,
        resume_recovery_s: if recoveries.is_empty() {
            0.0
        } else {
            recoveries.iter().sum::<f64>() / recoveries.len() as f64
        },
        retransmit_ratio: host.stats().retransmitted as f64 / host.stats().datagrams.max(1) as f64,
    }
}

/// Media di più seed.
fn average(sc: &Scenario, seeds: std::ops::Range<u64>) -> Outcome {
    let runs: Vec<Outcome> = seeds.map(|s| run(sc, s)).collect();
    let k = runs.len() as f64;
    let avg = |f: &dyn Fn(&Outcome) -> f64| runs.iter().map(f).sum::<f64>() / k;
    Outcome {
        frames_encoded: avg(&|o| o.frames_encoded as f64) as u64,
        frames_delivered: avg(&|o| o.frames_delivered as f64) as u64,
        avg_target_bps: avg(&|o| o.avg_target_bps),
        share_above_720p: avg(&|o| o.share_above_720p),
        drains: avg(&|o| o.drains as f64).round() as u64,
        skipped: avg(&|o| o.skipped as f64).round() as u64,
        keyframes: avg(&|o| o.keyframes as f64).round() as u64,
        latency_p50_ms: avg(&|o| o.latency_p50_ms),
        latency_p95_ms: avg(&|o| o.latency_p95_ms),
        frozen_s: avg(&|o| o.frozen_s),
        resume_recovery_s: avg(&|o| o.resume_recovery_s),
        retransmit_ratio: avg(&|o| o.retransmit_ratio),
    }
}

fn report(name: &str, o: &Outcome) {
    println!(
        "{name}: obiettivo medio {:.1} Mbit/s · sopra 5,5 Mbit/s {:.0}% · consegnati {}/{} · svuotamenti {} · saltati {} · keyframe {} · latenza p50 {:.0} p95 {:.0} ms · fermo {:.1} s · primo frame dopo il ritorno {:.2} s · ritrasmessi {:.0}%",
        o.avg_target_bps / 1e6,
        o.share_above_720p * 100.0,
        o.frames_delivered,
        o.frames_encoded,
        o.drains,
        o.skipped,
        o.keyframes,
        o.latency_p50_ms,
        o.latency_p95_ms,
        o.frozen_s,
        o.resume_recovery_s,
        o.retransmit_ratio * 100.0,
    );
}

/// Blocchi della radio: per `stall_us` i pacchetti restano fermi e arrivano tutti alla fine (ritardo extra che cala a
/// gradini da 50 ms; il collegamento è FIFO, quindi nessuno sorpassa), uno ogni `every_us` a partire da `from_us`.
fn stalls(link: &LinkConfig, from_us: u64, every_us: u64, stall_us: u64, until_us: u64) -> Vec<(u64, LinkConfig)> {
    let mut ev = Vec::new();
    let mut t = from_us;
    while t + stall_us < until_us {
        let steps = stall_us / 50_000;
        for s in 0..steps {
            let extra = stall_us - s * 50_000;
            ev.push((t + s * 50_000, LinkConfig { base_delay_us: link.base_delay_us + extra, ..link.clone() }));
        }
        ev.push((t + stall_us, link.clone()));
        t += every_us;
    }
    ev
}

/// 5G come nei test del 25/11: RTT ~65 ms con jitter, banda ampia, poche perdite, blocchi della radio da 400 ms ogni
/// 6 s, 30 fps.
fn cellular(seconds: u64) -> Scenario {
    let link = LinkConfig {
        base_delay_us: 30_000,
        jitter_us: 12_000,
        loss: 0.003,
        bandwidth_bps: Some(25_000_000),
        queue_bytes: 1024 * 1024,
        ..LinkConfig::ideal()
    };
    let back = LinkConfig { base_delay_us: 30_000, jitter_us: 6_000, ..LinkConfig::ideal() };
    Scenario {
        name: "5G",
        events: stalls(&link, 6_000_000, 6_000_000, 400_000, seconds * 1_000_000),
        link,
        back,
        kind: LinkKind::P2pDirect,
        fps: 30,
        seconds,
        start_bps: 6_000_000,
        min_bps: 1_000_000,
        max_bps: 20_000_000,
        pauses: Vec::new(),
        use_floor: true,
    }
}

/// 5G con due passaggi in background da 20 s.
fn cellular_with_pauses(seconds: u64) -> Scenario {
    Scenario {
        name: "5G con background",
        pauses: vec![(20_000_000, 40_000_000), (60_000_000, 80_000_000)],
        ..cellular(seconds)
    }
}

/// Relay lento: 2,5 Mbit/s, RTT 80 ms, coda corta.
fn slow_relay(seconds: u64) -> Scenario {
    let link = LinkConfig {
        base_delay_us: 40_000,
        jitter_us: 4_000,
        loss: 0.002,
        bandwidth_bps: Some(2_500_000),
        queue_bytes: 160 * 1024,
        ..LinkConfig::ideal()
    };
    let back = LinkConfig { base_delay_us: 40_000, jitter_us: 2_000, ..LinkConfig::ideal() };
    Scenario {
        name: "relay lento",
        link,
        back,
        events: Vec::new(),
        kind: LinkKind::Relay,
        fps: 30,
        seconds,
        start_bps: 6_000_000,
        min_bps: 600_000,
        max_bps: 20_000_000,
        pauses: Vec::new(),
        use_floor: true,
    }
}

/// Wi-Fi di casa dietro un powerline: banda buona, blocchi da 300 ms ogni 8 s, 60 fps.
fn home_wifi(seconds: u64) -> Scenario {
    let link = LinkConfig {
        base_delay_us: 3_000,
        jitter_us: 3_000,
        loss: 0.002,
        bandwidth_bps: Some(60_000_000),
        queue_bytes: 512 * 1024,
        ..LinkConfig::ideal()
    };
    let back = LinkConfig { base_delay_us: 3_000, jitter_us: 2_000, ..LinkConfig::ideal() };
    Scenario {
        name: "Wi-Fi con powerline",
        events: stalls(&link, 8_000_000, 8_000_000, 300_000, seconds * 1_000_000),
        link,
        back,
        kind: LinkKind::Lan,
        fps: 60,
        seconds,
        start_bps: 12_000_000,
        min_bps: 3_000_000,
        max_bps: 15_000_000,
        pauses: Vec::new(),
        use_floor: true,
    }
}

/// LAN con banda stretta (12 Mbit/s): congestione vera, il bitrate deve scendere sotto la capacità.
fn congested_lan(seconds: u64) -> Scenario {
    let link = LinkConfig {
        base_delay_us: 3_000,
        jitter_us: 1_000,
        bandwidth_bps: Some(12_000_000),
        queue_bytes: 256 * 1024,
        ..LinkConfig::ideal()
    };
    let back = LinkConfig { base_delay_us: 3_000, ..LinkConfig::ideal() };
    Scenario {
        name: "LAN stretta",
        link,
        back,
        events: Vec::new(),
        kind: LinkKind::Lan,
        fps: 60,
        seconds,
        start_bps: 12_000_000,
        min_bps: 3_000_000,
        max_bps: 32_000_000,
        pauses: Vec::new(),
        use_floor: true,
    }
}

#[test]
#[ignore]
fn transport_report() {
    for sc in [cellular(90), cellular_with_pauses(100), slow_relay(60), home_wifi(60), congested_lan(40)] {
        report(sc.name, &average(&sc, 1..4));
    }
    let sc = Scenario { use_floor: false, ..cellular_with_pauses(100) };
    report("5G con background, senza minimo", &average(&sc, 1..4));
}

#[test]
#[ignore]
fn transport_timeline() {
    let sc = match std::env::var("PHANTOM_SCENARIO").as_deref() {
        Ok("relay") => slow_relay(30),
        Ok("5g") => cellular(40),
        _ => cellular_with_pauses(60),
    };
    report(sc.name, &run(&sc, 1));
}

/// Il ritorno dal background su 5G: senza il minimo temporaneo del trasporto il primo frame arriva dopo ~5 s (il «0 fps»
/// dei test del 25/11), con il minimo in meno di un secondo e senza buttare il bitrate.
#[test]
fn resume_on_cellular_shows_video_within_a_second() {
    let with = average(&cellular_with_pauses(100), 1..3);
    let without = average(&Scenario { use_floor: false, ..cellular_with_pauses(100) }, 1..3);
    report("con minimo", &with);
    report("senza minimo", &without);
    assert!(with.resume_recovery_s < 1.0, "primo frame dopo {:.2} s", with.resume_recovery_s);
    assert!(without.resume_recovery_s > 2.0 * with.resume_recovery_s, "il minimo serve davvero");
    assert!(with.skipped * 2 < without.skipped, "meno frame saltati: {} contro {}", with.skipped, without.skipped);
    assert!(with.avg_target_bps >= 8_000_000.0, "bitrate medio {:.1} Mbit/s", with.avg_target_bps / 1e6);
}

/// Relay lento: niente ritrasmissioni di frammenti solo in ritardo (erano il 65% dei pacchetti) e video quasi mai fermo.
#[test]
fn slow_relay_does_not_retransmit_what_is_only_late() {
    let o = average(&slow_relay(40), 1..3);
    report("relay lento", &o);
    assert!(o.retransmit_ratio < 0.05, "ritrasmessi {:.0}%", o.retransmit_ratio * 100.0);
    assert!(o.frozen_s < 1.5, "fermo {:.1} s", o.frozen_s);
    assert!(o.drains <= 8, "svuotamenti {}", o.drains);
    assert!((1_200_000.0..2_500_000.0).contains(&o.avg_target_bps), "bitrate {:.1}", o.avg_target_bps / 1e6);
}

/// 5G a regime con blocchi della radio: bitrate alto e ritrasmissioni contenute.
#[test]
fn cellular_keeps_a_good_bitrate_through_radio_stalls() {
    let o = average(&cellular(60), 1..3);
    report("5G", &o);
    assert!(o.avg_target_bps >= 8_000_000.0, "bitrate {:.1}", o.avg_target_bps / 1e6);
    assert!(o.share_above_720p > 0.8, "sopra la soglia di 720p per il {:.0}%", o.share_above_720p * 100.0);
    assert!(o.retransmit_ratio < 0.1, "ritrasmessi {:.0}%", o.retransmit_ratio * 100.0);
}

/// LAN: Wi-Fi buono resta al massimo utile, quella stretta scende sotto la capacità.
#[test]
fn lan_stays_at_the_top_or_settles_below_capacity() {
    let wifi = average(&home_wifi(40), 1..3);
    report("Wi-Fi", &wifi);
    assert!(wifi.avg_target_bps >= 14_000_000.0, "bitrate {:.1}", wifi.avg_target_bps / 1e6);
    assert!(wifi.latency_p50_ms < 20.0);
    let narrow = average(&congested_lan(30), 1..3);
    report("LAN stretta", &narrow);
    assert!((6_000_000.0..11_000_000.0).contains(&narrow.avg_target_bps), "bitrate {:.1}", narrow.avg_target_bps / 1e6);
}
