//! Phantom core: colla tra protocollo, orologi e video, più i due «attori» (host e client) usati nelle prove
//! end-to-end sul simulatore di rete e su UDP reale.
//!
//! In produzione host e client gireranno su Android e iPhone (binding UniFFI, fase 1 seconda parte); qui sono
//! scritti come macchine a stati pure — nessun thread, nessun sonno, tempo passato dal chiamante — così le stesse
//! prove girano veloci e identiche su ogni piattaforma.

pub mod rate;

use std::collections::VecDeque;

use phantom_clock::{ClockSync, Sample};
use phantom_proto::{decode, Codec, Finger, Header, Message, Stats, QUEUE_DELAY_UNKNOWN};
use phantom_video::{AssembledFrame, EncodedFrame, Output, ReceiverConfig, VideoReceiver, VideoSender};
use rate::{RateConfig, RateController};

pub use phantom_clock;
pub use phantom_netsim;
pub use phantom_proto;
pub use phantom_video;

/// Ping senza risposta che il client ricorda: oltre, i più vecchi si dimenticano (le loro risposte, se mai arrivano, si
/// ignorano).
const MAX_PENDING_PINGS: usize = 64;

/// Impostazioni di una sessione.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Identificatore di sessione.
    pub conn_id: u32,
    /// Dimensione dei frammenti video dati.
    pub frag_size: u16,
    /// Frammenti dati per gruppo di parità (0 = nessuna).
    pub fec_group: u16,
    /// Frame recenti conservati dall'host per i NACK.
    pub cache_frames: usize,
    /// Impostazioni del ricevitore video.
    pub receiver: ReceiverConfig,
    /// Intervallo dei ping di sincronia nei primi secondi (µs).
    pub sync_fast_us: u64,
    /// Intervallo dei ping a regime (µs). Servono anche a misurare la coda del collegamento (la coda si svuota o si
    /// riempie in decine di ms), quindi vanno fitti: 10 al secondo costano circa 1 KB/s.
    pub sync_slow_us: u64,
    /// Campioni di ping conservati per stimare orologio e RTT di riferimento (a 100 ms, 160 campioni = 16 s).
    pub clock_window: usize,
    /// Intervallo dello stato del tocco (µs): 240 Hz = 4167.
    pub touch_interval_us: u64,
    /// Distanza minima tra due keyframe su richiesta (µs): se il collegamento è saturo, un keyframe dopo l'altro peggiora
    /// la saturazione.
    pub min_keyframe_gap_us: u64,
    /// Ogni quanto il client manda le statistiche all'host (µs).
    pub stats_interval_us: u64,
    /// Bitrate video di partenza (bit/s): si parte bassi per misurare il RTT a collegamento scarico, poi si sale.
    pub start_bitrate_bps: u64,
    /// Bitrate video massimo (bit/s).
    pub max_bitrate_bps: u64,
    /// Bitrate video minimo sotto cui non si scende (bit/s).
    pub min_bitrate_bps: u64,
    /// Codec dei frame che l'host spedisce.
    pub codec: Codec,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            conn_id: 0x5048_0001,
            frag_size: phantom_proto::MAX_VIDEO_PAYLOAD as u16,
            fec_group: 10,
            cache_frames: 32,
            receiver: ReceiverConfig::default(),
            sync_fast_us: 100_000,
            sync_slow_us: 100_000,
            clock_window: 160,
            touch_interval_us: 4_167,
            min_keyframe_gap_us: 300_000,
            stats_interval_us: 200_000,
            start_bitrate_bps: 6_000_000,
            max_bitrate_bps: 20_000_000,
            min_bitrate_bps: 2_000_000,
            codec: Codec::H265,
        }
    }
}

/// Un frame consegnato al decoder del client, con le misure di latenza.
#[derive(Clone, Debug)]
pub struct Delivered {
    /// Il frame.
    pub frame: AssembledFrame,
    /// Latenza di rete e riassemblaggio: istante di consegna − cattura riportata sull'orologio del client (µs).
    /// `None` finché gli orologi non sono sincronizzati.
    pub latency_us: Option<i64>,
}

/// Statistiche di un lato.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostStats {
    /// Frame inviati.
    pub frames_sent: u64,
    /// Datagrammi video inviati (ritrasmissioni escluse).
    pub datagrams: u64,
    /// Ritrasmissioni.
    pub retransmitted: u64,
    /// Ritrasmissioni negate dal budget.
    pub retransmit_denied: u64,
    /// Richieste di keyframe ricevute.
    pub keyframe_requests: u64,
    /// Stati del tocco ricevuti.
    pub touch_states: u64,
    /// Riduzioni del bitrate decise dal controllo.
    pub bitrate_decreases: u64,
}

/// Lato host (Android): riceve ping, NACK, richieste di keyframe e tocco; spedisce video.
#[derive(Debug)]
pub struct Host {
    cfg: SessionConfig,
    video: VideoSender,
    seq: u32,
    /// Il client ha chiesto un keyframe (o la sessione è appena iniziata).
    keyframe_wanted: bool,
    last_key_ts_us: Option<u64>,
    /// Ultimo stato del tocco ricevuto (stato completo): numero di sequenza, istante sul client, dita.
    pub last_touch: Option<(u32, u64, Vec<Finger>)>,
    last_touch_seq: Option<u32>,
    /// `last_touch` è arrivato dopo l'ultima chiamata a [`Host::take_touch`].
    touch_fresh: bool,
    stats: HostStats,
    rate: RateController,
}

impl Host {
    /// Crea l'host.
    pub fn new(cfg: SessionConfig) -> Self {
        let video = VideoSender::new(cfg.conn_id, cfg.frag_size, cfg.fec_group, cfg.cache_frames);
        let rate = RateController::new(RateConfig {
            start_bps: cfg.start_bitrate_bps,
            min_bps: cfg.min_bitrate_bps,
            max_bps: cfg.max_bitrate_bps,
        });
        Host {
            cfg,
            video,
            seq: 0,
            keyframe_wanted: true,
            last_key_ts_us: None,
            last_touch: None,
            last_touch_seq: None,
            touch_fresh: false,
            stats: HostStats::default(),
            rate,
        }
    }

    /// Statistiche.
    pub fn stats(&self) -> HostStats {
        HostStats {
            retransmitted: self.video.retransmitted,
            retransmit_denied: self.video.retransmit_denied,
            bitrate_decreases: u64::from(self.rate.decreases()),
            ..self.stats
        }
    }

    /// Bitrate video che l'encoder dovrebbe usare adesso (bit/s), deciso dal [`rate::RateController`] in base alle
    /// statistiche riferite dal client.
    pub fn target_bitrate_bps(&self) -> u64 {
        self.rate.bitrate_bps()
    }

    /// Il controllo del bitrate (sola lettura: fase, giudizio, riduzioni).
    pub fn rate(&self) -> &RateController {
        &self.rate
    }

    /// `true` se l'encoder deve produrre un keyframe adesso: il client ne ha chiesto uno (o la sessione è appena
    /// iniziata) e dall'ultimo sono passati almeno `min_keyframe_gap_us`; se il collegamento è saturo, un keyframe
    /// dopo l'altro peggiora la saturazione. `now_us` è l'orologio dell'host.
    pub fn wants_keyframe(&self, now_us: u64) -> bool {
        self.keyframe_wanted
            && self.last_key_ts_us.is_none_or(|t| now_us.saturating_sub(t) >= self.cfg.min_keyframe_gap_us)
    }

    /// Spedisce un frame già codificato: restituisce i datagrammi da mandare al client. `key` dice cosa ha prodotto
    /// l'encoder; chi chiama deve chiedere un keyframe all'encoder quando [`Host::wants_keyframe`] è `true`.
    /// `capture_ts_us` è l'istante di cattura sull'orologio dell'host.
    pub fn send_frame(&mut self, frame_id: u32, key: bool, capture_ts_us: u64, data: &[u8]) -> Vec<Vec<u8>> {
        if key {
            self.keyframe_wanted = false;
            self.last_key_ts_us = Some(capture_ts_us);
        }
        let f = EncodedFrame { frame_id, codec: self.cfg.codec, key, config: key, capture_ts_us, data };
        let out = self.video.send_frame(&f);
        self.stats.frames_sent += 1;
        self.stats.datagrams += out.len() as u64;
        out
    }

    /// Lo stato del tocco arrivato dopo l'ultima chiamata, se c'è: (numero di sequenza, istante sul client, dita).
    pub fn take_touch(&mut self) -> Option<(u32, u64, Vec<Finger>)> {
        if std::mem::take(&mut self.touch_fresh) {
            self.last_touch.clone()
        } else {
            None
        }
    }

    /// Elabora un datagramma ricevuto dal client; `now_us` è l'istante di arrivo sull'orologio dell'host. Restituisce
    /// le risposte da rimandare al client.
    pub fn on_datagram(&mut self, now_us: u64, bytes: &[u8]) -> Vec<Vec<u8>> {
        let Ok((hdr, msg)) = decode(bytes) else { return Vec::new() };
        if hdr.conn_id != self.cfg.conn_id {
            return Vec::new();
        }
        let mut out = Vec::new();
        match msg {
            Message::Ping { id, t_client_us } => {
                let reply = Message::Pong { id, t_client_us, t_host_rx_us: now_us, t_host_tx_us: now_us };
                let h = Header { conn_id: self.cfg.conn_id, seq: self.seq };
                self.seq = self.seq.wrapping_add(1);
                if let Ok(b) = reply.encode(h) {
                    out.push(b);
                }
            }
            Message::Nack { frame_id, missing } => out.extend(self.video.on_nack(now_us, frame_id, &missing)),
            Message::Stats(s) => {
                self.rate.on_stats(now_us, &s);
            }
            Message::KeyframeRequest { .. } => {
                self.keyframe_wanted = true;
                self.stats.keyframe_requests += 1;
            }
            Message::Touch { state_seq, t_client_us, fingers } => {
                // stato completo: conta solo il più recente; uno arrivato in ritardo e fuori ordine si ignora
                let is_newest =
                    self.last_touch_seq.is_none_or(|s| state_seq.wrapping_sub(s) < (1 << 31) && state_seq != s);
                if is_newest {
                    self.last_touch_seq = Some(state_seq);
                    self.last_touch = Some((state_seq, t_client_us, fingers));
                    self.touch_fresh = true;
                    self.stats.touch_states += 1;
                }
            }
            _ => {}
        }
        out
    }
}

/// Lato client (iPhone): riceve video, sincronizza l'orologio, manda NACK, richieste di keyframe e tocco.
#[derive(Debug)]
pub struct Client {
    cfg: SessionConfig,
    video: VideoReceiver,
    /// Stimatore dell'orologio.
    pub clock: ClockSync,
    seq: u32,
    ping_id: u32,
    /// Ping in attesa di risposta: (numero, istante di partenza). Una risposta vale solo se la si è chiesta, e il tempo
    /// di partenza è quello annotato qui, non quello che l'altro lato dice di aver ricevuto.
    pending_pings: VecDeque<(u32, u64)>,
    next_ping_us: u64,
    next_touch_us: u64,
    next_stats_us: u64,
    last_pong_us: Option<u64>,
    touch_seq: u32,
    /// Frame consegnati al decoder.
    pub delivered: Vec<Delivered>,
}

impl Client {
    /// Crea il client.
    pub fn new(cfg: SessionConfig) -> Self {
        let video = VideoReceiver::new(cfg.receiver.clone());
        let clock = ClockSync::with_window(cfg.clock_window);
        Client {
            cfg,
            video,
            clock,
            seq: 0,
            ping_id: 0,
            pending_pings: VecDeque::new(),
            next_ping_us: 0,
            next_touch_us: 0,
            next_stats_us: 0,
            last_pong_us: None,
            touch_seq: 0,
            delivered: Vec::new(),
        }
    }

    /// Frame consegnati dal ricevitore dall'ultima chiamata, nell'ordine di arrivo: chi usa il client per un'app li
    /// preleva a ogni giro (nelle prove restano in `delivered` per l'analisi finale).
    pub fn take_frames(&mut self) -> Vec<Delivered> {
        std::mem::take(&mut self.delivered)
    }

    /// Statistiche del ricevitore video.
    pub fn video_stats(&self) -> phantom_video::ReceiverStats {
        self.video.stats()
    }

    fn header(&mut self) -> Header {
        let h = Header { conn_id: self.cfg.conn_id, seq: self.seq };
        self.seq = self.seq.wrapping_add(1);
        h
    }

    fn encode(&mut self, m: &Message<'_>) -> Option<Vec<u8>> {
        let h = self.header();
        m.encode(h).ok()
    }

    fn handle_outputs(&mut self, now_us: u64, outs: Vec<Output>, send: &mut Vec<Vec<u8>>) {
        for o in outs {
            match o {
                Output::Frame(f) => {
                    let latency = self.clock.age_of_host_time(now_us, f.capture_ts_us);
                    self.delivered.push(Delivered { frame: f, latency_us: latency });
                }
                Output::Nack { frame_id, missing } => {
                    if let Some(b) = self.encode(&Message::Nack { frame_id, missing }) {
                        send.push(b);
                    }
                }
                Output::RequestKeyframe { last_good_frame, reason } => {
                    if let Some(b) = self.encode(&Message::KeyframeRequest { reason, last_good_frame }) {
                        send.push(b);
                    }
                }
                Output::Dropped { .. } => {}
            }
        }
    }

    /// Elabora un datagramma ricevuto dall'host; restituisce i datagrammi da spedire (NACK, richieste di keyframe).
    pub fn on_datagram(&mut self, now_us: u64, bytes: &[u8]) -> Vec<Vec<u8>> {
        let Ok((hdr, msg)) = decode(bytes) else { return Vec::new() };
        if hdr.conn_id != self.cfg.conn_id {
            return Vec::new();
        }
        let mut send = Vec::new();
        match msg {
            Message::Video(v) => {
                let outs = self.video.on_fragment(now_us, &v);
                self.handle_outputs(now_us, outs, &mut send);
            }
            Message::Pong { id, t_host_rx_us, t_host_tx_us, .. } => {
                // solo risposte a ping veri, una volta sola: le copie duplicate e quelle inventate non contano
                let asked = self.pending_pings.iter().position(|(pid, _)| *pid == id);
                if let Some((_, t0)) = asked.and_then(|pos| self.pending_pings.remove(pos)) {
                    let accepted = self.clock.add(Sample { t0, t1: t_host_rx_us, t2: t_host_tx_us, t3: now_us });
                    if accepted {
                        self.last_pong_us = Some(now_us);
                    }
                }
            }
            _ => {}
        }
        send
    }

    /// Da chiamare di frequente: ping di sincronia, timer del video. Restituisce i datagrammi da spedire.
    pub fn tick(&mut self, now_us: u64) -> Vec<Vec<u8>> {
        let mut send = Vec::new();
        if let Some(rtt) = self.clock.median_rtt_us() {
            self.video.set_rtt_us(rtt);
        }
        if now_us >= self.next_ping_us {
            let id = self.ping_id;
            self.ping_id = self.ping_id.wrapping_add(1);
            self.pending_pings.push_back((id, now_us));
            while self.pending_pings.len() > MAX_PENDING_PINGS {
                self.pending_pings.pop_front();
            }
            if let Some(b) = self.encode(&Message::Ping { id, t_client_us: now_us }) {
                send.push(b);
            }
            let every = if self.clock.len() < 20 { self.cfg.sync_fast_us } else { self.cfg.sync_slow_us };
            self.next_ping_us = now_us + every;
        }
        let outs = self.video.poll(now_us);
        self.handle_outputs(now_us, outs, &mut send);
        if now_us >= self.next_stats_us {
            self.next_stats_us = now_us + self.cfg.stats_interval_us;
            if let Some(b) = self.stats_message(now_us) {
                send.push(b);
            }
        }
        send
    }

    /// Spedisce lo stato del tocco se è ora (240 Hz di default): `fingers` è lo stato corrente completo.
    pub fn touch_tick(&mut self, now_us: u64, fingers: &[Finger]) -> Option<Vec<u8>> {
        if now_us < self.next_touch_us {
            return None;
        }
        self.next_touch_us = now_us + self.cfg.touch_interval_us;
        let seq = self.touch_seq;
        self.touch_seq = self.touch_seq.wrapping_add(1);
        self.encode(&Message::Touch { state_seq: seq, t_client_us: now_us, fingers: fingers.to_vec() })
    }

    /// Ritardo di coda misurato adesso (µs), se i ping dicono qualcosa di recente.
    pub fn queue_delay_us(&self, now_us: u64) -> Option<u64> {
        let fresh = self.last_pong_us.is_some_and(|t| now_us.saturating_sub(t) <= 3 * self.cfg.sync_slow_us);
        if fresh {
            self.clock.queue_delay_us(3, 8)
        } else {
            None
        }
    }

    /// Statistiche per l'host.
    pub fn stats_message(&mut self, now_us: u64) -> Option<Vec<u8>> {
        let s = self.video.stats();
        let queue = self.queue_delay_us(now_us).map_or(QUEUE_DELAY_UNKNOWN, |q| q.min(u64::from(u32::MAX - 1)) as u32);
        let m = Message::Stats(Stats {
            rx_bytes: s.bytes as u32,
            frames_ok: s.frames_ok as u32,
            // i frame scartati in attesa del keyframe sono la conseguenza di una perdita già contata: non si contano due volte
            frames_lost: (s.dropped_timeout + s.dropped_stale + s.dropped_overflow) as u32,
            frames_fec: s.fec_recovered_frames as u32,
            queue_delay_us: queue,
            rtt_us: self.clock.recent_rtt_us(3).map_or(0, |r| r.min(u64::from(u32::MAX - 1)) as u32),
        });
        self.encode(&m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pongs_count_only_once_and_only_if_asked() {
        let cfg = SessionConfig::default();
        let mut client = Client::new(cfg.clone());
        let mut host = Host::new(cfg.clone());
        // il client manda il suo primo ping a t = 1000 µs; l'orologio dell'host è avanti di 5 s
        let ping = client.tick(1_000).remove(0);
        let pong = host.on_datagram(5_001_100, &ping).remove(0);
        client.on_datagram(1_800, &pong);
        assert_eq!(client.clock.len(), 1, "la risposta a un ping vero conta");
        client.on_datagram(1_900, &pong);
        assert_eq!(client.clock.len(), 1, "la copia duplicata no");
        // risposta mai chiesta, con orari assurdi: ignorata
        let forged = Message::Pong { id: 999, t_client_us: 0, t_host_rx_us: u64::MAX, t_host_tx_us: u64::MAX }
            .encode(Header { conn_id: cfg.conn_id, seq: 0 })
            .unwrap_or_default();
        client.on_datagram(2_000, &forged);
        assert_eq!(client.clock.len(), 1, "una risposta inventata no");
        // il tempo di partenza è quello annotato dal client, non quello scritto nella risposta
        let tampered = Message::Pong { id: 1, t_client_us: 123, t_host_rx_us: 5_150_100, t_host_tx_us: 5_150_100 }
            .encode(Header { conn_id: cfg.conn_id, seq: 1 })
            .unwrap_or_default();
        let ping2 = client
            .tick(150_000)
            .into_iter()
            .find(|d| matches!(decode(d), Ok((_, Message::Ping { .. }))))
            .unwrap_or_default();
        assert!(!ping2.is_empty(), "il secondo ping parte dopo l'intervallo");
        client.on_datagram(150_800, &tampered);
        assert_eq!(client.clock.len(), 2);
        let off = client.clock.offset_at(150_800).unwrap_or(0);
        assert!((off - 5_000_000).abs() < 2_000, "offset {off}: ignora il t_client scritto nella risposta (123)");
    }

    #[test]
    fn rejects_foreign_sessions_everywhere() {
        let cfg = SessionConfig::default();
        let other = SessionConfig { conn_id: cfg.conn_id ^ 0xFFFF, ..SessionConfig::default() };
        let mut host = Host::new(cfg.clone());
        let mut stranger = Client::new(other);
        for d in stranger.tick(0) {
            assert!(host.on_datagram(0, &d).is_empty(), "una sessione diversa non riceve risposta");
        }
        assert_eq!(host.stats().touch_states, 0);
    }
}
