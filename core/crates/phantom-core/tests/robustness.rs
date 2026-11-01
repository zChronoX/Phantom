//! Prove di robustezza di host e client con messaggi ostili: ogni campo che arriva dalla rete può valere qualunque cosa
//! (orologi dell'altro lato assurdi, numeri di frame e di sequenza a caso, liste enormi, statistiche impossibili).
//! Nessuna combinazione deve far andare in panico il core, che nelle app gira dentro il processo.
//!
//! Il tempo locale (`now_us`) è invece quello di un orologio monotono vero: cresce, con qualche salto.

use phantom_core::phantom_proto::{Finger, Header, Message, Stats, MAX_FINGERS, QUEUE_DELAY_UNKNOWN};
use phantom_core::{Client, Host, SessionConfig};
use proptest::prelude::*;

fn finger() -> impl Strategy<Value = Finger> {
    (any::<u8>(), any::<u16>(), any::<u16>(), any::<u8>()).prop_map(|(id, x, y, pressure)| Finger {
        id,
        x,
        y,
        pressure,
    })
}

fn extreme_u64() -> impl Strategy<Value = u64> {
    prop_oneof![4 => any::<u64>(), 2 => 0u64..10, 1 => Just(u64::MAX), 1 => Just(1u64 << 63), 1 => Just((1u64 << 63) - 1)]
}

fn extreme_u32() -> impl Strategy<Value = u32> {
    prop_oneof![4 => any::<u32>(), 2 => 0u32..10, 1 => Just(u32::MAX), 1 => Just(QUEUE_DELAY_UNKNOWN)]
}

fn stats() -> impl Strategy<Value = Stats> {
    (extreme_u32(), extreme_u32(), extreme_u32(), extreme_u32(), extreme_u32(), extreme_u32()).prop_map(
        |(rx_bytes, frames_ok, frames_lost, frames_fec, queue_delay_us, rtt_us)| Stats {
            rx_bytes,
            frames_ok,
            frames_lost,
            frames_fec,
            queue_delay_us,
            rtt_us,
        },
    )
}

/// Messaggi che un client ostile può mandare all'host.
fn to_host() -> impl Strategy<Value = Message<'static>> {
    prop_oneof![
        3 => (any::<u32>(), extreme_u64()).prop_map(|(id, t_client_us)| Message::Ping { id, t_client_us }),
        3 => (extreme_u32(), proptest::collection::vec(any::<u16>(), 0..60)).prop_map(|(frame_id, missing)| Message::Nack { frame_id, missing }),
        2 => (any::<u8>(), any::<u32>()).prop_map(|(reason, last_good_frame)| Message::KeyframeRequest { reason, last_good_frame }),
        3 => (extreme_u32(), extreme_u64(), proptest::collection::vec(finger(), 0..=MAX_FINGERS))
            .prop_map(|(state_seq, t_client_us, fingers)| Message::Touch { state_seq, t_client_us, fingers }),
        4 => stats().prop_map(Message::Stats),
        1 => (any::<u32>(), extreme_u64(), any::<u16>(), any::<bool>()).prop_map(|(key_seq, t_client_us, code, down)| Message::Key { key_seq, t_client_us, code, down }),
        1 => any::<u8>().prop_map(|reason| Message::Close { reason }),
    ]
}

/// Messaggi non video che un host ostile può mandare al client.
fn to_client() -> impl Strategy<Value = Message<'static>> {
    prop_oneof![
        5 => (any::<u32>(), extreme_u64(), extreme_u64(), extreme_u64())
            .prop_map(|(id, t_client_us, t_host_rx_us, t_host_tx_us)| Message::Pong { id, t_client_us, t_host_rx_us, t_host_tx_us }),
        1 => (any::<u32>(), extreme_u64()).prop_map(|(id, t_client_us)| Message::Ping { id, t_client_us }),
        1 => stats().prop_map(Message::Stats),
        1 => any::<u8>().prop_map(|reason| Message::Close { reason }),
    ]
}

#[derive(Clone, Debug)]
enum HostOp {
    Message(Message<'static>, bool),
    SendFrame { len: usize, key: bool, id: u32, capture: u64 },
    WantsKeyframe,
    TakeTouch,
}

fn host_op() -> impl Strategy<Value = HostOp> {
    prop_oneof![
        8 => (to_host(), prop::bool::weighted(0.9)).prop_map(|(m, right_session)| HostOp::Message(m, right_session)),
        2 => (0usize..6_000, any::<bool>(), extreme_u32(), extreme_u64()).prop_map(|(len, key, id, capture)| HostOp::SendFrame { len, key, id, capture }),
        1 => Just(HostOp::WantsKeyframe),
        1 => Just(HostOp::TakeTouch),
    ]
}

fn time_step() -> impl Strategy<Value = u64> {
    prop_oneof![6 => 0u64..20_000, 2 => 20_000u64..3_000_000, 1 => Just(60_000_000_000u64)]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn host_survives_hostile_clients(ops in proptest::collection::vec((time_step(), host_op()), 1..200), start in 0u64..(1u64 << 40)) {
        let cfg = SessionConfig::default();
        let mut host = Host::new(cfg.clone());
        let mut now = start;
        for (dt, op) in ops {
            now += dt;
            match op {
                HostOp::Message(m, right_session) => {
                    let conn_id = if right_session { cfg.conn_id } else { cfg.conn_id ^ 1 };
                    if let Ok(bytes) = m.encode(Header { conn_id, seq: 0 }) {
                        let replies = host.on_datagram(now, &bytes);
                        prop_assert!(replies.iter().all(|r| r.len() <= phantom_core::phantom_proto::MAX_DATAGRAM));
                    }
                }
                HostOp::SendFrame { len, key, id, capture } => {
                    let data = vec![0x42u8; len];
                    let dgrams = host.send_frame(id, key, capture, &data);
                    prop_assert!(dgrams.iter().all(|d| d.len() <= phantom_core::phantom_proto::MAX_DATAGRAM));
                }
                HostOp::WantsKeyframe => {
                    let _ = host.wants_keyframe(now);
                }
                HostOp::TakeTouch => {
                    let _ = host.take_touch();
                }
            }
            let bps = host.target_bitrate_bps();
            prop_assert!(bps >= cfg.min_bitrate_bps && bps <= cfg.max_bitrate_bps, "bitrate fuori dai limiti: {bps}");
        }
        let _ = host.stats();
    }
}

#[derive(Clone, Debug)]
enum ClientOp {
    Message(Message<'static>),
    Garbage(Vec<u8>),
    Tick,
    Touch(Vec<Finger>),
    TakeFrames,
    StatsMessage,
}

fn client_op() -> impl Strategy<Value = ClientOp> {
    prop_oneof![
        8 => to_client().prop_map(ClientOp::Message),
        2 => proptest::collection::vec(any::<u8>(), 0..1300).prop_map(ClientOp::Garbage),
        6 => Just(ClientOp::Tick),
        2 => proptest::collection::vec(finger(), 0..=MAX_FINGERS).prop_map(ClientOp::Touch),
        2 => Just(ClientOp::TakeFrames),
        1 => Just(ClientOp::StatsMessage),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn client_survives_hostile_hosts(ops in proptest::collection::vec((time_step(), client_op()), 1..250), start in 0u64..(1u64 << 40)) {
        let cfg = SessionConfig::default();
        let mut client = Client::new(cfg.clone());
        let mut now = start;
        for (dt, op) in ops {
            now += dt;
            match op {
                ClientOp::Message(m) => {
                    if let Ok(bytes) = m.encode(Header { conn_id: cfg.conn_id, seq: 0 }) {
                        let out = client.on_datagram(now, &bytes);
                        prop_assert!(out.iter().all(|r| r.len() <= phantom_core::phantom_proto::MAX_DATAGRAM));
                    }
                }
                ClientOp::Garbage(bytes) => {
                    let _ = client.on_datagram(now, &bytes);
                }
                ClientOp::Tick => {
                    let out = client.tick(now);
                    prop_assert!(out.iter().all(|r| r.len() <= phantom_core::phantom_proto::MAX_DATAGRAM));
                }
                ClientOp::Touch(fingers) => {
                    let _ = client.touch_tick(now, &fingers);
                }
                ClientOp::TakeFrames => {
                    let _ = client.take_frames();
                }
                ClientOp::StatsMessage => {
                    let _ = client.stats_message(now);
                    let _ = client.queue_delay_us(now);
                }
            }
        }
        let _ = client.clock.offset_at(now);
        let _ = client.video_stats();
    }
}
