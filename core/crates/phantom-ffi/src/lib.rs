//! Binding UniFFI del core di Phantom: l'involucro che Swift (app iPhone) e Kotlin (server Android) vedono.
//!
//! Qui non c'è logica: ogni funzione inoltra a [`phantom_core`], che è scritto come macchina a stati pura. Per questo
//! l'interfaccia è piccola e uniforme:
//!
//! - **il tempo lo passa l'app**: ogni chiamata riceve `now_us`, i microsecondi di un orologio monotono locale
//!   (`ProcessInfo.systemUptime` su iOS, `SystemClock.elapsedRealtimeNanos()` su Android). Il core non legge orologi
//!   e non dorme, quindi non crea thread né timer: l'app lo chiama quando arriva un datagramma e a intervalli
//!   regolari (`tick`);
//! - **i datagrammi entrano ed escono come byte**: ogni funzione che ne produce restituisce l'elenco da spedire, nel
//!   trasporto che l'app preferisce (UDP oggi, QUIC dalla fase 2);
//! - **nessuna funzione fallisce**: un datagramma malformato o di un'altra sessione viene ignorato (il parser non
//!   va mai in panico, vedi ADR-004);
//! - gli oggetti si possono usare da più thread: dentro c'è un blocco che serializza le chiamate.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use phantom_core::phantom_proto::{Codec, Finger, MAX_DATAGRAM, MAX_FINGERS};
use phantom_core::{Client, Delivered, Host, SessionConfig};

uniffi::setup_scaffolding!();

/// Prende il blocco anche se un altro thread è andato in panico mentre lo teneva: il core non lascia mai stati a metà,
/// e un'app non deve cadere a catena per questo.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Codec del video.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VideoCodec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    H265,
}

impl From<VideoCodec> for Codec {
    fn from(c: VideoCodec) -> Self {
        match c {
            VideoCodec::H264 => Codec::H264,
            VideoCodec::H265 => Codec::H265,
        }
    }
}

impl From<Codec> for VideoCodec {
    fn from(c: Codec) -> Self {
        match c {
            Codec::H264 => VideoCodec::H264,
            Codec::H265 => VideoCodec::H265,
        }
    }
}

/// Impostazioni di una sessione. Host e client devono usare gli stessi `conn_id` e `fec_group`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SessionSettings {
    /// Identificatore della sessione: i datagrammi con un altro valore si ignorano.
    pub conn_id: u32,
    /// Codec dei frame che l'host spedisce.
    pub codec: VideoCodec,
    /// Frammenti dati per ogni frammento di parità (0 = nessuna parità).
    pub fec_group: u16,
    /// Bitrate di partenza (bit/s): si parte bassi e si sale finché la rete regge.
    pub start_bitrate_bps: u64,
    /// Bitrate minimo (bit/s).
    pub min_bitrate_bps: u64,
    /// Bitrate massimo (bit/s).
    pub max_bitrate_bps: u64,
    /// Intervallo dello stato del tocco (µs); 4167 = 240 Hz.
    pub touch_interval_us: u64,
}

/// Le impostazioni predefinite: HEVC, parità 1:10, bitrate da 2 a 20 Mbps partendo da 6, tocco a 240 Hz.
#[uniffi::export]
pub fn default_session_settings() -> SessionSettings {
    let d = SessionConfig::default();
    SessionSettings {
        conn_id: d.conn_id,
        codec: d.codec.into(),
        fec_group: d.fec_group,
        start_bitrate_bps: d.start_bitrate_bps,
        min_bitrate_bps: d.min_bitrate_bps,
        max_bitrate_bps: d.max_bitrate_bps,
        touch_interval_us: d.touch_interval_us,
    }
}

impl From<SessionSettings> for SessionConfig {
    fn from(s: SessionSettings) -> Self {
        SessionConfig {
            conn_id: s.conn_id,
            codec: s.codec.into(),
            fec_group: s.fec_group,
            start_bitrate_bps: s.start_bitrate_bps,
            min_bitrate_bps: s.min_bitrate_bps,
            max_bitrate_bps: s.max_bitrate_bps,
            touch_interval_us: s.touch_interval_us,
            ..SessionConfig::default()
        }
    }
}

/// Dimensione massima di un datagramma del protocollo (byte): il trasporto deve poterne spedire di questa dimensione.
#[uniffi::export]
pub fn max_datagram_size() -> u32 {
    MAX_DATAGRAM as u32
}

/// Numero massimo di dita in uno stato del tocco.
#[uniffi::export]
pub fn max_fingers() -> u32 {
    MAX_FINGERS as u32
}

/// Un dito sul touchscreen. Le coordinate sono normalizzate: 0 = bordo sinistro/alto, 65535 = bordo destro/basso.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TouchPoint {
    /// Identificatore stabile del dito durante il contatto.
    pub id: u8,
    /// Ascissa normalizzata.
    pub x: u16,
    /// Ordinata normalizzata.
    pub y: u16,
    /// Pressione, 0–255.
    pub pressure: u8,
}

impl From<TouchPoint> for Finger {
    fn from(p: TouchPoint) -> Self {
        Finger { id: p.id, x: p.x, y: p.y, pressure: p.pressure }
    }
}

impl From<Finger> for TouchPoint {
    fn from(f: Finger) -> Self {
        TouchPoint { id: f.id, x: f.x, y: f.y, pressure: f.pressure }
    }
}

/// Stato completo del touchscreen ricevuto dall'host.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TouchState {
    /// Numero di sequenza dello stato (cresce di uno a ogni invio del client).
    pub seq: u32,
    /// Istante di campionamento sull'orologio del client (µs).
    pub client_time_us: u64,
    /// Le dita appoggiate in quell'istante; vuoto = nessun dito.
    pub fingers: Vec<TouchPoint>,
}

/// Un frame video completo, pronto per il decoder.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct VideoFrame {
    /// Numero del frame.
    pub frame_id: u32,
    /// Codec.
    pub codec: VideoCodec,
    /// Keyframe.
    pub key: bool,
    /// Contiene i parametri del codec (SPS/PPS/VPS) davanti ai dati.
    pub config: bool,
    /// Istante di cattura sull'orologio dell'host (µs).
    pub capture_ts_us: u64,
    /// Latenza dalla cattura alla consegna (µs), una volta sincronizzati gli orologi; `None` prima.
    pub latency_us: Option<i64>,
    /// Il frame è stato ricostruito (parità) o ritrasmesso: utile per le statistiche.
    pub recovered: bool,
    /// Dati codificati.
    pub data: Vec<u8>,
}

impl From<Delivered> for VideoFrame {
    fn from(d: Delivered) -> Self {
        VideoFrame {
            frame_id: d.frame.frame_id,
            codec: d.frame.codec.into(),
            key: d.frame.key,
            config: d.frame.config,
            capture_ts_us: d.frame.capture_ts_us,
            latency_us: d.latency_us,
            recovered: d.frame.recovered_fec || d.frame.retransmitted,
            data: d.frame.data,
        }
    }
}

/// Contatori dell'host.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HostCounters {
    /// Frame spediti.
    pub frames_sent: u64,
    /// Datagrammi video spediti (ritrasmissioni escluse).
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
    /// Bitrate che l'encoder dovrebbe usare adesso (bit/s).
    pub target_bitrate_bps: u64,
}

/// Contatori e misure del client.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ClientCounters {
    /// Frame consegnati al decoder.
    pub frames_ok: u64,
    /// Frame persi per cause di rete (scaduti, troppo vecchi, troppi in volo).
    pub frames_lost: u64,
    /// Frame scartati in attesa di un keyframe dopo una perdita.
    pub frames_dropped_waiting_key: u64,
    /// Frame recuperati con la parità.
    pub frames_recovered_by_parity: u64,
    /// Frammenti ricevuti.
    pub fragments: u64,
    /// Byte di video ricevuti.
    pub bytes: u64,
    /// Richieste di ritrasmissione spedite.
    pub nacks: u64,
    /// Richieste di keyframe spedite (ripetizioni comprese).
    pub keyframe_requests: u64,
    /// Di cui ripetizioni, perché il keyframe atteso non era arrivato.
    pub keyframe_retries: u64,
    /// Tempo di andata e ritorno recente (µs), se misurato.
    pub rtt_us: Option<u64>,
    /// Ritardo di coda stimato sul collegamento (µs), se misurabile.
    pub queue_delay_us: Option<u64>,
    /// Orologio dell'host meno orologio del client (µs), stimato; `None` finché mancano campioni.
    pub clock_offset_us: Option<i64>,
}

struct HostInner {
    host: Host,
    next_frame_id: u32,
}

/// Lato host (Android): riceve ping, ritrasmissioni, richieste di keyframe e tocco dal client; spedisce video.
#[derive(uniffi::Object)]
pub struct PhantomHost {
    inner: Mutex<HostInner>,
}

#[uniffi::export]
impl PhantomHost {
    /// Crea l'host di una sessione.
    #[uniffi::constructor]
    pub fn new(settings: SessionSettings) -> Arc<Self> {
        Arc::new(PhantomHost { inner: Mutex::new(HostInner { host: Host::new(settings.into()), next_frame_id: 0 }) })
    }

    /// Elabora un datagramma arrivato dal client; `now_us` è l'ora locale dell'host. Restituisce i datagrammi da
    /// rimandare al client (risposte ai ping, frammenti ritrasmessi).
    pub fn on_datagram(&self, now_us: u64, data: Vec<u8>) -> Vec<Vec<u8>> {
        lock(&self.inner).host.on_datagram(now_us, &data)
    }

    /// `true` se l'encoder deve produrre un keyframe adesso (richiesta del client o inizio sessione): l'app lo chiede
    /// all'encoder (`requestSyncFrame` su MediaCodec) e poi passa a [`PhantomHost::send_frame`] il frame uscito.
    pub fn wants_keyframe(&self, now_us: u64) -> bool {
        lock(&self.inner).host.wants_keyframe(now_us)
    }

    /// Spedisce un frame già codificato. `key` dice se l'encoder ha prodotto un keyframe, `capture_ts_us` è l'istante
    /// di cattura sull'orologio dell'host. Restituisce i datagrammi da mandare al client.
    pub fn send_frame(&self, key: bool, capture_ts_us: u64, data: Vec<u8>) -> Vec<Vec<u8>> {
        let mut g = lock(&self.inner);
        let id = g.next_frame_id;
        g.next_frame_id = id.wrapping_add(1);
        g.host.send_frame(id, key, capture_ts_us, &data)
    }

    /// Bitrate che l'encoder dovrebbe usare adesso (bit/s): cambia con le condizioni della rete.
    pub fn target_bitrate_bps(&self) -> u64 {
        lock(&self.inner).host.target_bitrate_bps()
    }

    /// Lo stato del tocco arrivato dopo l'ultima chiamata, se ce n'è uno nuovo.
    pub fn take_touch(&self) -> Option<TouchState> {
        let (seq, client_time_us, fingers) = lock(&self.inner).host.take_touch()?;
        Some(TouchState { seq, client_time_us, fingers: fingers.into_iter().map(TouchPoint::from).collect() })
    }

    /// Contatori.
    pub fn counters(&self) -> HostCounters {
        let g = lock(&self.inner);
        let s = g.host.stats();
        HostCounters {
            frames_sent: s.frames_sent,
            datagrams: s.datagrams,
            retransmitted: s.retransmitted,
            retransmit_denied: s.retransmit_denied,
            keyframe_requests: s.keyframe_requests,
            touch_states: s.touch_states,
            bitrate_decreases: s.bitrate_decreases,
            target_bitrate_bps: g.host.target_bitrate_bps(),
        }
    }
}

/// Lato client (iPhone): riceve video, sincronizza l'orologio, chiede ritrasmissioni e keyframe, spedisce il tocco.
#[derive(uniffi::Object)]
pub struct PhantomClient {
    inner: Mutex<Client>,
}

#[uniffi::export]
impl PhantomClient {
    /// Crea il client di una sessione.
    #[uniffi::constructor]
    pub fn new(settings: SessionSettings) -> Arc<Self> {
        Arc::new(PhantomClient { inner: Mutex::new(Client::new(settings.into())) })
    }

    /// Elabora un datagramma arrivato dall'host; `now_us` è l'ora locale del client. Restituisce i datagrammi da
    /// rimandare all'host (richieste di ritrasmissione e di keyframe).
    pub fn on_datagram(&self, now_us: u64, data: Vec<u8>) -> Vec<Vec<u8>> {
        lock(&self.inner).on_datagram(now_us, &data)
    }

    /// Da chiamare di frequente (ogni millisecondo o a ogni giro del ciclo di rete): ping di sincronia, scadenze del
    /// video, statistiche per l'host. Restituisce i datagrammi da spedire all'host.
    pub fn tick(&self, now_us: u64) -> Vec<Vec<u8>> {
        lock(&self.inner).tick(now_us)
    }

    /// Da chiamare a ogni campione del touchscreen con le dita appoggiate adesso (vuoto = nessuna): spedisce lo stato
    /// al ritmo configurato (240 Hz) e restituisce il datagramma, se è ora. Più di 10 dita: le prime 10.
    pub fn touch_tick(&self, now_us: u64, fingers: Vec<TouchPoint>) -> Option<Vec<u8>> {
        let fingers: Vec<Finger> = fingers.into_iter().take(MAX_FINGERS).map(Finger::from).collect();
        lock(&self.inner).touch_tick(now_us, &fingers)
    }

    /// I frame completi arrivati dall'ultima chiamata, in ordine, pronti per il decoder.
    pub fn take_frames(&self) -> Vec<VideoFrame> {
        lock(&self.inner).take_frames().into_iter().map(VideoFrame::from).collect()
    }

    /// Contatori e misure; `now_us` è l'ora locale del client.
    pub fn counters(&self, now_us: u64) -> ClientCounters {
        let g = lock(&self.inner);
        let s = g.video_stats();
        ClientCounters {
            frames_ok: s.frames_ok,
            frames_lost: s.dropped_timeout + s.dropped_stale + s.dropped_overflow,
            frames_dropped_waiting_key: s.dropped_waiting_key,
            frames_recovered_by_parity: s.fec_recovered_frames,
            fragments: s.fragments,
            bytes: s.bytes,
            nacks: s.nacks,
            keyframe_requests: s.keyframe_requests,
            keyframe_retries: s.key_retries,
            rtt_us: g.clock.recent_rtt_us(3),
            queue_delay_us: g.queue_delay_us(now_us),
            clock_offset_us: g.clock.offset_at(now_us),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scambia datagrammi tra host e client finché non c'è più niente da spedire. Gli orologi dei due lati differiscono
    /// di `HOST_OFFSET_US`.
    const HOST_OFFSET_US: u64 = 5_000_000;

    #[derive(Clone, Copy)]
    enum To {
        Host,
        Client,
    }

    fn pump(host: &PhantomHost, client: &PhantomClient, t: u64, mut queue: Vec<(To, Vec<u8>)>) {
        while let Some((to, d)) = queue.pop() {
            match to {
                To::Host => queue.extend(host.on_datagram(t + HOST_OFFSET_US, d).into_iter().map(|r| (To::Client, r))),
                To::Client => queue.extend(client.on_datagram(t, d).into_iter().map(|r| (To::Host, r))),
            }
        }
    }

    #[test]
    fn full_loop_through_the_ffi_types() {
        let host = PhantomHost::new(default_session_settings());
        let client = PhantomClient::new(default_session_settings());
        let fingers = vec![TouchPoint { id: 3, x: 1000, y: 2000, pressure: 90 }];
        let mut frames = Vec::new();
        let mut touches = 0;
        let mut first_key = None;
        // 3 secondi a passi di 1 ms, un frame ogni 8 ms
        for step in 0..3_000u64 {
            let t = step * 1_000;
            let mut out: Vec<(To, Vec<u8>)> = client.tick(t).into_iter().map(|d| (To::Host, d)).collect();
            out.extend(client.touch_tick(t, fingers.clone()).map(|d| (To::Host, d)));
            pump(&host, &client, t, out);
            if step % 8 == 0 && step >= 100 {
                let host_now = t + HOST_OFFSET_US;
                let key = host.wants_keyframe(host_now);
                first_key.get_or_insert(key);
                let data = vec![0xAB; if key { 40_000 } else { 9_000 }];
                let out = host.send_frame(key, host_now, data).into_iter().map(|d| (To::Client, d)).collect();
                pump(&host, &client, t, out);
            }
            frames.extend(client.take_frames());
            if let Some(ts) = host.take_touch() {
                touches += 1;
                assert_eq!(ts.fingers, vec![TouchPoint { id: 3, x: 1000, y: 2000, pressure: 90 }]);
            }
        }
        assert_eq!(first_key, Some(true), "all'inizio della sessione l'host chiede un keyframe");
        assert!(frames.len() > 330, "frame ricevuti: {}", frames.len());
        assert!(frames[0].key && frames[0].config, "il primo frame è il keyframe");
        assert!(frames.iter().skip(1).filter(|f| f.key).count() <= 1, "i keyframe non si ripetono senza motivo");
        assert!(frames.iter().all(|f| f.data.len() == 40_000 || f.data.len() == 9_000));
        assert!(frames.windows(2).all(|w| w[1].frame_id > w[0].frame_id), "in ordine");
        // passo di 1 ms e intervallo di 4,167 ms: uno stato ogni 5 ms, cioè 600 in 3 s
        assert!(touches >= 590, "stati del tocco ricevuti dall'host: {touches}");
        let c = client.counters(3_000_000);
        let off = c.clock_offset_us.expect("orologi sincronizzati");
        assert!((off - HOST_OFFSET_US as i64).abs() < 50, "offset stimato {off} contro {HOST_OFFSET_US}");
        assert_eq!(c.frames_lost, 0);
        assert!(frames.iter().filter_map(|f| f.latency_us).all(|l| l.abs() < 1_000), "latenza zero in memoria");
        let h = host.counters();
        assert!(h.frames_sent > 330 && h.touch_states >= 590);
        assert!(h.target_bitrate_bps > 6_000_000, "il bitrate sale a collegamento libero: {}", h.target_bitrate_bps);
    }

    #[test]
    fn malformed_and_foreign_datagrams_are_ignored() {
        let host = PhantomHost::new(default_session_settings());
        let client = PhantomClient::new(default_session_settings());
        for junk in [vec![], vec![0u8; 3], vec![0x50, 0, 3], vec![0xFF; 1200], vec![0x50; 64]] {
            assert!(host.on_datagram(0, junk.clone()).is_empty());
            assert!(client.on_datagram(0, junk).is_empty());
        }
        // un ping valido di un'altra sessione
        let other = SessionSettings { conn_id: 0xDEAD_BEEF, ..default_session_settings() };
        let stranger = PhantomClient::new(other);
        for d in stranger.tick(0) {
            assert!(host.on_datagram(0, d).is_empty(), "una sessione diversa non riceve risposta");
        }
        assert_eq!(host.counters().touch_states, 0);
    }

    #[test]
    fn more_than_ten_fingers_are_truncated_not_dropped() {
        let host = PhantomHost::new(default_session_settings());
        let client = PhantomClient::new(default_session_settings());
        let fingers: Vec<TouchPoint> =
            (0..14).map(|i| TouchPoint { id: i, x: u16::from(i) * 100, y: 5, pressure: 1 }).collect();
        let d = client.touch_tick(0, fingers).expect("lo stato parte");
        host.on_datagram(0, d);
        let ts = host.take_touch().expect("arrivato");
        assert_eq!(ts.fingers.len(), max_fingers() as usize);
        assert!(host.take_touch().is_none(), "uno stato già prelevato non si rivede");
    }

    #[test]
    fn settings_roundtrip_and_limits() {
        let d = default_session_settings();
        assert_eq!(d.codec, VideoCodec::H265);
        assert_eq!(d.fec_group, 10);
        assert_eq!(max_datagram_size(), 1200);
        let cfg: SessionConfig = SessionSettings { codec: VideoCodec::H264, fec_group: 0, ..d.clone() }.into();
        assert_eq!(cfg.codec, Codec::H264);
        assert_eq!(cfg.fec_group, 0);
        assert_eq!(VideoCodec::from(Codec::H264), VideoCodec::H264);
    }
}
