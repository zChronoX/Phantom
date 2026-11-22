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

use std::fmt;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use phantom_core::jitter::{AudioJitter, JitterConfig};
use phantom_core::pacing::{FrameDecision, SourceDropper};
use phantom_core::phantom_proto::{
    caps, AudioCodec as ProtoAudioCodec, Codec, ControlCommand as ProtoControlCommand, Finger, Header,
    HostReport as ProtoHostReport, Message, HEADER_LEN, MAX_DATAGRAM, MAX_FINGERS, MAX_VIDEO_PAYLOAD, VIDEO_HEADER_LEN,
};
use phantom_core::{AudioSample, Client, Delivered, Host, KeyEvent, Negotiated, SessionConfig};
use phantom_p2p::{P2pEndpoint, P2pError, P2pLink};
use phantom_quic::{ClientEndpoint, HostEndpoint, QuicError, QuicLink};

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
    /// Valore casuale annunciato nell'apertura della sessione: 16 byte (se più corto si completa con zeri, se più
    /// lungo si tronca). Nella fase 5 sarà la base della chiave; per ora può restare vuoto.
    pub nonce: Vec<u8>,
    /// Dimensione massima del payload video per datagramma (byte, almeno 1 e al massimo `MAX_VIDEO_PAYLOAD`).
    /// Su QUIC va scelta in base a `max_datagram_size` tramite `video_payload_for_datagram`.
    pub frag_size: u16,
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
        nonce: d.nonce.to_vec(),
        frag_size: d.frag_size,
    }
}

impl From<SessionSettings> for SessionConfig {
    fn from(s: SessionSettings) -> Self {
        // il nonce del protocollo è fisso a 16 byte: qui si completa o si tronca, senza errori da gestire nell'app
        let mut nonce = [0u8; 16];
        let n = s.nonce.len().min(nonce.len());
        nonce[..n].copy_from_slice(&s.nonce[..n]);
        let frag_size = s.frag_size.clamp(1, MAX_VIDEO_PAYLOAD as u16);
        SessionConfig {
            conn_id: s.conn_id,
            codec: s.codec.into(),
            fec_group: s.fec_group,
            start_bitrate_bps: s.start_bitrate_bps,
            min_bitrate_bps: s.min_bitrate_bps,
            max_bitrate_bps: s.max_bitrate_bps,
            touch_interval_us: s.touch_interval_us,
            nonce,
            frag_size,
            ..SessionConfig::default()
        }
    }
}

/// Codec audio esposto ai binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AudioCodec {
    /// AAC-LC (raw Access Unit, `audio/mp4a-latm`).
    AacLc,
}

impl From<AudioCodec> for ProtoAudioCodec {
    fn from(c: AudioCodec) -> Self {
        match c {
            AudioCodec::AacLc => ProtoAudioCodec::AacLc,
        }
    }
}

impl From<ProtoAudioCodec> for AudioCodec {
    fn from(c: ProtoAudioCodec) -> Self {
        match c {
            ProtoAudioCodec::AacLc => AudioCodec::AacLc,
        }
    }
}

/// Pacchetto audio ricevuto dal client, pronto per il player.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AudioFrame {
    /// Numero progressivo del pacchetto audio.
    pub seq: u32,
    /// Codec audio.
    pub codec: AudioCodec,
    /// Frequenza di campionamento in Hz (es. 48000).
    pub sample_rate: u32,
    /// Numero di canali (1 = mono, 2 = stereo).
    pub channels: u8,
    /// `true` se contiene l'`AudioSpecificConfig` invece di un frame codificato.
    pub config: bool,
    /// Istante di cattura sull'orologio dell'host (µs).
    pub capture_ts_us: u64,
    /// Dati codificati.
    pub data: Vec<u8>,
}

impl From<AudioSample> for AudioFrame {
    fn from(a: AudioSample) -> Self {
        AudioFrame {
            seq: a.seq,
            codec: a.codec.into(),
            sample_rate: a.sample_rate,
            channels: a.channels,
            config: a.config,
            capture_ts_us: a.capture_ts_us,
            data: a.data,
        }
    }
}

/// Comando di controllo remoto inviato dal client all'host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RemoteControlCommand {
    /// Risveglia lo schermo dell'host se spento.
    WakeScreen,
    /// Spegne/oscura il display fisico dell'host mantenendo attivo lo streaming.
    ScreenOff,
    /// Riaccende il display fisico dell'host.
    ScreenOn,
    /// Tasto Indietro.
    Back,
    /// Tasto Home.
    Home,
    /// App recenti.
    Recents,
    /// Tendina delle notifiche.
    Notifications,
    /// Impostazioni rapide.
    QuickSettings,
    /// Tasto di accensione (blocca o risveglia).
    Power,
    /// Volume su.
    VolumeUp,
    /// Volume giù.
    VolumeDown,
    /// Screenshot.
    Screenshot,
}

impl From<RemoteControlCommand> for ProtoControlCommand {
    fn from(c: RemoteControlCommand) -> Self {
        match c {
            RemoteControlCommand::WakeScreen => ProtoControlCommand::WakeScreen,
            RemoteControlCommand::ScreenOff => ProtoControlCommand::ScreenOff,
            RemoteControlCommand::ScreenOn => ProtoControlCommand::ScreenOn,
            RemoteControlCommand::Back => ProtoControlCommand::Back,
            RemoteControlCommand::Home => ProtoControlCommand::Home,
            RemoteControlCommand::Recents => ProtoControlCommand::Recents,
            RemoteControlCommand::Notifications => ProtoControlCommand::Notifications,
            RemoteControlCommand::QuickSettings => ProtoControlCommand::QuickSettings,
            RemoteControlCommand::Power => ProtoControlCommand::Power,
            RemoteControlCommand::VolumeUp => ProtoControlCommand::VolumeUp,
            RemoteControlCommand::VolumeDown => ProtoControlCommand::VolumeDown,
            RemoteControlCommand::Screenshot => ProtoControlCommand::Screenshot,
        }
    }
}

impl From<ProtoControlCommand> for RemoteControlCommand {
    fn from(c: ProtoControlCommand) -> Self {
        match c {
            ProtoControlCommand::WakeScreen => RemoteControlCommand::WakeScreen,
            ProtoControlCommand::ScreenOff => RemoteControlCommand::ScreenOff,
            ProtoControlCommand::ScreenOn => RemoteControlCommand::ScreenOn,
            ProtoControlCommand::Back => RemoteControlCommand::Back,
            ProtoControlCommand::Home => RemoteControlCommand::Home,
            ProtoControlCommand::Recents => RemoteControlCommand::Recents,
            ProtoControlCommand::Notifications => RemoteControlCommand::Notifications,
            ProtoControlCommand::QuickSettings => RemoteControlCommand::QuickSettings,
            ProtoControlCommand::Power => RemoteControlCommand::Power,
            ProtoControlCommand::VolumeUp => RemoteControlCommand::VolumeUp,
            ProtoControlCommand::VolumeDown => RemoteControlCommand::VolumeDown,
            ProtoControlCommand::Screenshot => RemoteControlCommand::Screenshot,
        }
    }
}

fn parse_optional_auth_token(token: &[u8]) -> Option<[u8; 16]> {
    if token.is_empty() {
        return None;
    }
    let mut out = [0u8; 16];
    let n = token.len().min(16);
    out[..n].copy_from_slice(&token[..n]);
    Some(out)
}

/// Verifica un primo datagramma `Hello` del client contro il token di accoppiamento atteso prima di sostituire una
/// sessione attiva sull'host.
#[uniffi::export]
pub fn verify_client_hello(conn_id: u32, expected_auth_token: Vec<u8>, datagram: Vec<u8>) -> bool {
    let expected = parse_optional_auth_token(&expected_auth_token);
    phantom_core::verify_client_hello(conn_id, expected, &datagram)
}

/// Costruisce un datagramma `Close` con il motivo indicato (es. `2` = `UNAUTHORIZED`) per rifiutare client non accoppiati.
#[uniffi::export]
pub fn build_close_datagram(conn_id: u32, reason: u8) -> Vec<u8> {
    Message::Close { reason }.encode(Header { conn_id, seq: 0 }).unwrap_or_default()
}

/// Una funzione che host e client possono concordare all'apertura della sessione.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Capability {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    H265,
    /// Frammento di parità XOR per gruppo (recupero di un frammento perso senza ritrasmissione).
    XorParity,
    /// Ritrasmissione dei frammenti persi su richiesta.
    Nack,
    /// Richiesta di un nuovo keyframe.
    KeyframeRequest,
    /// Ritardo di coda misurato dal client e riportato all'host.
    QueueDelay,
    /// Audio AAC-LC in tempo reale (host → client).
    AudioAac,
    /// Appunti condivisi bidirezionali.
    Clipboard,
    /// Controllo risveglio/oscuramento display fisico dell'host.
    ScreenPower,
    /// Rapporto periodico dell'host per la telemetria.
    HostReport,
    /// Tasti di sistema come comandi di controllo.
    SystemKeys,
}

impl Capability {
    fn bit(self) -> u32 {
        match self {
            Capability::H264 => caps::H264,
            Capability::H265 => caps::H265,
            Capability::XorParity => caps::XOR_PARITY,
            Capability::Nack => caps::NACK,
            Capability::KeyframeRequest => caps::KEYFRAME_REQUEST,
            Capability::QueueDelay => caps::QUEUE_DELAY,
            Capability::AudioAac => caps::AUDIO_AAC,
            Capability::Clipboard => caps::CLIPBOARD,
            Capability::ScreenPower => caps::SCREEN_POWER,
            Capability::HostReport => caps::HOST_REPORT,
            Capability::SystemKeys => caps::SYSTEM_KEYS,
        }
    }
}

/// `true` se la sessione ha concordato quella funzione (vedi [`NegotiatedInfo::caps`]).
#[uniffi::export]
pub fn caps_contains(caps: u32, capability: Capability) -> bool {
    caps & capability.bit() != 0
}

/// Esito dell'apertura della sessione (`Hello`/`HelloAck`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NegotiatedInfo {
    /// Versione del protocollo scelta.
    pub proto: u8,
    /// Funzioni comuni ai due lati: usarle con [`caps_contains`].
    pub caps: u32,
    /// Valore casuale annunciato dall'altro lato (16 byte).
    pub peer_nonce: Vec<u8>,
}

impl From<Negotiated> for NegotiatedInfo {
    fn from(n: Negotiated) -> Self {
        NegotiatedInfo { proto: n.proto, caps: n.caps, peer_nonce: n.peer_nonce.to_vec() }
    }
}

/// Un evento di tastiera ricevuto dall'host. Premere e rilasciare sono due eventi distinti.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct KeyInput {
    /// Numero progressivo dell'evento (cresce di uno a ogni invio del client).
    pub seq: u32,
    /// Istante dell'evento sull'orologio del client (µs).
    pub client_time_us: u64,
    /// Codice HID del tasto.
    pub code: u16,
    /// `true` = premuto, `false` = rilasciato.
    pub down: bool,
}

impl From<KeyEvent> for KeyInput {
    fn from(k: KeyEvent) -> Self {
        KeyInput { seq: k.seq, client_time_us: k.t_client_us, code: k.code, down: k.down }
    }
}

/// Dimensione massima di un datagramma del protocollo (byte): il trasporto deve poterne spedire di questa dimensione.
#[uniffi::export]
pub fn max_datagram_size() -> u32 {
    MAX_DATAGRAM as u32
}

/// Payload video massimo (`SessionSettings::frag_size`) compatibile con un datagramma di `max_datagram_size` byte.
#[uniffi::export]
pub fn video_payload_for_datagram(max_datagram_size: u32) -> u16 {
    let overhead = (HEADER_LEN + VIDEO_HEADER_LEN) as u32;
    max_datagram_size.saturating_sub(overhead).clamp(1, MAX_VIDEO_PAYLOAD as u32) as u16
}

/// Nome server predefinito usato nel certificato autofirmato effimero di [`QuicHostEndpoint`].
#[uniffi::export]
pub fn default_quic_server_name() -> String {
    "localhost".to_owned()
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

/// Esito di [`PhantomHost::send_frame_via`].
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FrameSendReport {
    /// Datagrammi prodotti per il frame (dati e parità).
    pub datagrams: u32,
    /// Byte spediti (intestazioni Phantom comprese).
    pub bytes: u64,
    /// Datagrammi entrati in una coda di invio piena: per farli entrare QUIC ha scartato datagrammi più vecchi.
    pub queue_overflows: u32,
    /// Datagrammi rifiutati dal trasporto (connessione chiusa, troppo grandi, datagrammi non supportati).
    pub send_errors: u32,
    /// Il frame non è stato spedito: coda di invio troppo piena, o si aspetta il keyframe dopo uno svuotamento.
    pub skipped: bool,
    /// L'encoder deve produrre un keyframe adesso (`requestSyncFrame`).
    pub request_keyframe: bool,
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
    /// Eventi di tastiera ricevuti.
    pub key_events: u64,
    /// Riduzioni del bitrate decise dal controllo.
    pub bitrate_decreases: u64,
    /// Bitrate che l'encoder dovrebbe usare adesso (bit/s).
    pub target_bitrate_bps: u64,
    /// Keyframe spediti.
    pub keyframes_sent: u64,
}

/// Rapporto periodico dell'host per la telemetria (encoder, coda di invio, stato termico).
#[derive(Clone, Copy, Debug, Default, PartialEq, uniffi::Record)]
pub struct HostReportInfo {
    /// Frame al secondo prodotti dall'encoder.
    pub encoder_fps: f64,
    /// Bitrate reale in uscita dall'encoder (kbit/s).
    pub encoded_kbps: u32,
    /// Bitrate chiesto all'encoder dal controllo (kbit/s).
    pub target_kbps: u32,
    /// Larghezza codificata (pixel).
    pub width: u16,
    /// Altezza codificata (pixel).
    pub height: u16,
    /// Tempo medio dalla cattura all'uscita dall'encoder (µs).
    pub encode_latency_us: u32,
    /// Byte in coda di invio nel trasporto.
    pub send_queue_bytes: u32,
    /// Datagrammi scartati dal trasporto per coda piena (cumulativo).
    pub queue_overflows: u32,
    /// Invii rifiutati dal trasporto (cumulativo).
    pub send_errors: u32,
    /// Keyframe spediti (cumulativo; quando si spedisce lo mette l'host).
    pub keyframes: u32,
    /// Stato termico: 0 normale, 1 tiepido, 2 caldo, 3 critico.
    pub thermal_level: u8,
    /// Frame saltati alla sorgente perché la coda di invio era troppo piena (cumulativo).
    pub frames_skipped: u32,
}

impl From<HostReportInfo> for ProtoHostReport {
    fn from(r: HostReportInfo) -> Self {
        ProtoHostReport {
            encoder_fps_x10: (r.encoder_fps * 10.0).round().clamp(0.0, f64::from(u16::MAX)) as u16,
            encoded_kbps: r.encoded_kbps,
            target_kbps: r.target_kbps,
            width: r.width,
            height: r.height,
            encode_latency_us: r.encode_latency_us,
            send_queue_bytes: r.send_queue_bytes,
            queue_overflows: r.queue_overflows,
            send_errors: r.send_errors,
            keyframes: r.keyframes,
            thermal_level: r.thermal_level,
            frames_skipped: r.frames_skipped,
        }
    }
}

impl From<ProtoHostReport> for HostReportInfo {
    fn from(r: ProtoHostReport) -> Self {
        HostReportInfo {
            encoder_fps: f64::from(r.encoder_fps_x10) / 10.0,
            encoded_kbps: r.encoded_kbps,
            target_kbps: r.target_kbps,
            width: r.width,
            height: r.height,
            encode_latency_us: r.encode_latency_us,
            send_queue_bytes: r.send_queue_bytes,
            queue_overflows: r.queue_overflows,
            send_errors: r.send_errors,
            keyframes: r.keyframes,
            thermal_level: r.thermal_level,
            frames_skipped: r.frames_skipped,
        }
    }
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
    dropper: SourceDropper,
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
        Arc::new(PhantomHost {
            inner: Mutex::new(HostInner {
                host: Host::new(settings.into()),
                next_frame_id: 0,
                dropper: SourceDropper::new(),
            }),
        })
    }

    /// Crea l'host con token di accoppiamento opzionale (vuoto = disabilitato) e flag audio.
    #[uniffi::constructor]
    pub fn new_with_options(settings: SessionSettings, auth_token: Vec<u8>, audio_enabled: bool) -> Arc<Self> {
        let mut cfg: SessionConfig = settings.into();
        cfg.auth_token = parse_optional_auth_token(&auth_token);
        cfg.audio_enabled = audio_enabled;
        Arc::new(PhantomHost {
            inner: Mutex::new(HostInner { host: Host::new(cfg), next_frame_id: 0, dropper: SourceDropper::new() }),
        })
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

    /// Frammenta un frame codificato e lo spedisce direttamente su `connection`, in una sola chiamata dall'app.
    ///
    /// Equivale a [`PhantomHost::send_frame`] più un `send_datagram` per ogni frammento, ma senza far passare ogni
    /// pacchetto attraverso JNA/Swift (a 1080p sono migliaia di chiamate e copie al secondo).
    ///
    /// Se la coda di invio del trasporto contiene già più di ~250 ms di video il frame **non viene spedito**
    /// (`skipped`): la coda si svuota, poi si chiede un solo keyframe (`request_keyframe`) e si riparte da quello.
    /// Vedi `phantom_core::pacing`.
    pub fn send_frame_via(
        &self,
        connection: Arc<QuicConnection>,
        key: bool,
        capture_ts_us: u64,
        data: Vec<u8>,
    ) -> FrameSendReport {
        let mut report = FrameSendReport {
            datagrams: 0,
            bytes: 0,
            queue_overflows: 0,
            send_errors: 0,
            skipped: false,
            request_keyframe: false,
        };
        let queued = SEND_BUFFER_BYTES.saturating_sub(connection.send_buffer_space());
        {
            let mut g = lock(&self.inner);
            let target = g.host.target_bitrate_bps();
            let limit = g.dropper.limit_bytes(target, SEND_BUFFER_BYTES * 4 / 5);
            let drains = g.dropper.drains();
            let outcome = g.dropper.decide(queued, limit, key);
            if g.dropper.drains() > drains {
                // la coda è andata oltre la soglia: è congestione anche se il client non l'ha ancora vista
                g.host.on_send_stall(capture_ts_us);
            }
            if outcome.request_keyframe {
                g.host.request_keyframe();
            }
            if outcome.decision == FrameDecision::Skip {
                report.skipped = true;
                report.request_keyframe = g.host.wants_keyframe(capture_ts_us);
                return report;
            }
        }
        let datagrams = self.send_frame(key, capture_ts_us, data);
        for d in &datagrams {
            report.datagrams += 1;
            report.bytes += d.len() as u64;
            let outcome = connection.send_counted(d);
            if outcome.displaced_older {
                report.queue_overflows += 1;
            }
            if !outcome.sent {
                report.send_errors += 1;
            }
        }
        let mut g = lock(&self.inner);
        if key {
            g.dropper.on_keyframe_sent(report.bytes as usize);
        }
        if report.queue_overflows > 0 {
            // il frame precedente arriverà rotto: si svuota e si riparte da un keyframe (non uno per pacchetto)
            let drains = g.dropper.drains();
            g.dropper.on_queue_overflow();
            if g.dropper.drains() > drains {
                g.host.on_send_stall(capture_ts_us);
            }
        }
        report.request_keyframe = g.host.wants_keyframe(capture_ts_us);
        report
    }

    /// Frame saltati finora dallo scarto alla sorgente (coda di invio troppo piena).
    pub fn frames_skipped(&self) -> u64 {
        lock(&self.inner).dropper.skipped()
    }

    /// Chiede un keyframe dal lato host (vale la distanza minima tra keyframe): lo si legge poi da
    /// [`PhantomHost::wants_keyframe`].
    pub fn request_keyframe(&self) {
        lock(&self.inner).host.request_keyframe();
    }

    /// Bitrate che l'encoder dovrebbe usare adesso (bit/s): cambia con le condizioni della rete.
    pub fn target_bitrate_bps(&self) -> u64 {
        lock(&self.inner).host.target_bitrate_bps()
    }

    /// Limita il bitrate obiettivo durante la sessione (bit/s), senza mai superare il massimo iniziale delle
    /// impostazioni: l'host passa il bitrate utile del gradino di qualità più alto, così l'obiettivo non sale molto
    /// oltre quello che l'encoder può usare e le riduzioni per congestione hanno effetto subito.
    pub fn set_max_bitrate_bps(&self, max_bps: u64) {
        lock(&self.inner).host.set_max_bitrate_bps(max_bps);
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
            key_events: s.key_events,
            bitrate_decreases: s.bitrate_decreases,
            target_bitrate_bps: g.host.target_bitrate_bps(),
            keyframes_sent: s.keyframes_sent,
        }
    }

    /// Rapporto periodico per la telemetria del client (circa una volta al secondo): il datagramma da spedire, o
    /// `None` se la sessione non è aperta o il client non lo sa leggere. Il numero di keyframe lo mette l'host.
    pub fn host_report(&self, report: HostReportInfo) -> Option<Vec<u8>> {
        lock(&self.inner).host.host_report(report.into())
    }

    /// Esito dell'apertura della sessione (`Hello`/`HelloAck`), se il client l'ha già aperta: versione e funzioni
    /// comuni.
    pub fn negotiated(&self) -> Option<NegotiatedInfo> {
        lock(&self.inner).host.negotiated().map(NegotiatedInfo::from)
    }

    /// Il più vecchio evento di tastiera non ancora prelevato. Sono eventi, non stato: vanno prelevati tutti, in
    /// ordine.
    pub fn take_key(&self) -> Option<KeyInput> {
        lock(&self.inner).host.take_key().map(KeyInput::from)
    }

    /// Spedisce un pacchetto audio codificato al client; restituisce il datagramma pronto o `None` prima dell'apertura.
    pub fn send_audio(
        &self,
        codec: AudioCodec,
        sample_rate: u32,
        channels: u8,
        config: bool,
        capture_ts_us: u64,
        data: Vec<u8>,
    ) -> Option<Vec<u8>> {
        lock(&self.inner).host.send_audio(codec.into(), sample_rate, channels, config, capture_ts_us, &data)
    }

    /// Spedisce il testo degli appunti dell'host al client.
    pub fn send_clipboard(&self, text: String) -> Option<Vec<u8>> {
        lock(&self.inner).host.send_clipboard(&text)
    }

    /// Ultimo testo degli appunti ricevuto dal client, se presente.
    pub fn take_clipboard(&self) -> Option<String> {
        lock(&self.inner).host.take_clipboard()
    }

    /// Il più vecchio comando di controllo remoto ricevuto dal client (es. risveglio o schermo spento).
    pub fn take_control(&self) -> Option<RemoteControlCommand> {
        lock(&self.inner).host.take_control().map(RemoteControlCommand::from)
    }

    /// `true` se la sessione è stata chiusa (dal client o da [`PhantomHost::close`]): l'app deve fermare il trasporto.
    pub fn is_closed(&self) -> bool {
        lock(&self.inner).host.is_closed()
    }

    /// Motivo della chiusura, se c'è stata (0 = normale).
    pub fn close_reason(&self) -> Option<u8> {
        lock(&self.inner).host.close_reason()
    }

    /// Chiude la sessione: restituisce il datagramma `Close` da spedire al client, oppure `None` se era già chiusa.
    pub fn close(&self, reason: u8) -> Option<Vec<u8>> {
        lock(&self.inner).host.close(reason)
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

    /// Crea un client che annuncia il supporto sia ad H.264 sia ad HEVC (H.265) nell'`Hello`.
    #[uniffi::constructor]
    pub fn new_multi_codec(settings: SessionSettings) -> Arc<Self> {
        let mut cfg: SessionConfig = settings.into();
        cfg.extra_caps |= caps::H264 | caps::H265;
        Arc::new(PhantomClient { inner: Mutex::new(Client::new(cfg)) })
    }

    /// Crea un client multi-codec (H.264 + HEVC) con token di accoppiamento opzionale (vuoto = nessuno).
    #[uniffi::constructor]
    pub fn new_with_auth(settings: SessionSettings, auth_token: Vec<u8>) -> Arc<Self> {
        let mut cfg: SessionConfig = settings.into();
        cfg.extra_caps |= caps::H264 | caps::H265;
        cfg.auth_token = parse_optional_auth_token(&auth_token);
        Arc::new(PhantomClient { inner: Mutex::new(Client::new(cfg)) })
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

    /// I pacchetti audio arrivati dall'host dall'ultima chiamata, in ordine di arrivo.
    pub fn take_audio(&self) -> Vec<AudioFrame> {
        lock(&self.inner).take_audio().into_iter().map(AudioFrame::from).collect()
    }

    /// Spedisce il testo degli appunti del client all'host.
    pub fn send_clipboard(&self, text: String) -> Option<Vec<u8>> {
        lock(&self.inner).send_clipboard(&text)
    }

    /// Ultimo testo degli appunti ricevuto dall'host, se presente.
    pub fn take_clipboard(&self) -> Option<String> {
        lock(&self.inner).take_clipboard()
    }

    /// Spedisce un comando di controllo remoto all'host (risveglio / spegnimento display fisico).
    pub fn send_control(&self, command: RemoteControlCommand) -> Option<Vec<u8>> {
        lock(&self.inner).send_control(command.into())
    }

    /// Spedisce un evento di tastiera all'host: `code` è il codice HID, `down` dice premuto o rilasciato. Sono eventi,
    /// non stato: vanno mandati tutti, anche due di seguito con lo stesso codice. Restituisce il datagramma da spedire,
    /// oppure `None` se la sessione è chiusa.
    pub fn send_key(&self, now_us: u64, code: u16, down: bool) -> Option<Vec<u8>> {
        lock(&self.inner).key(now_us, code, down)
    }

    /// Esito dell'apertura della sessione (`Hello`/`HelloAck`), se l'host l'ha già confermata: versione e funzioni
    /// comuni.
    pub fn negotiated(&self) -> Option<NegotiatedInfo> {
        lock(&self.inner).negotiated().map(NegotiatedInfo::from)
    }

    /// `true` se la sessione è stata chiusa (dall'host o da [`PhantomClient::close`]): l'app deve fermare il trasporto.
    pub fn is_closed(&self) -> bool {
        lock(&self.inner).is_closed()
    }

    /// Motivo della chiusura, se c'è stata (0 = normale).
    pub fn close_reason(&self) -> Option<u8> {
        lock(&self.inner).close_reason()
    }

    /// Chiude la sessione: restituisce il datagramma `Close` da spedire all'host, oppure `None` se era già chiusa.
    pub fn close(&self, reason: u8) -> Option<Vec<u8>> {
        lock(&self.inner).close(reason)
    }

    /// Da chiamare quando il decoder dell'app scarta o non decodifica un frame già consegnato (coda del renderer
    /// piena, errore): i frame non-key successivi sarebbero corrotti fino al prossimo keyframe. Il ricevitore li
    /// scarta e restituisce la richiesta di keyframe da spedire (`None` se ce n'è già una in corso).
    pub fn request_keyframe(&self, now_us: u64) -> Option<Vec<u8>> {
        lock(&self.inner).request_keyframe(now_us)
    }

    /// Ultimo rapporto dell'host (encoder, coda di invio, stato termico), se ne è arrivato uno.
    pub fn host_report(&self) -> Option<HostReportInfo> {
        lock(&self.inner).host_report().map(HostReportInfo::from)
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

/// Un pacchetto audio pronto per il riproduttore.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct JitterAudioFrame {
    /// Posizione del primo campione sulla linea del tempo del riproduttore (campioni).
    pub pts_samples: i64,
    /// Dati codificati (un pacchetto AAC).
    pub data: Vec<u8>,
}

/// Risposta di [`AudioJitterBuffer::poll`].
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct JitterAudioPoll {
    /// Se presente: svuotare il riproduttore e portare il suo orologio a questa posizione (campioni) prima di
    /// accodare `frames`.
    pub resync_playhead_samples: Option<i64>,
    /// Pacchetti da accodare, in ordine.
    pub frames: Vec<JitterAudioFrame>,
    /// Velocità di riproduzione consigliata (1,0 ± 0,5%) per assorbire la deriva tra gli orologi senza salti.
    pub playback_rate: f64,
}

/// Contatori del buffer anti-jitter audio.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct JitterAudioCounters {
    /// Pacchetti ricevuti (doppioni compresi).
    pub received: u64,
    /// Doppioni scartati (copie di ridondanza).
    pub duplicates: u64,
    /// Pacchetti arrivati dopo che il loro posto era passato.
    pub late: u64,
    /// Posti saltati (silenzio di un pacchetto ciascuno).
    pub skipped: u64,
    /// Risincronizzazioni (avvio compreso).
    pub resyncs: u64,
    /// Ritardo di riproduzione attuale (ms).
    pub delay_ms: u32,
}

/// Buffer anti-jitter per l'audio (lato iPhone): mette ogni pacchetto al suo posto sulla linea del tempo (dall'istante
/// di cattura dell'host), scarta i doppioni, aspetta i pacchetti in ritardo finché c'è tempo e adatta il ritardo di
/// riproduzione alla rete. Vedi `phantom_core::jitter`.
#[derive(uniffi::Object)]
pub struct AudioJitterBuffer {
    inner: Mutex<AudioJitter>,
}

#[uniffi::export]
impl AudioJitterBuffer {
    /// Crea il buffer per AAC-LC a `sample_rate` Hz.
    #[uniffi::constructor]
    pub fn new(sample_rate: u32) -> Arc<Self> {
        Arc::new(AudioJitterBuffer { inner: Mutex::new(AudioJitter::new(JitterConfig::aac(sample_rate))) })
    }

    /// Un pacchetto audio (non di configurazione); `now_us` è l'orologio locale.
    pub fn push(&self, now_us: u64, capture_ts_us: u64, data: Vec<u8>) {
        lock(&self.inner).push(now_us, capture_ts_us, data);
    }

    /// Pacchetti da accodare. `playhead_samples` è la posizione attuale del riproduttore (`None` se non è partito).
    pub fn poll(&self, now_us: u64, playhead_samples: Option<i64>) -> JitterAudioPoll {
        let p = lock(&self.inner).poll(now_us, playhead_samples);
        JitterAudioPoll {
            resync_playhead_samples: p.resync_playhead,
            frames: p
                .frames
                .into_iter()
                .map(|f| JitterAudioFrame { pts_samples: f.pts_samples, data: f.data })
                .collect(),
            playback_rate: p.playback_rate,
        }
    }

    /// Dimentica tutto (nuova sessione o formato audio cambiato).
    pub fn reset(&self) {
        lock(&self.inner).reset();
    }

    /// Contatori.
    pub fn counters(&self) -> JitterAudioCounters {
        let s = lock(&self.inner).stats();
        JitterAudioCounters {
            received: s.received,
            duplicates: s.duplicates,
            late: s.late,
            skipped: s.skipped,
            resyncs: s.resyncs,
            delay_ms: (s.delay_us / 1_000) as u32,
        }
    }
}

/// Errore del trasporto QUIC esposto ai binding Swift e Kotlin.
#[derive(Debug, uniffi::Error)]
pub enum QuicTransportError {
    /// Indirizzo IP/porta non valido (atteso `ip:porta`, es. `127.0.0.1:47800`).
    InvalidAddress {
        /// Dettaglio dell'errore di parsing.
        detail: String,
    },
    /// Il certificato da fissare è vuoto.
    EmptyCertificate,
    /// Nessuna risposta entro il timeout richiesto.
    Timeout,
    /// L'endpoint è stato chiuso e non accetta più connessioni.
    EndpointClosed,
    /// L'altro lato non supporta i datagrammi QUIC (RFC 9221).
    DatagramsUnsupported,
    /// Il datagramma supera il massimo corrente della connessione.
    DatagramTooLarge {
        /// Lunghezza del datagramma che si è provato a spedire.
        len: u32,
        /// Massimo accettato in questo momento (`max_datagram_size`).
        max: u32,
    },
    /// Il datagramma ricevuto supera la capacità del buffer ed è stato scartato.
    BufferTooSmall {
        /// Lunghezza del datagramma arrivato.
        len: u32,
        /// Capacità massima passata a `recv_datagram`.
        capacity: u32,
    },
    /// Connessione chiusa o handshake TLS/QUIC fallito (incluso certificato diverso da quello fissato).
    ConnectionFailed {
        /// Descrizione dell'errore.
        detail: String,
    },
    /// Errore locale di I/O, TLS o configurazione dell'endpoint.
    Transport {
        /// Descrizione dell'errore.
        detail: String,
    },
}

impl fmt::Display for QuicTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QuicTransportError::InvalidAddress { detail } => write!(f, "indirizzo non valido: {detail}"),
            QuicTransportError::EmptyCertificate => write!(f, "il certificato da fissare è vuoto"),
            QuicTransportError::Timeout => write!(f, "nessuna risposta entro il timeout"),
            QuicTransportError::EndpointClosed => write!(f, "endpoint chiuso"),
            QuicTransportError::DatagramsUnsupported => write!(f, "datagrammi QUIC non supportati dall'altro lato"),
            QuicTransportError::DatagramTooLarge { len, max } => write!(f, "datagramma di {len} byte, massimo {max}"),
            QuicTransportError::BufferTooSmall { len, capacity } => {
                write!(f, "datagramma di {len} byte, buffer di {capacity}")
            }
            QuicTransportError::ConnectionFailed { detail } => write!(f, "{detail}"),
            QuicTransportError::Transport { detail } => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for QuicTransportError {}

impl From<QuicError> for QuicTransportError {
    fn from(error: QuicError) -> Self {
        match error {
            QuicError::EmptyCertificate => QuicTransportError::EmptyCertificate,
            QuicError::Timeout => QuicTransportError::Timeout,
            QuicError::EndpointClosed => QuicTransportError::EndpointClosed,
            QuicError::DatagramsUnsupported => QuicTransportError::DatagramsUnsupported,
            QuicError::DatagramTooLarge { len, max } => QuicTransportError::DatagramTooLarge {
                len: u32::try_from(len).unwrap_or(u32::MAX),
                max: u32::try_from(max).unwrap_or(u32::MAX),
            },
            QuicError::BufferTooSmall { len, capacity } => QuicTransportError::BufferTooSmall {
                len: u32::try_from(len).unwrap_or(u32::MAX),
                capacity: u32::try_from(capacity).unwrap_or(u32::MAX),
            },
            QuicError::Connection(_) => QuicTransportError::ConnectionFailed { detail: error.to_string() },
            QuicError::Io(_)
            | QuicError::Tls(_)
            | QuicError::TlsConfig(_)
            | QuicError::Certificate(_)
            | QuicError::Connect(_)
            | QuicError::SendDatagram(_) => QuicTransportError::Transport { detail: error.to_string() },
        }
    }
}

impl From<P2pError> for QuicTransportError {
    fn from(error: P2pError) -> Self {
        match error {
            P2pError::InvalidSecretKey => QuicTransportError::EmptyCertificate,
            P2pError::InvalidEndpointId(detail) => QuicTransportError::InvalidAddress { detail },
            P2pError::Timeout => QuicTransportError::Timeout,
            P2pError::EndpointClosed => QuicTransportError::EndpointClosed,
            P2pError::DatagramsUnsupported => QuicTransportError::DatagramsUnsupported,
            P2pError::DatagramTooLarge { len, max } => QuicTransportError::DatagramTooLarge {
                len: u32::try_from(len).unwrap_or(u32::MAX),
                max: u32::try_from(max).unwrap_or(u32::MAX),
            },
            P2pError::BufferTooSmall { len, capacity } => QuicTransportError::BufferTooSmall {
                len: u32::try_from(len).unwrap_or(u32::MAX),
                capacity: u32::try_from(capacity).unwrap_or(u32::MAX),
            },
            P2pError::Connection(detail) => QuicTransportError::ConnectionFailed { detail },
            P2pError::Transport(detail) => QuicTransportError::Transport { detail },
        }
    }
}

fn parse_socket_addr(addr: &str) -> Result<SocketAddr, QuicTransportError> {
    addr.parse::<SocketAddr>().map_err(|e| QuicTransportError::InvalidAddress { detail: e.to_string() })
}

/// Risolve sia indirizzi IP numerici (`ip:porta`) sia nomi DNS (`host.esempio:porta`, es. Tailscale/DDNS),
/// preferendo IPv4 per compatibilità con il bind client `0.0.0.0:0`.
fn resolve_socket_addr(addr: &str) -> Result<SocketAddr, QuicTransportError> {
    if let Ok(parsed) = addr.parse::<SocketAddr>() {
        return Ok(parsed);
    }
    let addrs = addr.to_socket_addrs().map_err(|e| QuicTransportError::InvalidAddress { detail: e.to_string() })?;
    let mut first = None;
    for candidate in addrs {
        if candidate.is_ipv4() {
            return Ok(candidate);
        }
        if first.is_none() {
            first = Some(candidate);
        }
    }
    first.ok_or_else(|| QuicTransportError::InvalidAddress { detail: format!("nessun indirizzo risolto per {addr}") })
}

/// Identità TLS persistente dell'host (certificato DER e chiave privata PKCS#8 DER).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HostIdentity {
    /// Certificato autofirmato X.509 in formato DER.
    pub cert_der: Vec<u8>,
    /// Chiave privata PKCS#8 in formato DER.
    pub key_der: Vec<u8>,
}

/// Genera una nuova identità TLS persistente da salvare sull'host e riusare con [`QuicHostEndpoint::bind_with_identity`].
#[uniffi::export]
pub fn generate_host_identity() -> Result<HostIdentity, QuicTransportError> {
    let id = phantom_quic::generate_host_identity()?;
    Ok(HostIdentity { cert_der: id.cert_der, key_der: id.key_der })
}

/// Genera una nuova chiave segreta Ed25519 (32 byte) per il trasporto P2P integrato (`iroh`).
#[uniffi::export]
pub fn generate_p2p_secret_key() -> Vec<u8> {
    phantom_p2p::generate_secret_key().to_vec()
}

/// Ricava l'`EndpointId` pubblico P2P dalla chiave segreta di 32 byte salvata sull'host.
#[uniffi::export]
pub fn p2p_endpoint_id_from_secret(secret_key: Vec<u8>) -> Result<String, QuicTransportError> {
    Ok(phantom_p2p::endpoint_id_from_secret_key(&secret_key)?)
}

/// Mantiene vivo l'endpoint (e il suo socket UDP) finché esiste almeno una [`QuicConnection`] aperta.
#[allow(dead_code)]
enum QuicEndpointKeepAlive {
    Host(Arc<HostEndpoint>),
    Client(Arc<ClientEndpoint>),
    P2p(Arc<P2pEndpoint>),
}

/// Backend effettivo di una [`QuicConnection`]: QUIC diretto (LAN) oppure P2P integrato (`iroh`, 5G/4G/ovunque).
enum ConnectionBackend {
    Direct(QuicLink),
    P2p(P2pLink),
}

/// Endpoint QUIC lato host (server): usa un certificato effimero o persistente e accetta una connessione.
#[derive(uniffi::Object)]
pub struct QuicHostEndpoint {
    inner: Arc<HostEndpoint>,
}

#[uniffi::export]
impl QuicHostEndpoint {
    /// Apre l'endpoint in ascolto su `addr` (es. `0.0.0.0:47800` o `127.0.0.1:0`).
    #[uniffi::constructor]
    pub fn bind(addr: String) -> Result<Arc<Self>, QuicTransportError> {
        let addr = parse_socket_addr(&addr)?;
        let inner = Arc::new(HostEndpoint::bind(addr)?);
        Ok(Arc::new(Self { inner }))
    }

    /// Apre l'endpoint in ascolto su `addr` usando un'identità persistente generata da [`generate_host_identity`].
    #[uniffi::constructor]
    pub fn bind_with_identity(
        addr: String,
        cert_der: Vec<u8>,
        key_der: Vec<u8>,
    ) -> Result<Arc<Self>, QuicTransportError> {
        let addr = parse_socket_addr(&addr)?;
        let inner = Arc::new(HostEndpoint::bind_with_identity(addr, cert_der, key_der)?);
        Ok(Arc::new(Self { inner }))
    }

    /// Indirizzo locale (`ip:porta`) su cui l'endpoint ascolta.
    pub fn local_addr(&self) -> Result<String, QuicTransportError> {
        Ok(self.inner.local_addr()?.to_string())
    }

    /// Certificato autofirmato DER da consegnare al client perché lo fissi durante la connessione.
    pub fn certificate_der(&self) -> Vec<u8> {
        self.inner.certificate_der().to_vec()
    }

    /// Aspetta una connessione in ingresso e completa il handshake entro `timeout_ms` millisecondi.
    pub fn accept(&self, timeout_ms: u64) -> Result<Arc<QuicConnection>, QuicTransportError> {
        let link = ConnectionBackend::Direct(self.inner.accept(Duration::from_millis(timeout_ms))?);
        let keep_alive = QuicEndpointKeepAlive::Host(Arc::clone(&self.inner));
        Ok(Arc::new(QuicConnection { link, _keep_alive: keep_alive }))
    }
}

/// Endpoint QUIC lato client: apre il socket locale e si collega all'host fissando il certificato.
#[derive(uniffi::Object)]
pub struct QuicClientEndpoint {
    inner: Arc<ClientEndpoint>,
}

#[uniffi::export]
impl QuicClientEndpoint {
    /// Apre l'endpoint locale su `addr` (es. `0.0.0.0:0` o `127.0.0.1:0`).
    #[uniffi::constructor]
    pub fn bind(addr: String) -> Result<Arc<Self>, QuicTransportError> {
        let addr = parse_socket_addr(&addr)?;
        let inner = Arc::new(ClientEndpoint::bind(addr)?);
        Ok(Arc::new(Self { inner }))
    }

    /// Si collega a `addr` (`ip:porta` oppure `hostname:porta`) verificando che il certificato sia esattamente
    /// `pinned_certificate_der`.
    pub fn connect(
        &self,
        addr: String,
        server_name: String,
        pinned_certificate_der: Vec<u8>,
        timeout_ms: u64,
    ) -> Result<Arc<QuicConnection>, QuicTransportError> {
        let addr = resolve_socket_addr(&addr)?;
        let timeout = Duration::from_millis(timeout_ms);
        let direct = self.inner.connect(addr, &server_name, &pinned_certificate_der, timeout)?;
        let link = ConnectionBackend::Direct(direct);
        let keep_alive = QuicEndpointKeepAlive::Client(Arc::clone(&self.inner));
        Ok(Arc::new(QuicConnection { link, _keep_alive: keep_alive }))
    }
}

/// Endpoint P2P integrato lato host (`iroh`): raggiungibile ovunque (5G/4G/Wi-Fi) tramite `EndpointId` senza VPN né router.
#[derive(uniffi::Object)]
pub struct P2pHostEndpoint {
    inner: Arc<P2pEndpoint>,
}

#[uniffi::export]
impl P2pHostEndpoint {
    /// Apre l'endpoint P2P host usando la chiave persistente di 32 byte generata da [`generate_p2p_secret_key`].
    #[uniffi::constructor]
    pub fn bind(secret_key: Vec<u8>, online_timeout_ms: u64) -> Result<Arc<Self>, QuicTransportError> {
        let inner = Arc::new(P2pEndpoint::bind_host(&secret_key, Duration::from_millis(online_timeout_ms))?);
        Ok(Arc::new(Self { inner }))
    }

    /// Identificatore pubblico (`EndpointId`) da includere nel QR di accoppiamento.
    pub fn endpoint_id(&self) -> String {
        self.inner.endpoint_id()
    }

    /// `true` se l'endpoint P2P ha raggiunto il relay online entro `online_timeout_ms`.
    pub fn is_online(&self) -> bool {
        self.inner.is_online()
    }

    /// Aspetta una connessione P2P in ingresso entro `timeout_ms` millisecondi.
    pub fn accept(&self, timeout_ms: u64) -> Result<Arc<QuicConnection>, QuicTransportError> {
        let link = ConnectionBackend::P2p(self.inner.accept(Duration::from_millis(timeout_ms))?);
        let keep_alive = QuicEndpointKeepAlive::P2p(Arc::clone(&self.inner));
        Ok(Arc::new(QuicConnection { link, _keep_alive: keep_alive }))
    }

    /// Notifica un cambio di rete sull'host (es. riconnessione Wi-Fi) per aggiornare i percorsi P2P.
    pub fn network_change(&self) {
        self.inner.network_change();
    }
}

/// Endpoint P2P integrato lato client (`iroh`): si collega all'host tramite `EndpointId` da 5G/4G o Wi-Fi.
#[derive(uniffi::Object)]
pub struct P2pClientEndpoint {
    inner: Arc<P2pEndpoint>,
}

#[uniffi::export]
impl P2pClientEndpoint {
    /// Apre un endpoint P2P client pronto per il collegamento remoto.
    #[uniffi::constructor]
    pub fn bind(online_timeout_ms: u64) -> Result<Arc<Self>, QuicTransportError> {
        let inner = Arc::new(P2pEndpoint::bind_client(Duration::from_millis(online_timeout_ms))?);
        Ok(Arc::new(Self { inner }))
    }

    /// Si collega all'host remoto identificato da `remote_endpoint_id` (hole punching diretto o relay cifrato E2E).
    pub fn connect(
        &self,
        remote_endpoint_id: String,
        timeout_ms: u64,
    ) -> Result<Arc<QuicConnection>, QuicTransportError> {
        let link = ConnectionBackend::P2p(self.inner.connect(&remote_endpoint_id, Duration::from_millis(timeout_ms))?);
        let keep_alive = QuicEndpointKeepAlive::P2p(Arc::clone(&self.inner));
        Ok(Arc::new(QuicConnection { link, _keep_alive: keep_alive }))
    }

    /// Notifica un cambio di rete sul client (es. passaggio Wi-Fi ↔ 5G/4G) per migrare i percorsi P2P.
    pub fn network_change(&self) {
        self.inner.network_change();
    }
}

/// Connessione QUIC aperta (diretta in LAN o P2P integrata `iroh`) che sposta datagrammi non affidabili (RFC 9221).
///
/// Invio e ricezione possono avvenire in contemporanea da thread diversi senza bloccarsi a vicenda.
#[derive(uniffi::Object)]
pub struct QuicConnection {
    // `link` deve stare prima di `_keep_alive`: Rust droppa i campi in ordine di dichiarazione, quindi la connessione
    // si chiude prima dell'endpoint/runtime che la ospita.
    link: ConnectionBackend,
    _keep_alive: QuicEndpointKeepAlive,
}

#[uniffi::export]
impl QuicConnection {
    /// Spedisce subito un datagramma, se sta entro [`QuicConnection::max_datagram_size`].
    pub fn send_datagram(&self, datagram: Vec<u8>) -> Result<(), QuicTransportError> {
        match &self.link {
            ConnectionBackend::Direct(link) => Ok(link.send_datagram(&datagram)?),
            ConnectionBackend::P2p(link) => Ok(link.send_datagram(&datagram)?),
        }
    }

    /// Aspetta un datagramma fino a `timeout_ms` millisecondi, con buffer massimo di `max_bytes`.
    pub fn recv_datagram(&self, max_bytes: u32, timeout_ms: u64) -> Result<Vec<u8>, QuicTransportError> {
        let capacity = (max_bytes as usize).clamp(1, 65_536);
        let mut buf = vec![0u8; capacity];
        let timeout = Duration::from_millis(timeout_ms);
        let n = match &self.link {
            ConnectionBackend::Direct(link) => link.recv_datagram(&mut buf, timeout)?,
            ConnectionBackend::P2p(link) => link.recv_datagram(&mut buf, timeout)?,
        };
        buf.truncate(n);
        Ok(buf)
    }

    /// Massimo datagramma accettato in questo momento (dipende dalla path MTU); `None` se non supportato.
    pub fn max_datagram_size(&self) -> Option<u32> {
        let max = match &self.link {
            ConnectionBackend::Direct(link) => link.max_datagram_size(),
            ConnectionBackend::P2p(link) => link.max_datagram_size(),
        };
        max.and_then(|n| u32::try_from(n).ok())
    }

    /// Chiude la connessione con un codice e un motivo applicativo.
    pub fn close(&self, code: u32, reason: Vec<u8>) {
        match &self.link {
            ConnectionBackend::Direct(link) => link.close(code, &reason),
            ConnectionBackend::P2p(link) => link.close(code, &reason),
        }
    }

    /// `true` se la connessione QUIC è già chiusa.
    pub fn is_closed(&self) -> bool {
        match &self.link {
            ConnectionBackend::Direct(link) => link.is_closed(),
            ConnectionBackend::P2p(link) => link.is_closed(),
        }
    }

    /// Byte di datagrammi in coda di invio, non ancora usciti (telemetria: se resta alto il collegamento non regge
    /// il bitrate).
    pub fn queued_send_bytes(&self) -> u64 {
        let space = self.send_buffer_space();
        SEND_BUFFER_BYTES.saturating_sub(space) as u64
    }
}

/// Coda dei datagrammi in uscita impostata nei due trasporti.
const SEND_BUFFER_BYTES: usize = phantom_quic::DATAGRAM_SEND_BUFFER;

/// Esito di un invio contato.
struct SendOutcome {
    sent: bool,
    displaced_older: bool,
}

impl QuicConnection {
    fn send_buffer_space(&self) -> usize {
        match &self.link {
            ConnectionBackend::Direct(link) => link.datagram_send_buffer_space(),
            ConnectionBackend::P2p(link) => link.datagram_send_buffer_space(),
        }
    }

    /// Spedisce un datagramma dicendo se è stato accettato e se, per farlo entrare, QUIC ha dovuto scartare
    /// datagrammi più vecchi ancora in coda.
    fn send_counted(&self, datagram: &[u8]) -> SendOutcome {
        let displaced_older = datagram.len() > self.send_buffer_space();
        let sent = match &self.link {
            ConnectionBackend::Direct(link) => link.send_datagram(datagram).is_ok(),
            ConnectionBackend::P2p(link) => link.send_datagram(datagram).is_ok(),
        };
        SendOutcome { sent, displaced_older: displaced_older && sent }
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

    /// Apre la sessione con il primo `Hello` e l'ack dell'host.
    fn open(host: &PhantomHost, client: &PhantomClient) {
        let out: Vec<(To, Vec<u8>)> = client.tick(0).into_iter().map(|d| (To::Host, d)).collect();
        pump(host, client, 0, out);
        assert!(client.negotiated().is_some(), "sessione aperta");
        assert!(host.negotiated().is_some(), "anche l'host vede la sessione aperta");
    }

    #[test]
    fn input_waits_for_the_session() {
        let host = PhantomHost::new(default_session_settings());
        let client = PhantomClient::new(default_session_settings());
        let fingers = vec![TouchPoint { id: 3, x: 1000, y: 2000, pressure: 90 }];
        assert!(client.touch_tick(0, fingers.clone()).is_none(), "niente tocco prima dell'apertura");
        assert!(client.send_key(0, 4, true).is_none(), "niente tastiera prima dell'apertura");
        assert!(host.send_frame(true, 0, vec![1, 2, 3]).is_empty(), "niente video prima dell'apertura");
        assert!(!host.wants_keyframe(0), "niente keyframe prima dell'apertura");

        open(&host, &client);
        assert!(client.touch_tick(1, fingers).is_some(), "dopo l'apertura il tocco parte");
        assert!(client.send_key(1, 4, true).is_some(), "dopo l'apertura la tastiera parte");
        assert!(!host.send_frame(true, 1, vec![1, 2, 3]).is_empty(), "dopo l'apertura il video parte");
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
        open(&host, &client);
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

    #[test]
    fn negotiation_and_caps_are_visible() {
        let host = PhantomHost::new(default_session_settings());
        let client = PhantomClient::new(default_session_settings());
        let out: Vec<(To, Vec<u8>)> = client.tick(0).into_iter().map(|d| (To::Host, d)).collect();
        pump(&host, &client, 0, out);

        let n = client.negotiated().expect("il client ha negoziato");
        assert_eq!(n.proto, 0);
        assert_eq!(n.peer_nonce.len(), 16);
        assert!(caps_contains(n.caps, Capability::H265), "HEVC concordato");
        assert!(caps_contains(n.caps, Capability::XorParity), "parità concordata");
        assert!(caps_contains(n.caps, Capability::Nack));
        assert!(!caps_contains(n.caps, Capability::H264), "un solo codec si concorda");
        let h = host.negotiated().expect("anche l'host vede la sessione aperta");
        assert_eq!(h.caps, n.caps);
        assert_eq!(h.proto, n.proto);
    }

    #[test]
    fn nonce_crosses_the_bindings_and_short_ones_are_padded() {
        let settings = SessionSettings { nonce: vec![7; 16], ..default_session_settings() };
        let host = PhantomHost::new(settings.clone());
        let client = PhantomClient::new(SessionSettings { nonce: vec![3; 3], ..settings });
        let out: Vec<(To, Vec<u8>)> = client.tick(0).into_iter().map(|d| (To::Host, d)).collect();
        pump(&host, &client, 0, out);

        let mut expected = vec![3u8; 3];
        expected.resize(16, 0);
        assert_eq!(host.negotiated().expect("negoziata").peer_nonce, expected, "il nonce corto si completa con zeri");
        assert_eq!(client.negotiated().expect("negoziata").peer_nonce, vec![7; 16]);
    }

    #[test]
    fn keyboard_events_pass_through_the_bindings() {
        let host = PhantomHost::new(default_session_settings());
        let client = PhantomClient::new(default_session_settings());
        open(&host, &client);
        for (code, down) in [(4u16, true), (4, false)] {
            let d = client.send_key(1_000, code, down).expect("datagramma di tastiera");
            host.on_datagram(0, d);
        }
        let press = host.take_key().expect("pressione");
        assert_eq!((press.seq, press.code, press.down, press.client_time_us), (0, 4, true, 1_000));
        let release = host.take_key().expect("rilascio");
        assert_eq!((release.seq, release.down), (1, false));
        assert!(host.take_key().is_none(), "la coda si svuota");
        assert_eq!(host.counters().key_events, 2);
    }

    #[test]
    fn close_is_terminal_on_both_sides() {
        let host = PhantomHost::new(default_session_settings());
        let client = PhantomClient::new(default_session_settings());
        let d = client.close(0).expect("datagramma di chiusura");
        assert!(client.is_closed());
        assert!(host.on_datagram(0, d).is_empty(), "alla chiusura non si risponde");
        assert!(host.is_closed());
        assert_eq!(host.close_reason(), Some(0));
        assert_eq!(client.close_reason(), Some(0));
        assert!(client.tick(1_000).is_empty(), "il client chiuso non manda niente");
        assert!(client.send_key(1_000, 4, true).is_none());
        assert!(host.send_frame(true, 0, vec![1, 2, 3]).is_empty());
    }

    #[test]
    fn video_payload_and_frag_size_follow_mtu_limits() {
        assert_eq!(video_payload_for_datagram(1162), 1122);
        assert_eq!(video_payload_for_datagram(10), 1);
        assert_eq!(video_payload_for_datagram(2000), MAX_VIDEO_PAYLOAD as u16);

        let d = default_session_settings();
        assert_eq!(d.frag_size, MAX_VIDEO_PAYLOAD as u16);
        let clamped_low: SessionConfig = SessionSettings { frag_size: 0, ..d.clone() }.into();
        assert_eq!(clamped_low.frag_size, 1);
        let clamped_high: SessionConfig = SessionSettings { frag_size: u16::MAX, ..d }.into();
        assert_eq!(clamped_high.frag_size, MAX_VIDEO_PAYLOAD as u16);
    }

    #[test]
    fn host_report_travels_from_host_to_client() {
        let host = PhantomHost::new(default_session_settings());
        let client = PhantomClient::new(default_session_settings());
        let hello = client.tick(0).remove(0);
        let ack = host.on_datagram(0, hello).remove(0);
        client.on_datagram(0, ack);
        assert!(caps_contains(client.negotiated().expect("aperta").caps, Capability::HostReport));
        let info = HostReportInfo {
            encoder_fps: 59.94,
            encoded_kbps: 12_000,
            width: 1080,
            height: 2400,
            ..HostReportInfo::default()
        };
        let d = host.host_report(info).expect("rapporto");
        client.on_datagram(1, d);
        let got = client.host_report().expect("ricevuto");
        assert!((got.encoder_fps - 59.9).abs() < 1e-9, "decimi di fps: {}", got.encoder_fps);
        assert_eq!((got.encoded_kbps, got.width, got.height, got.keyframes), (12_000, 1080, 2400, 0));
    }

    #[test]
    fn audio_jitter_buffer_orders_and_deduplicates() {
        let j = AudioJitterBuffer::new(48_000);
        let frame_us = |i: u64| 5_000_000 + i * 1_000_000 * 1024 / 48_000;
        for i in [0u64, 2, 1, 1, 2] {
            j.push(0, frame_us(i), vec![i as u8]);
        }
        let poll = j.poll(0, None);
        assert!(poll.resync_playhead_samples.is_some(), "avvio");
        let pts: Vec<i64> = poll.frames.iter().map(|f| f.pts_samples).collect();
        assert_eq!(pts, vec![0, 1024, 2048]);
        assert_eq!(poll.frames[1].data, vec![1]);
        let c = j.counters();
        assert_eq!((c.received, c.duplicates, c.resyncs, c.delay_ms), (5, 2, 1, 80));
        j.reset();
        assert!(j.poll(0, None).frames.is_empty());
    }

    #[test]
    fn quic_bindings_reject_bad_inputs_and_mismatched_certificates() {
        assert!(matches!(
            QuicHostEndpoint::bind("not-an-addr".to_owned()),
            Err(QuicTransportError::InvalidAddress { .. })
        ));
        let client = QuicClientEndpoint::bind("127.0.0.1:0".to_owned()).expect("bind client");
        assert!(matches!(
            client.connect("127.0.0.1:1".to_owned(), default_quic_server_name(), vec![], 100),
            Err(QuicTransportError::EmptyCertificate)
        ));

        let host = QuicHostEndpoint::bind("127.0.0.1:0".to_owned()).expect("bind host");
        let addr = host.local_addr().expect("addr");
        let mut wrong_cert = host.certificate_der();
        for b in &mut wrong_cert {
            *b ^= 0xff;
        }
        let acceptor = std::thread::spawn(move || {
            let _ = host.accept(2_000);
        });
        let err = client.connect(addr, default_quic_server_name(), wrong_cert, 2_000);
        assert!(matches!(err, Err(QuicTransportError::ConnectionFailed { .. })));
        acceptor.join().expect("thread accept");
    }

    #[test]
    fn quic_bindings_open_exchange_and_close_over_loopback() {
        let host_ep = QuicHostEndpoint::bind("127.0.0.1:0".to_owned()).expect("bind host");
        let addr = host_ep.local_addr().expect("addr");
        let cert = host_ep.certificate_der();
        assert!(!cert.is_empty());

        let (tx, rx) = std::sync::mpsc::channel();
        let host_thread = std::thread::spawn(move || {
            let link = host_ep.accept(5_000).expect("accept");
            let max_dgram = link.max_datagram_size().expect("datagrammi abilitati");
            let mut cfg = default_session_settings();
            cfg.frag_size = video_payload_for_datagram(max_dgram);
            let host = PhantomHost::new(cfg);

            let start = std::time::Instant::now();
            let mut frame_sent = false;
            while start.elapsed() < Duration::from_secs(2) {
                match link.recv_datagram(2048, 5) {
                    Ok(d) => {
                        let now = start.elapsed().as_micros() as u64;
                        for reply in host.on_datagram(now, d) {
                            let _ = link.send_datagram(reply);
                        }
                    }
                    Err(QuicTransportError::Timeout) => {}
                    Err(_) => break,
                }
                if host.negotiated().is_some() && !frame_sent {
                    let now = start.elapsed().as_micros() as u64;
                    let key = host.wants_keyframe(now);
                    // invio in una sola chiamata (percorso usato dall'app Android)
                    let report = host.send_frame_via(link.clone(), key, now, vec![0xCD; 8_000]);
                    assert!(report.datagrams >= 8, "8 000 byte in frammenti da ~1 100 più la parità");
                    assert_eq!(report.send_errors, 0);
                    assert_eq!(report.queue_overflows, 0, "coda vuota: nessun datagramma scartato");
                    assert!(report.bytes > 8_000);
                    frame_sent = true;
                }
                if host.is_closed() {
                    break;
                }
            }
            tx.send((host.negotiated().is_some(), host.is_closed(), host.take_key())).expect("report host");
        });

        let client_ep = QuicClientEndpoint::bind("127.0.0.1:0".to_owned()).expect("bind client");
        let link = client_ep.connect(addr, default_quic_server_name(), cert, 5_000).expect("connect");
        let client = PhantomClient::new(default_session_settings());
        let start = std::time::Instant::now();
        let mut frames = Vec::new();
        let mut key_sent = false;
        while start.elapsed() < Duration::from_secs(2) {
            let now = start.elapsed().as_micros() as u64;
            for d in client.tick(now) {
                link.send_datagram(d).expect("tick client");
            }
            if client.negotiated().is_some() && !key_sent {
                if let Some(d) = client.send_key(now, 30, true) {
                    link.send_datagram(d).expect("key client");
                    key_sent = true;
                }
            }
            match link.recv_datagram(2048, 5) {
                Ok(d) => {
                    for reply in client.on_datagram(now, d) {
                        let _ = link.send_datagram(reply);
                    }
                }
                Err(QuicTransportError::Timeout) => {}
                Err(_) => break,
            }
            frames.extend(client.take_frames());
            if !frames.is_empty() && key_sent {
                break;
            }
        }
        if let Some(close_dgram) = client.close(0) {
            let _ = link.send_datagram(close_dgram);
        }
        std::thread::sleep(Duration::from_millis(50));
        link.close(0, Vec::new());

        let (host_negotiated, host_closed, taken_key) = rx.recv_timeout(Duration::from_secs(3)).expect("esito host");
        host_thread.join().expect("thread host");
        assert!(host_negotiated);
        assert!(host_closed);
        assert_eq!(taken_key.map(|k| (k.code, k.down)), Some((30, true)));
        assert!(!frames.is_empty(), "almeno un frame ricevuto su QUIC");
        assert_eq!(frames[0].data.len(), 8_000);
    }

    #[test]
    fn quic_connection_survives_dropping_endpoints() {
        let host_ep = QuicHostEndpoint::bind("127.0.0.1:0".to_owned()).expect("bind host");
        let addr = host_ep.local_addr().expect("addr");
        let cert = host_ep.certificate_der();
        let (tx, rx) = std::sync::mpsc::channel();

        let accept_thread = std::thread::spawn(move || {
            let link = host_ep.accept(5_000).expect("accept");
            drop(host_ep);
            tx.send(link).expect("send host link");
        });

        let client_ep = QuicClientEndpoint::bind("127.0.0.1:0".to_owned()).expect("bind client");
        let client_link = client_ep.connect(addr, default_quic_server_name(), cert, 5_000).expect("connect");
        drop(client_ep);

        let host_link = rx.recv_timeout(Duration::from_secs(5)).expect("host link");
        accept_thread.join().expect("thread accept");

        // Diamo tempo all'eventuale frame di chiusura (se l'endpoint fosse caduto) di arrivare.
        std::thread::sleep(Duration::from_millis(50));
        assert!(!host_link.is_closed(), "il link host deve restare aperto dopo drop(host_ep)");
        assert!(!client_link.is_closed(), "il link client deve restare aperto dopo drop(client_ep)");

        client_link.send_datagram(b"ping".to_vec()).expect("send client->host");
        assert_eq!(host_link.recv_datagram(1024, 1_000).expect("recv host"), b"ping");

        host_link.send_datagram(b"pong".to_vec()).expect("send host->client");
        assert_eq!(client_link.recv_datagram(1024, 1_000).expect("recv client"), b"pong");

        client_link.close(0, Vec::new());
        host_link.close(0, Vec::new());
    }

    #[test]
    fn persistent_identity_dns_hostname_and_multi_codec_work() {
        let identity = generate_host_identity().expect("genera identità");
        let host_ep = QuicHostEndpoint::bind_with_identity(
            "127.0.0.1:0".to_owned(),
            identity.cert_der.clone(),
            identity.key_der.clone(),
        )
        .expect("bind identità");
        let port = host_ep.local_addr().expect("addr").rsplit(':').next().expect("porta").to_owned();
        let (tx, rx) = std::sync::mpsc::channel();
        let accept_thread = std::thread::spawn(move || {
            let link = host_ep.accept(5_000).expect("accept");
            let _ = tx.send(link);
        });

        let client_ep = QuicClientEndpoint::bind("0.0.0.0:0".to_owned()).expect("bind client");
        let client_link = client_ep
            .connect(format!("localhost:{port}"), default_quic_server_name(), identity.cert_der, 5_000)
            .expect("connect tramite hostname DNS");
        let host_link = rx.recv_timeout(Duration::from_secs(5)).expect("host link");
        accept_thread.join().expect("join accept");
        client_link.close(0, Vec::new());
        host_link.close(0, Vec::new());

        let host = PhantomHost::new(SessionSettings { codec: VideoCodec::H264, ..default_session_settings() });
        let client = PhantomClient::new_multi_codec(default_session_settings());
        open(&host, &client);
        let n = client.negotiated().expect("negoziata");
        assert!(caps_contains(n.caps, Capability::H264), "H.264 negoziato con client multi-codec");
        assert!(!caps_contains(n.caps, Capability::H265));

        let p2p_secret = generate_p2p_secret_key();
        assert_eq!(p2p_secret.len(), 32);
        let p2p_id = p2p_endpoint_id_from_secret(p2p_secret).expect("endpoint id P2P");
        assert!(!p2p_id.is_empty());
        assert!(matches!(p2p_endpoint_id_from_secret(vec![1, 2, 3]), Err(QuicTransportError::EmptyCertificate)));

        let token = vec![0x77u8; 16];
        let auth_host = PhantomHost::new_with_options(default_session_settings(), token.clone(), true);
        let bad_client = PhantomClient::new_with_auth(default_session_settings(), vec![0x11; 16]);
        let bad_hello = bad_client.tick(0).remove(0);
        assert!(!verify_client_hello(default_session_settings().conn_id, token.clone(), bad_hello.clone()));
        let bad_reply = auth_host.on_datagram(0, bad_hello);
        assert_eq!(bad_reply.len(), 1);
        assert!(auth_host.negotiated().is_none());

        let ok_client = PhantomClient::new_with_auth(default_session_settings(), token.clone());
        let ok_hello = ok_client.tick(0).remove(0);
        assert!(verify_client_hello(default_session_settings().conn_id, token, ok_hello.clone()));
        let ack = auth_host.on_datagram(10, ok_hello).remove(0);
        ok_client.on_datagram(20, ack);
        assert!(ok_client.negotiated().is_some());
        let n_caps = ok_client.negotiated().expect("negoziata").caps;
        assert!(caps_contains(n_caps, Capability::AudioAac));
        assert!(caps_contains(n_caps, Capability::Clipboard));
        assert!(caps_contains(n_caps, Capability::ScreenPower));

        let audio_dgram = auth_host.send_audio(AudioCodec::AacLc, 48_000, 2, true, 100, vec![0x11, 0x90]).expect("asc");
        ok_client.on_datagram(30, audio_dgram);
        let rx_audio = ok_client.take_audio();
        assert_eq!(rx_audio.len(), 1);
        assert!(rx_audio[0].config);

        let clip = ok_client.send_clipboard("hello".to_owned()).expect("clip");
        auth_host.on_datagram(40, clip);
        assert_eq!(auth_host.take_clipboard().as_deref(), Some("hello"));

        let ctrl = ok_client.send_control(RemoteControlCommand::ScreenOff).expect("ctrl");
        auth_host.on_datagram(50, ctrl);
        assert_eq!(auth_host.take_control(), Some(RemoteControlCommand::ScreenOff));
        assert!(!build_close_datagram(default_session_settings().conn_id, 2).is_empty());
    }
}
