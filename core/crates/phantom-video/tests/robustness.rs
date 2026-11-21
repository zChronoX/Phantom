//! Prove di robustezza del ricevitore video con frammenti ostili: valori estremi, frammenti incoerenti tra loro,
//! duplicati, mancanti, frame fuori ordine, tempo che salta o torna indietro. Il ricevitore è esposto alla rete e gira
//! dentro un'app: non deve andare in panico, non deve consegnare due volte lo stesso frame né in disordine.
//!
//! I frammenti passano da codifica e decodifica del protocollo, come nell'uso reale: quelli che il parser rifiuta non
//! arrivano al ricevitore.

use phantom_proto::{decode, video_flags, Codec, Header, Message, VideoFragment, MAX_VIDEO_PAYLOAD};
use phantom_video::{Output, ReceiverConfig, VideoReceiver};
use proptest::prelude::*;

#[derive(Clone, Debug)]
struct FragSpec {
    frame_id: u32,
    frag_count: u16,
    frag_size: u16,
    /// Quanto manca a `frag_count * frag_size` (a 0 il frame è pieno), ridotto in modulo alla dimensione.
    shortfall: u16,
    fec_group: u16,
    index: u16,
    parity: bool,
    key: bool,
    config: bool,
    retransmit: bool,
    /// `None` = lunghezza esatta; `Some(n)` = lunghezza sbagliata di proposito.
    payload_len: Option<u16>,
    h264: bool,
}

fn frame_ids() -> impl Strategy<Value = u32> {
    prop_oneof![
        8 => 0u32..6,
        2 => (u32::MAX - 3)..=u32::MAX,
        1 => any::<u32>(),
    ]
}

fn frag_spec() -> impl Strategy<Value = FragSpec> {
    (
        frame_ids(),
        prop_oneof![8 => 1u16..9, 1 => 1u16..400],
        1u16..80,
        any::<u16>(),
        prop_oneof![5 => 0u16..5, 1 => any::<u16>()],
        any::<u16>(),
        any::<bool>(),
        (any::<bool>(), any::<bool>(), any::<bool>()),
        prop_oneof![8 => Just(None), 2 => any::<u16>().prop_map(Some)],
        any::<bool>(),
    )
        .prop_map(
            |(
                frame_id,
                frag_count,
                frag_size,
                shortfall,
                fec_group,
                index,
                parity,
                (key, config, retransmit),
                payload_len,
                h264,
            )| {
                FragSpec {
                    frame_id,
                    frag_count,
                    frag_size,
                    shortfall,
                    fec_group,
                    index,
                    parity,
                    key,
                    config,
                    retransmit,
                    payload_len,
                    h264,
                }
            },
        )
}

#[derive(Clone, Debug)]
enum Op {
    Fragment(FragSpec),
    Poll,
    SetRtt(u64),
}

#[derive(Clone, Debug)]
struct Step {
    /// Variazione del tempo: positiva, nulla o, ogni tanto, negativa (un orologio che torna indietro un poco).
    dt: i64,
    op: Op,
}

fn step() -> impl Strategy<Value = Step> {
    let dt = prop_oneof![
        6 => 0i64..30_000,
        2 => 30_000i64..2_000_000,
        1 => -1_000i64..0,
        1 => Just(10_000_000_000i64),
    ];
    let op = prop_oneof![
        10 => frag_spec().prop_map(Op::Fragment),
        3 => Just(Op::Poll),
        1 => prop_oneof![Just(0u64), 1u64..1_000_000, Just(u64::MAX)].prop_map(Op::SetRtt),
    ];
    (dt, op).prop_map(|(dt, op)| Step { dt, op })
}

/// Un frammento in byte, se il parser lo accetta.
fn build(spec: &FragSpec) -> Option<Vec<u8>> {
    let size = spec.frag_size.min(MAX_VIDEO_PAYLOAD as u16).max(1);
    let max_total = u32::from(spec.frag_count) * u32::from(size);
    let total_len = max_total - (u32::from(spec.shortfall) % u32::from(size)).min(max_total);
    let mut flags = 0;
    if spec.key {
        flags |= video_flags::KEY;
    }
    if spec.config {
        flags |= video_flags::CONFIG;
    }
    if spec.retransmit {
        flags |= video_flags::RETRANSMIT;
    }
    if spec.parity {
        flags |= video_flags::PARITY;
    }
    let index = if spec.parity {
        let groups = phantom_proto::parity_groups(spec.frag_count, spec.fec_group).max(1);
        spec.frag_count.saturating_add(spec.index % groups)
    } else {
        spec.index % spec.frag_count
    };
    let exact = if spec.parity { usize::from(size) } else { phantom_proto::fragment_len(total_len, size, index) };
    let len = spec.payload_len.map_or(exact, |n| usize::from(n) % (usize::from(size) + 1));
    let payload = vec![0xA5u8; len];
    let frag = VideoFragment {
        frame_id: spec.frame_id,
        frag_index: index,
        frag_count: spec.frag_count,
        flags,
        codec: if spec.h264 { Codec::H264 } else { Codec::H265 },
        frag_size: size,
        total_len,
        capture_ts_us: 123,
        fec_group: spec.fec_group,
        payload: &payload,
    };
    Message::Video(frag).encode(Header { conn_id: 1, seq: 0 }).ok()
}

/// `a` viene dopo `b` nell'aritmetica circolare dei numeri di frame.
fn newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < (1 << 31)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn receiver_survives_hostile_fragment_streams(steps in proptest::collection::vec(step(), 1..250)) {
        let mut rx = VideoReceiver::new(ReceiverConfig::default());
        let mut now: u64 = 1_000_000;
        let mut last_delivered: Option<u32> = None;
        for st in &steps {
            now = if st.dt >= 0 { now.saturating_add(st.dt as u64) } else { now.saturating_sub(st.dt.unsigned_abs()) };
            let outs = match &st.op {
                Op::Fragment(spec) => match build(spec) {
                    Some(bytes) => match decode(&bytes) {
                        Ok((_, Message::Video(v))) => rx.on_fragment(now, &v),
                        _ => Vec::new(),
                    },
                    None => Vec::new(),
                },
                Op::Poll => rx.poll(now),
                Op::SetRtt(r) => {
                    rx.set_rtt_us(*r);
                    Vec::new()
                }
            };
            for o in outs {
                if let Output::Frame(f) = o {
                    if let Some(prev) = last_delivered {
                        prop_assert!(newer(f.frame_id, prev), "frame {} dopo {}: doppio o in disordine", f.frame_id, prev);
                    }
                    last_delivered = Some(f.frame_id);
                }
            }
        }
        // alla fine il ricevitore si può ancora interrogare senza problemi
        let _ = rx.poll(now.saturating_add(10_000_000));
        let _ = rx.stats();
    }
}

// ───────────────────────────────────────── flussi coerenti con perdite, duplicati e riordino

use phantom_video::{EncodedFrame, VideoSender};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

fn pattern(frame_id: u32, len: usize) -> Vec<u8> {
    (0..len).map(|i| (i as u32).wrapping_mul(31).wrapping_add(frame_id.wrapping_mul(7)) as u8).collect()
}

#[derive(Clone, Debug)]
struct Fate {
    /// Il datagramma si perde se vale meno di `loss_per_256`.
    roll: u8,
    /// Ritardo aggiuntivo (µs): crea riordino.
    extra_us: u32,
    /// Arriva anche una seconda volta.
    dup: bool,
}

fn fate() -> impl Strategy<Value = Fate> {
    (any::<u8>(), prop_oneof![5 => Just(0u32), 3 => 0u32..4_000, 1 => 0u32..60_000], prop::bool::weighted(0.05))
        .prop_map(|(roll, extra_us, dup)| Fate { roll, extra_us, dup })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 120, ..ProptestConfig::default() })]

    /// Con qualunque schema di perdite, duplicati e riordino: ogni frame consegnato è identico all'originale (anche
    /// quando un frammento è stato ricostruito con la parità), i frame escono in ordine e senza ripetizioni; e senza
    /// perdite arriva tutto.
    #[test]
    fn delivered_frames_are_intact_and_ordered(
        lens in proptest::collection::vec(0usize..9_000, 1..25),
        frag_size in 40u16..1_164,
        fec_group in 0u16..9,
        loss_per_256 in prop_oneof![Just(0u8), 1u8..40, 40u8..120],
        fates in proptest::collection::vec(fate(), 400..401),
    ) {
        let mut tx = VideoSender::new(7, frag_size, fec_group, 64);
        let mut rx = VideoReceiver::new(ReceiverConfig::default());
        rx.set_rtt_us(2_000);

        // (istante di arrivo, numero d'ordine, byte)
        let mut inflight: BinaryHeap<Reverse<(u64, u64, Vec<u8>)>> = BinaryHeap::new();
        let mut order = 0u64;
        let mut fate_i = 0usize;
        let mut originals: Vec<Vec<u8>> = Vec::new();
        let mut schedule = |inflight: &mut BinaryHeap<Reverse<(u64, u64, Vec<u8>)>>, now: u64, base_delay: u64, dgrams: Vec<Vec<u8>>| {
            for d in dgrams {
                let f = &fates[fate_i % fates.len()];
                fate_i += 1;
                if f.roll < loss_per_256 {
                    continue;
                }
                let copies = if f.dup { 2 } else { 1 };
                for k in 0..copies {
                    order += 1;
                    inflight.push(Reverse((now + base_delay + u64::from(f.extra_us) * (k + 1), order, d.clone())));
                }
            }
        };

        // il video: un frame ogni 8333 µs, un keyframe ogni 5 (rete di sicurezza per il recupero)
        let frame_us = 8_333u64;
        let mut next_frame = 0usize;
        let mut now = 0u64;
        let end = lens.len() as u64 * frame_us + 3_000_000;
        let mut last_delivered: Option<u32> = None;
        let mut delivered = 0usize;
        while now < end {
            while next_frame < lens.len() && now >= next_frame as u64 * frame_us {
                let data = pattern(next_frame as u32, lens[next_frame]);
                originals.push(data.clone());
                let key = next_frame.is_multiple_of(5);
                let f = EncodedFrame { frame_id: next_frame as u32, codec: Codec::H265, key, config: key, capture_ts_us: now, data: &data };
                schedule(&mut inflight, now, 1_000, tx.send_frame(&f));
                next_frame += 1;
            }
            while let Some(Reverse((t, _, _))) = inflight.peek() {
                if *t > now {
                    break;
                }
                let Some(Reverse((_, _, bytes))) = inflight.pop() else { break };
                let Ok((_, Message::Video(v))) = decode(&bytes) else { continue };
                let outs = rx.on_fragment(now, &v);
                handle(outs, now, &mut tx, &mut schedule, &mut inflight, &originals, &mut last_delivered, &mut delivered)?;
            }
            let outs = rx.poll(now);
            handle(outs, now, &mut tx, &mut schedule, &mut inflight, &originals, &mut last_delivered, &mut delivered)?;
            now += 500;
        }
        if std::env::var_os("PHANTOM_PROP_DEBUG").is_some() {
            eprintln!("perdita {loss_per_256}/256 · parità {fec_group} · frame {} · consegnati {delivered}", lens.len());
        }
        if loss_per_256 == 0 && fates.iter().all(|f| f.extra_us == 0) {
            prop_assert_eq!(delivered, lens.len(), "senza perdite né riordino arriva tutto");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle(
    outs: Vec<Output>,
    now: u64,
    tx: &mut VideoSender,
    schedule: &mut impl FnMut(&mut BinaryHeap<Reverse<(u64, u64, Vec<u8>)>>, u64, u64, Vec<Vec<u8>>),
    inflight: &mut BinaryHeap<Reverse<(u64, u64, Vec<u8>)>>,
    originals: &[Vec<u8>],
    last_delivered: &mut Option<u32>,
    delivered: &mut usize,
) -> Result<(), TestCaseError> {
    for o in outs {
        match o {
            Output::Frame(f) => {
                if let Some(prev) = *last_delivered {
                    prop_assert!(newer(f.frame_id, prev), "frame {} dopo {}: doppio o in disordine", f.frame_id, prev);
                }
                *last_delivered = Some(f.frame_id);
                let original = originals.get(f.frame_id as usize);
                prop_assert!(original.is_some(), "frame {} mai inviato", f.frame_id);
                prop_assert!(
                    original.is_some_and(|o| *o == f.data),
                    "frame {} alterato ({} byte contro {})",
                    f.frame_id,
                    f.data.len(),
                    original.map_or(0, Vec::len)
                );
                *delivered += 1;
            }
            Output::Nack { frame_id, missing } => {
                let rtx = tx.on_nack(now, frame_id, &missing);
                schedule(inflight, now, 1_000, rtx);
            }
            _ => {}
        }
    }
    Ok(())
}
