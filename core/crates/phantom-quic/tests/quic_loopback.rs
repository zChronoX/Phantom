//! Prova end-to-end del trasporto QUIC: due endpoint veri su UDP loopback che
//! trasportano `Host` e `Client` del core, con apertura della sessione,
//! video, tocco, tastiera e chiusura ordinata.
//!
//! Come le prove UDP del core, usa thread e tempo reale: verifica che la
//! catena funzioni davvero (handshake, datagrammi, MTU), non la qualità.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use phantom_core::phantom_proto::caps::{H265, KEYFRAME_REQUEST, NACK, QUEUE_DELAY, XOR_PARITY};
use phantom_core::phantom_proto::{close_reason, Finger, MAX_DATAGRAM, MAX_VIDEO_PAYLOAD};
use phantom_core::{Client, Host, Negotiated, SessionConfig};
use phantom_quic::{generate_host_identity, ClientEndpoint, HostEndpoint, QuicError};

const HOST_NONCE: [u8; 16] = [0x22; 16];
const CLIENT_NONCE: [u8; 16] = [0x11; 16];
const SERVER_NAME: &str = "localhost";

fn loopback() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

fn now_us(epoch: Instant) -> u64 {
    epoch.elapsed().as_micros() as u64
}

/// Configurazione della sessione con il nonce del lato.
fn session_config(nonce: [u8; 16]) -> SessionConfig {
    SessionConfig { nonce, ..SessionConfig::default() }
}

/// Frammento video che sta nei datagrammi QUIC correnti: il massimo del
/// trasporto meno le due intestazioni del protocollo.
fn fragment_size(max_datagram_size: usize) -> u16 {
    let overhead = phantom_core::phantom_proto::HEADER_LEN + phantom_core::phantom_proto::VIDEO_HEADER_LEN;
    let payload = max_datagram_size.saturating_sub(overhead).min(MAX_VIDEO_PAYLOAD);
    payload.max(1) as u16
}

fn frame_bytes(id: u32, key: bool) -> Vec<u8> {
    let len = if key { 80_000 } else { 20_000 };
    (0..len).map(|i| (i as u32).wrapping_mul(2_654_435_761).wrapping_add(id) as u8).collect()
}

struct HostReport {
    frames_sent: u32,
    touch_states: u64,
    key_events: u64,
    negotiated: Option<Negotiated>,
    closed: bool,
    close_reason: Option<u8>,
    max_datagram_size: Option<usize>,
}

struct ClientReport {
    delivered: usize,
    negotiated: Option<Negotiated>,
    closed: bool,
    close_reason: Option<u8>,
}

fn host_thread(ready: mpsc::Sender<(SocketAddr, Vec<u8>)>, done: mpsc::Sender<HostReport>) {
    let endpoint = HostEndpoint::bind(loopback()).expect("bind host");
    let address = endpoint.local_addr().expect("indirizzo host");
    ready.send((address, endpoint.certificate_der().to_vec())).expect("host pronto");

    let link = endpoint.accept(Duration::from_secs(5)).expect("accept");
    let max_datagram_size = link.max_datagram_size();
    let mut config = session_config(HOST_NONCE);
    if let Some(max) = max_datagram_size {
        config.frag_size = fragment_size(max);
    }
    let mut host = Host::new(config);
    let epoch = Instant::now();
    let mut buf = vec![0u8; 2048];
    let mut frames_sent = 0u32;
    let mut next_frame = Instant::now() + Duration::from_millis(50);
    let started = Instant::now();

    while started.elapsed() < Duration::from_secs(3) {
        match link.recv_datagram(&mut buf, Duration::from_millis(2)) {
            Ok(n) => {
                let now = now_us(epoch);
                for reply in host.on_datagram(now, &buf[..n]) {
                    link.send_datagram(&reply).expect("risposta host");
                }
            }
            Err(QuicError::Timeout) => {}
            Err(_) => break,
        }
        let now = Instant::now();
        if now >= next_frame && host.negotiated().is_some() {
            let capture = now_us(epoch);
            let wants_key = host.wants_keyframe(capture);
            let data = frame_bytes(frames_sent, wants_key);
            for datagram in host.send_frame(frames_sent, wants_key, capture, &data) {
                link.send_datagram(&datagram).expect("datagramma video");
            }
            frames_sent += 1;
            next_frame += Duration::from_micros(8_333);
        }
        if host.is_closed() {
            break;
        }
    }

    done.send(HostReport {
        frames_sent,
        touch_states: host.stats().touch_states,
        key_events: host.stats().key_events,
        negotiated: host.negotiated(),
        closed: host.is_closed(),
        close_reason: host.close_reason(),
        max_datagram_size,
    })
    .expect("rapporto host");
}

fn client_thread(address: SocketAddr, certificate: Vec<u8>, done: mpsc::Sender<ClientReport>) {
    let endpoint = ClientEndpoint::bind(loopback()).expect("bind client");
    let link = endpoint.connect(address, SERVER_NAME, &certificate, Duration::from_secs(5)).expect("connect");
    let mut client = Client::new(session_config(CLIENT_NONCE));
    let fingers = [Finger { id: 0, x: 20_000, y: 30_000, pressure: 100 }];
    let epoch = Instant::now();
    let mut buf = vec![0u8; 2048];
    let mut next_touch = Instant::now();
    let mut next_key = Instant::now() + Duration::from_millis(300);
    let started = Instant::now();

    while started.elapsed() < Duration::from_secs(2) {
        let now = now_us(epoch);
        for datagram in client.tick(now) {
            link.send_datagram(&datagram).expect("tick client");
        }
        let instant = Instant::now();
        if instant >= next_touch {
            if let Some(datagram) = client.touch_tick(now, &fingers) {
                link.send_datagram(&datagram).expect("tocco");
            }
            next_touch += Duration::from_micros(4_167);
        }
        if instant >= next_key {
            for down in [true, false] {
                if let Some(datagram) = client.key(now, 30, down) {
                    link.send_datagram(&datagram).expect("tasto");
                }
            }
            next_key = instant + Duration::from_millis(500);
        }
        if let Some(datagram) = client.stats_message(now) {
            link.send_datagram(&datagram).expect("statistiche");
        }
        match link.recv_datagram(&mut buf, Duration::from_millis(2)) {
            Ok(n) => {
                for reply in client.on_datagram(now, &buf[..n]) {
                    link.send_datagram(&reply).expect("risposta client");
                }
            }
            Err(QuicError::Timeout) => {}
            Err(_) => break,
        }
        if client.is_closed() {
            break;
        }
    }

    let now = now_us(epoch);
    if let Some(datagram) = client.close(close_reason::NORMAL) {
        link.send_datagram(&datagram).expect("close");
    }
    // Pompa brevemente: il `Close` è un datagramma non affidabile e va
    // consegnato all'host prima che la connessione QUIC si chiuda.
    let deadline = Instant::now() + Duration::from_millis(200);
    while Instant::now() < deadline {
        match link.recv_datagram(&mut buf, Duration::from_millis(5)) {
            Ok(n) => {
                for reply in client.on_datagram(now, &buf[..n]) {
                    let _ = link.send_datagram(&reply);
                }
            }
            Err(QuicError::Timeout) => {}
            Err(_) => break,
        }
    }

    done.send(ClientReport {
        delivered: client.delivered.len(),
        negotiated: client.negotiated(),
        closed: client.is_closed(),
        close_reason: client.close_reason(),
    })
    .expect("rapporto client");
}

#[test]
fn session_over_quic_negotiates_exchanges_and_closes() {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (host_tx, host_rx) = mpsc::channel();
    let (client_tx, client_rx) = mpsc::channel();

    let host = thread::spawn(move || host_thread(ready_tx, host_tx));
    let (address, certificate) = ready_rx.recv_timeout(Duration::from_secs(5)).expect("host pronto");
    assert!(!certificate.is_empty(), "certificato dell'host");
    let client = thread::spawn(move || client_thread(address, certificate, client_tx));

    let client_report = client_rx.recv_timeout(Duration::from_secs(10)).expect("rapporto client");
    let host_report = host_rx.recv_timeout(Duration::from_secs(10)).expect("rapporto host");
    client.join().expect("thread client");
    host.join().expect("thread host");

    println!(
        "QUIC: inviati {} · consegnati {} · tocco {} · tasti {} · massimo datagramma {:?} · chiusi {} {}",
        host_report.frames_sent,
        client_report.delivered,
        host_report.touch_states,
        host_report.key_events,
        host_report.max_datagram_size,
        host_report.closed,
        client_report.closed
    );

    let negotiated_host = host_report.negotiated.expect("host: sessione negoziata");
    let negotiated_client = client_report.negotiated.expect("client: sessione negoziata");
    assert_eq!(negotiated_host.peer_nonce, CLIENT_NONCE);
    assert_eq!(negotiated_client.peer_nonce, HOST_NONCE);
    for bit in [H265, XOR_PARITY, NACK, KEYFRAME_REQUEST, QUEUE_DELAY] {
        assert_ne!(negotiated_host.caps & bit, 0, "funzione {bit:#x} negoziata dall'host");
        assert_ne!(negotiated_client.caps & bit, 0, "funzione {bit:#x} negoziata dal client");
    }

    assert!(host_report.max_datagram_size.unwrap_or(0) >= MAX_DATAGRAM / 2, "datagrammi QUIC disponibili");
    assert!(host_report.frames_sent > 100, "frame inviati: {}", host_report.frames_sent);
    assert!(
        client_report.delivered as f64 >= f64::from(host_report.frames_sent) * 0.5,
        "frame consegnati: {} su {}",
        client_report.delivered,
        host_report.frames_sent
    );
    assert!(host_report.touch_states > 100, "stati del tocco: {}", host_report.touch_states);
    assert!(host_report.key_events >= 2, "eventi di tastiera: {}", host_report.key_events);
    assert!(host_report.closed, "l'host ha ricevuto la chiusura");
    assert_eq!(host_report.close_reason, Some(close_reason::NORMAL));
    assert!(client_report.closed, "il client è chiuso");
    assert_eq!(client_report.close_reason, Some(close_reason::NORMAL));
}

#[test]
fn client_rejects_a_certificate_that_does_not_match() {
    let (ready_tx, ready_rx) = mpsc::channel();
    let host = thread::spawn(move || {
        let endpoint = HostEndpoint::bind(loopback()).expect("bind host");
        let address = endpoint.local_addr().expect("indirizzo host");
        ready_tx.send((address, endpoint.certificate_der().to_vec())).expect("host pronto");
        // Il client non deve completare il handshake: qui basta aspettare che
        // l'endpoint si chiuda quando il test finisce.
        let _ = endpoint.accept(Duration::from_secs(2));
    });

    let (address, mut certificate) = ready_rx.recv_timeout(Duration::from_secs(5)).expect("host pronto");
    for byte in &mut certificate {
        *byte ^= 0xff;
    }
    let client = ClientEndpoint::bind(loopback()).expect("bind client");
    let result = client.connect(address, SERVER_NAME, &certificate, Duration::from_secs(2));
    assert!(result.is_err(), "un certificato diverso non deve essere accettato");
    host.join().expect("thread host");
}

#[test]
fn empty_pinned_certificate_is_rejected_immediately() {
    let client = ClientEndpoint::bind(loopback()).expect("bind client");
    let result = client.connect(loopback(), SERVER_NAME, &[], Duration::from_millis(100));
    assert!(matches!(result, Err(QuicError::EmptyCertificate)));
}

#[test]
fn persistent_host_identity_survives_rebind() {
    let identity = generate_host_identity().expect("genera identità");
    assert!(!identity.cert_der.is_empty());
    assert!(!identity.key_der.is_empty());

    for _ in 0..2 {
        let cert = identity.cert_der.clone();
        let key = identity.key_der.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let host = thread::spawn(move || {
            let endpoint = HostEndpoint::bind_with_identity(loopback(), cert, key).expect("bind identità");
            let addr = endpoint.local_addr().expect("addr");
            ready_tx.send(addr).expect("ready");
            let link = endpoint.accept(Duration::from_secs(5)).expect("accept");
            let mut buf = [0u8; 64];
            let n = link.recv_datagram(&mut buf, Duration::from_secs(2)).expect("recv");
            assert_eq!(&buf[..n], b"ok");
        });

        let addr = ready_rx.recv_timeout(Duration::from_secs(5)).expect("addr");
        let client = ClientEndpoint::bind(loopback()).expect("bind client");
        let link = client.connect(addr, SERVER_NAME, &identity.cert_der, Duration::from_secs(5)).expect("connect");
        link.send_datagram(b"ok").expect("send");
        host.join().expect("join host");
    }
}
