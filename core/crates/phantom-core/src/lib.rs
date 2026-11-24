//! Phantom core: colla tra protocollo, orologi e video, più i due «attori» (host e client) usati nelle prove
//! end-to-end sul simulatore di rete e su UDP reale.
//!
//! In produzione host e client gireranno su Android e iPhone (binding UniFFI, fase 1 seconda parte); qui sono
//! scritti come macchine a stati pure — nessun thread, nessun sonno, tempo passato dal chiamante — così le stesse
//! prove girano veloci e identiche su ogni piattaforma.

pub mod fec;
pub mod jitter;
pub mod pacing;
pub mod rate;

use std::collections::VecDeque;

use phantom_clock::{ClockSync, Sample};
use phantom_proto::{
    audio_flags, caps, close_reason, decode, AudioCodec, AudioPacket, Codec, ControlCommand, Finger, Header,
    HostReport, Message, Role, Stats, MAX_AUDIO_PAYLOAD, MAX_CLIPBOARD_BYTES, MAX_HOST_LOG_CHUNK, MAX_HOST_LOG_PARTS,
    QUEUE_DELAY_UNKNOWN, VERSION, VIDEO_PAUSE_LEASE_US,
};
use phantom_video::{AssembledFrame, EncodedFrame, Output, ReceiverConfig, VideoReceiver, VideoSender};
use rate::{RateConfig, RateController};

pub use phantom_clock;
pub use phantom_netsim;
pub use phantom_proto;
pub use phantom_video;

/// Ping senza risposta che il client ricorda: oltre, i più vecchi si dimenticano (le loro risposte, se mai arrivano, si
/// ignorano).
const MAX_PENDING_PINGS: usize = 64;

/// Eventi di tastiera in attesa di essere prelevati dall'host: oltre, si butta il più vecchio (meglio perdere una
/// pressione vecchia che quella appena fatta).
const MAX_PENDING_KEYS: usize = 32;

/// Comandi di controllo remoto in attesa di essere prelevati dall'host.
const MAX_PENDING_CONTROLS: usize = 16;

/// Pacchetti audio in attesa sul client: coda corta per evitare accumulo di latenza.
const MAX_PENDING_AUDIO: usize = 64;

fn ct_eq_16(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut diff = 0u8;
    for i in 0..16 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn is_newer_seq(prev: Option<u32>, seq: u32) -> bool {
    prev.is_none_or(|s| seq.wrapping_sub(s) < (1 << 31) && seq != s)
}

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Verifica un primo datagramma `Hello` del client prima di sostituire una sessione attiva sull'host.
pub fn verify_client_hello(conn_id: u32, expected_auth_token: Option<[u8; 16]>, bytes: &[u8]) -> bool {
    let Ok((hdr, msg)) = decode(bytes) else { return false };
    if hdr.conn_id != conn_id {
        return false;
    }
    let Message::Hello { role: Role::Client, proto_min, proto_max, auth_token, .. } = msg else {
        return false;
    };
    #[allow(clippy::unnecessary_min_or_max)]
    let proto = proto_max.min(VERSION);
    if proto < proto_min {
        return false;
    }
    match expected_auth_token {
        None => true,
        Some(expected) => auth_token.as_ref().is_some_and(|tok| ct_eq_16(tok, &expected)),
    }
}

/// Pacchetto audio ricevuto dal client, pronto per il decoder audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSample {
    /// Numero progressivo del pacchetto audio.
    pub seq: u32,
    /// Codec audio.
    pub codec: AudioCodec,
    /// Frequenza di campionamento in Hz.
    pub sample_rate: u32,
    /// Numero di canali (1 = mono, 2 = stereo).
    pub channels: u8,
    /// `true` se contiene `AudioSpecificConfig` invece di un frame audio.
    pub config: bool,
    /// Istante di cattura sull'orologio dell'host (µs).
    pub capture_ts_us: u64,
    /// Dati codificati.
    pub data: Vec<u8>,
}

/// Impostazioni di una sessione.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Identificatore di sessione.
    pub conn_id: u32,
    /// Dimensione dei frammenti video dati.
    pub frag_size: u16,
    /// Frammenti dati per gruppo di parità con la rete pulita (0 = nessuna).
    pub fec_group: u16,
    /// `true` (predefinito): il gruppo di parità si stringe con le perdite riportate dal client ([`fec::AdaptiveFec`]).
    pub adaptive_fec: bool,
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
    /// Valore casuale annunciato nell'apertura della sessione (16 byte). Nella fase 5 sarà la base della chiave:
    /// l'app lo riempie con byte casuali, qui resta zero finché non c'è cifratura.
    pub nonce: [u8; 16],
    /// Intervallo tra due `Hello` finché l'host non risponde (µs): l'ack può perdersi, quindi si ritenta.
    pub hello_retry_us: u64,
    /// Bit di capacità aggiuntivi annunciati dal client in `Hello` (es. secondo codec decodificabile).
    pub extra_caps: u32,
    /// Token di accoppiamento condiviso (16 byte): se impostato sull'host, i client senza token uguale vengono rifiutati.
    pub auth_token: Option<[u8; 16]>,
    /// `true` se l'host cattura e trasmette audio AAC-LC in tempo reale.
    pub audio_enabled: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            conn_id: 0x5048_0001,
            frag_size: phantom_proto::MAX_VIDEO_PAYLOAD as u16,
            fec_group: 10,
            adaptive_fec: true,
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
            nonce: [0; 16],
            hello_retry_us: 250_000,
            extra_caps: 0,
            auth_token: None,
            audio_enabled: false,
        }
    }
}

/// Esito dell'apertura della sessione (`Hello`/`HelloAck`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Negotiated {
    /// Versione del protocollo scelta.
    pub proto: u8,
    /// Funzioni comuni ai due lati (intersezione di quelle annunciate).
    pub caps: u32,
    /// Valore casuale annunciato dall'altro lato.
    pub peer_nonce: [u8; 16],
}

/// Un evento di tastiera ricevuto dall'host. A differenza del tocco è un evento, non uno stato: premere e rilasciare
/// sono due eventi e vanno consegnati entrambi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    /// Numero progressivo del client.
    pub seq: u32,
    /// Orologio del client al momento dell'evento (µs).
    pub t_client_us: u64,
    /// Codice HID del tasto.
    pub code: u16,
    /// `true` = premuto, `false` = rilasciato.
    pub down: bool,
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
    /// Eventi di tastiera accettati.
    pub key_events: u64,
    /// Riduzioni del bitrate decise dal controllo.
    pub bitrate_decreases: u64,
    /// Keyframe spediti.
    pub keyframes_sent: u64,
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
    /// Eventi di tastiera ricevuti e non ancora prelevati, dal più vecchio.
    keys: VecDeque<KeyEvent>,
    last_key_seq: Option<u32>,
    audio_seq: u32,
    clip_seq: u32,
    last_clip_seq: Option<u32>,
    pending_clipboard: Option<String>,
    last_ctrl_seq: Option<u32>,
    controls: VecDeque<ControlCommand>,
    log_id: u32,
    /// Il client è in background: niente video fino a questo istante (orologio dell'host), salvo rinnovo.
    video_paused_until_us: Option<u64>,
    stats: HostStats,
    rate: RateController,
    /// Dimensione del gruppo di parità secondo le perdite riportate dal client.
    fec: fec::AdaptiveFec,
    /// Esito dell'apertura della sessione, se il client ha già mandato `Hello`.
    negotiated: Option<Negotiated>,
    /// Motivo della chiusura, se la sessione è stata chiusa (stato terminale).
    closed: Option<u8>,
}

impl Host {
    /// Crea l'host.
    pub fn new(cfg: SessionConfig) -> Self {
        let video = VideoSender::new(cfg.conn_id, cfg.frag_size, cfg.fec_group, cfg.cache_frames);
        let cfg_fec_group = if cfg.adaptive_fec { cfg.fec_group } else { 0 };
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
            keys: VecDeque::new(),
            last_key_seq: None,
            audio_seq: 0,
            clip_seq: 0,
            last_clip_seq: None,
            pending_clipboard: None,
            last_ctrl_seq: None,
            controls: VecDeque::new(),
            log_id: 0,
            video_paused_until_us: None,
            stats: HostStats::default(),
            rate,
            fec: fec::AdaptiveFec::new(cfg_fec_group),
            negotiated: None,
            closed: None,
        }
    }

    /// Esito dell'apertura della sessione (`Hello`/`HelloAck`), se il client l'ha già aperta.
    pub fn negotiated(&self) -> Option<Negotiated> {
        self.negotiated
    }

    /// `true` se la sessione è stata chiusa: da quel momento l'host non spedisce né risponde più nulla.
    pub fn is_closed(&self) -> bool {
        self.closed.is_some()
    }

    /// Motivo della chiusura, se c'è stata (0 = [`close_reason::NORMAL`]).
    pub fn close_reason(&self) -> Option<u8> {
        self.closed
    }

    /// Chiude la sessione: restituisce il datagramma `Close` da spedire, oppure `None` se era già chiusa.
    pub fn close(&mut self, reason: u8) -> Option<Vec<u8>> {
        if self.closed.is_some() {
            return None;
        }
        let out = self.encode(&Message::Close { reason });
        self.closed = Some(reason);
        out
    }

    /// Le funzioni che l'host dichiara nell'apertura.
    fn caps(&self) -> u32 {
        let parity = if self.cfg.fec_group > 0 { caps::XOR_PARITY } else { 0 };
        let audio = if self.cfg.audio_enabled { caps::AUDIO_AAC } else { 0 };
        self.cfg.codec.caps_bit()
            | caps::NACK
            | caps::KEYFRAME_REQUEST
            | caps::QUEUE_DELAY
            | parity
            | audio
            | caps::CLIPBOARD
            | caps::SCREEN_POWER
            | caps::HOST_REPORT
            | caps::SYSTEM_KEYS
            | caps::HOST_LOG
            | caps::VIDEO_PAUSE
    }

    fn encode(&mut self, m: &Message<'_>) -> Option<Vec<u8>> {
        let h = Header { conn_id: self.cfg.conn_id, seq: self.seq };
        self.seq = self.seq.wrapping_add(1);
        m.encode(h).ok()
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

    /// La coda di invio dell'host si è dovuta svuotare (vedi [`rate::RateController::on_send_stall`]).
    pub fn on_send_stall(&mut self, now_us: u64) {
        self.rate.on_send_stall(now_us);
    }

    /// Massimo del bitrate durante la sessione (vedi [`rate::RateController::set_max_bps`]).
    pub fn set_max_bitrate_bps(&mut self, max_bps: u64) {
        self.rate.set_max_bps(max_bps);
    }

    /// Limita la dimensione dei frammenti video a `max_payload` byte (mai oltre quella delle impostazioni).
    ///
    /// Va chiamata prima di ogni frame con il massimo che il trasporto accetta **adesso**: la MTU di QUIC cambia durante
    /// la sessione (la ricerca della MTU la alza; dopo una raffica di perdite quinn sospetta un «black hole» e la
    /// riporta a 1200 per un minuto). Con una dimensione fissa presa all'apertura, in quel minuto ogni frammento
    /// risultava troppo grande e il video si fermava del tutto. Il ricevitore legge la dimensione da ogni pacchetto,
    /// quindi può cambiare da un frame all'altro.
    pub fn limit_frag_size(&mut self, max_payload: u16) {
        self.video.frag_size = self.cfg.frag_size.min(max_payload).max(1);
    }

    /// Frammenti dati per ogni frammento di parità adesso (0 = parità spenta): si stringe con le perdite, vedi
    /// [`fec::AdaptiveFec`].
    pub fn fec_group(&self) -> u16 {
        self.video.fec_group
    }

    /// Dimensione attuale dei frammenti video (byte).
    pub fn frag_size(&self) -> u16 {
        self.video.frag_size
    }

    /// Il controllo del bitrate (sola lettura: fase, giudizio, riduzioni).
    pub fn rate(&self) -> &RateController {
        &self.rate
    }

    /// `true` se l'encoder deve produrre un keyframe adesso: il client ne ha chiesto uno (o la sessione è appena
    /// iniziata) e dall'ultimo sono passati almeno `min_keyframe_gap_us`; se il collegamento è saturo, un keyframe
    /// dopo l'altro peggiora la saturazione. `now_us` è l'orologio dell'host. Prima dell'apertura della sessione non
    /// si chiede nulla: il primo frame parte solo dopo un `Hello` valido.
    pub fn wants_keyframe(&self, now_us: u64) -> bool {
        self.negotiated.is_some()
            && self.video_paused_until_us.is_none()
            && self.keyframe_wanted
            && self.last_key_ts_us.is_none_or(|t| now_us.saturating_sub(t) >= self.cfg.min_keyframe_gap_us)
    }

    /// Prepara il rapporto periodico per la telemetria del client (circa una volta al secondo). Il conteggio dei
    /// keyframe lo mette l'host; il resto lo misura l'app. `None` se la sessione non è aperta o il client non sa
    /// leggere il rapporto (app più vecchia).
    pub fn host_report(&mut self, report: HostReport) -> Option<Vec<u8>> {
        let n = self.negotiated?;
        if self.closed.is_some() || n.caps & caps::HOST_REPORT == 0 {
            return None;
        }
        let report = HostReport { keyframes: self.stats.keyframes_sent.min(u64::from(u32::MAX)) as u32, ..report };
        self.encode(&Message::HostReport(report))
    }

    /// Chiede un keyframe dal lato host, senza aspettare il client: per esempio quando il trasporto ha dovuto scartare
    /// datagrammi già in coda (il client vedrebbe un frame rotto solo dopo il timeout). Vale il distanziamento minimo
    /// di [`Host::wants_keyframe`]; prima dell'apertura della sessione non fa nulla.
    pub fn request_keyframe(&mut self) {
        if self.negotiated.is_some() && self.closed.is_none() {
            self.keyframe_wanted = true;
        }
    }

    /// Spedisce un frame già codificato: restituisce i datagrammi da mandare al client. `key` dice cosa ha prodotto
    /// l'encoder; chi chiama deve chiedere un keyframe all'encoder quando [`Host::wants_keyframe`] è `true`.
    /// `capture_ts_us` è l'istante di cattura sull'orologio dell'host. Prima dell'apertura della sessione non esce
    /// nulla (e il frame non entra nelle statistiche).
    pub fn send_frame(&mut self, frame_id: u32, key: bool, capture_ts_us: u64, data: &[u8]) -> Vec<Vec<u8>> {
        if self.closed.is_some() || self.negotiated.is_none() || self.video_paused(capture_ts_us) {
            return Vec::new();
        }
        if key {
            self.keyframe_wanted = false;
            self.last_key_ts_us = Some(capture_ts_us);
            self.stats.keyframes_sent += 1;
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

    /// Il più vecchio evento di tastiera non ancora prelevato. Sono eventi, non stato: vanno consegnati tutti, in
    /// ordine, e la coda è limitata (i più vecchi si perdono se l'app non li preleva).
    pub fn take_key(&mut self) -> Option<KeyEvent> {
        self.keys.pop_front()
    }

    /// Spedisce un pacchetto audio codificato al client. Prima dell'apertura della sessione non esce nulla.
    pub fn send_audio(
        &mut self,
        codec: AudioCodec,
        sample_rate: u32,
        channels: u8,
        config: bool,
        capture_ts_us: u64,
        data: &[u8],
    ) -> Option<Vec<u8>> {
        if self.closed.is_some() || self.negotiated.is_none() || data.len() > MAX_AUDIO_PAYLOAD {
            return None;
        }
        let seq = self.audio_seq;
        self.audio_seq = self.audio_seq.wrapping_add(1);
        let flags = if config { audio_flags::CONFIG } else { 0 };
        let pkt = AudioPacket { audio_seq: seq, codec, channels, flags, sample_rate, capture_ts_us, payload: data };
        self.encode(&Message::Audio(pkt))
    }

    /// Spedisce il testo degli appunti dell'host al client (troncato in UTF-8 a [`MAX_CLIPBOARD_BYTES`]).
    pub fn send_clipboard(&mut self, text: &str) -> Option<Vec<u8>> {
        if self.closed.is_some() || self.negotiated.is_none() {
            return None;
        }
        let clipped = truncate_utf8(text, MAX_CLIPBOARD_BYTES);
        let seq = self.clip_seq;
        self.clip_seq = self.clip_seq.wrapping_add(1);
        self.encode(&Message::Clipboard { clip_seq: seq, text: clipped })
    }

    /// Ultimo testo degli appunti ricevuto dal client, se ce n'è uno nuovo.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.pending_clipboard.take()
    }

    /// Spedisce il log dell'host al client che l'ha chiesto (`ControlCommand::SendLog`): restituisce i datagrammi, a
    /// pezzi da [`MAX_HOST_LOG_CHUNK`] byte. Se il testo è più lungo di quanto ci sta si tiene la parte finale (la più
    /// recente). Ogni pezzo esce due volte: sono datagrammi non affidabili e una diagnosi a metà serve poco. Vuoto se la
    /// sessione non è aperta o il client non sa leggere il log.
    pub fn send_log(&mut self, text: &str) -> Vec<Vec<u8>> {
        let Some(n) = self.negotiated else { return Vec::new() };
        if self.closed.is_some() || n.caps & caps::HOST_LOG == 0 {
            return Vec::new();
        }
        let max = MAX_HOST_LOG_CHUNK * usize::from(MAX_HOST_LOG_PARTS);
        let bytes = text.as_bytes();
        let bytes = if bytes.len() > max { &bytes[bytes.len() - max..] } else { bytes };
        let chunks: Vec<&[u8]> =
            if bytes.is_empty() { vec![&[][..]] } else { bytes.chunks(MAX_HOST_LOG_CHUNK).collect() };
        let parts = chunks.len() as u16;
        let log_id = self.log_id;
        self.log_id = self.log_id.wrapping_add(1);
        let mut out = Vec::with_capacity(chunks.len() * 2);
        for _ in 0..2 {
            for (i, chunk) in chunks.iter().enumerate() {
                if let Some(d) = self.encode(&Message::HostLog { log_id, part: i as u16, parts, bytes: chunk }) {
                    out.push(d);
                }
            }
        }
        out
    }

    /// `true` se il client è in background e il video non va mandato (`ControlCommand::PauseVideo`). `now_us` è
    /// l'orologio dell'host: se la pausa non è stata rinnovata in tempo finisce qui, e il prossimo frame sarà un
    /// keyframe. Chi chiama può sospendere l'encoder finché resta `true`.
    pub fn video_paused(&mut self, now_us: u64) -> bool {
        match self.video_paused_until_us {
            Some(until) if now_us >= until => {
                self.resume_video();
                false
            }
            Some(_) => true,
            None => false,
        }
    }

    fn resume_video(&mut self) {
        if self.video_paused_until_us.take().is_some() {
            // il client ha buttato il decoder: si riparte da un keyframe nuovo, senza aspettare il distanziamento
            self.keyframe_wanted = true;
            self.last_key_ts_us = None;
        }
    }

    /// Il più vecchio comando di controllo remoto ricevuto dal client (es. risveglio o schermo spento).
    pub fn take_control(&mut self) -> Option<ControlCommand> {
        self.controls.pop_front()
    }

    /// `true` se il token del client non corrisponde a quello configurato sull'host.
    fn auth_rejected(&self, auth_token: Option<[u8; 16]>) -> bool {
        let Some(expected) = self.cfg.auth_token else { return false };
        !auth_token.as_ref().is_some_and(|tok| ct_eq_16(tok, &expected))
    }

    /// Elabora un datagramma ricevuto dal client; `now_us` è l'istante di arrivo sull'orologio dell'host. Restituisce
    /// le risposte da rimandare al client.
    pub fn on_datagram(&mut self, now_us: u64, bytes: &[u8]) -> Vec<Vec<u8>> {
        if self.closed.is_some() {
            return Vec::new();
        }
        let Ok((hdr, msg)) = decode(bytes) else { return Vec::new() };
        if hdr.conn_id != self.cfg.conn_id {
            return Vec::new();
        }
        let mut out = Vec::new();
        match msg {
            Message::Hello { role, proto_min, proto_max, caps: peer_caps, nonce, auth_token } => {
                // apre solo il client; chi si presenta con il ruolo sbagliato si ignora
                if role == Role::Client {
                    // la versione più alta che entrambi parliamo; oggi `VERSION` è 0, per questo clippy vede il minimo
                    // come costante e inutile, ma resta la regola per quando le versioni saranno più di una
                    #[allow(clippy::unnecessary_min_or_max)]
                    let proto = proto_max.min(VERSION);
                    if proto < proto_min {
                        if let Some(b) = self.encode(&Message::Close { reason: close_reason::BAD_PROTOCOL }) {
                            out.push(b);
                        }
                    } else if self.auth_rejected(auth_token) {
                        if let Some(b) = self.encode(&Message::Close { reason: close_reason::UNAUTHORIZED }) {
                            out.push(b);
                        }
                    } else {
                        let common = peer_caps & self.caps();
                        self.negotiated = Some(Negotiated { proto, caps: common, peer_nonce: nonce });
                        // un `Hello` ripetuto riceve di nuovo l'ack: quello precedente può essersi perso
                        let ack = Message::HelloAck { proto, caps: common, nonce: self.cfg.nonce };
                        if let Some(b) = self.encode(&ack) {
                            out.push(b);
                        }
                    }
                }
            }
            Message::Close { reason } => {
                // la chiusura vale in qualunque momento, anche prima dell'apertura
                self.closed = Some(reason);
            }
            // cancello: finché la sessione non è aperta si accettano solo `Hello` e `Close`; ping, video, tocco,
            // tastiera e statistiche di chi non si è presentato si ignorano senza toccare lo stato
            _ if self.negotiated.is_none() => {}
            Message::Ping { id, t_client_us } => {
                let reply = Message::Pong { id, t_client_us, t_host_rx_us: now_us, t_host_tx_us: now_us };
                if let Some(b) = self.encode(&reply) {
                    out.push(b);
                }
            }
            Message::Nack { frame_id, missing } => out.extend(self.video.on_nack(now_us, frame_id, &missing)),
            Message::Stats(s) => {
                let load = match self.rate.on_stats(now_us, &s) {
                    rate::Verdict::Fine => fec::Load::Clear,
                    rate::Verdict::Congested | rate::Verdict::HeavyLoss => fec::Load::Loaded,
                    rate::Verdict::Hold => fec::Load::Unknown,
                };
                if let Some(g) = self.fec.on_stats(now_us, s.frames_ok, s.frames_lost, s.frames_fec, load) {
                    self.video.fec_group = g;
                }
            }
            Message::KeyframeRequest { .. } => {
                self.keyframe_wanted = true;
                self.stats.keyframe_requests += 1;
            }
            Message::Touch { state_seq, t_client_us, fingers } => {
                // stato completo: conta solo il più recente; uno arrivato in ritardo e fuori ordine si ignora
                if is_newer_seq(self.last_touch_seq, state_seq) {
                    self.last_touch_seq = Some(state_seq);
                    self.last_touch = Some((state_seq, t_client_us, fingers));
                    self.touch_fresh = true;
                    self.stats.touch_states += 1;
                }
            }
            Message::Key { key_seq, t_client_us, code, down } => {
                // eventi, non stato: si accodano tutti; duplicati e riordini si scartano come per il tocco
                if is_newer_seq(self.last_key_seq, key_seq) {
                    self.last_key_seq = Some(key_seq);
                    if self.keys.len() == MAX_PENDING_KEYS {
                        self.keys.pop_front();
                    }
                    self.keys.push_back(KeyEvent { seq: key_seq, t_client_us, code, down });
                    self.stats.key_events += 1;
                }
            }
            Message::Clipboard { clip_seq, text } if is_newer_seq(self.last_clip_seq, clip_seq) => {
                self.last_clip_seq = Some(clip_seq);
                self.pending_clipboard = Some(text.to_owned());
            }
            Message::Control { ctrl_seq, command } if is_newer_seq(self.last_ctrl_seq, ctrl_seq) => {
                self.last_ctrl_seq = Some(ctrl_seq);
                // la pausa del video la gestisce il core: l'app la legge con `video_paused`
                match command {
                    ControlCommand::PauseVideo => {
                        self.video_paused_until_us = Some(now_us.saturating_add(VIDEO_PAUSE_LEASE_US));
                        return out;
                    }
                    ControlCommand::ResumeVideo => {
                        self.resume_video();
                        return out;
                    }
                    _ => {}
                }
                if self.controls.len() == MAX_PENDING_CONTROLS {
                    self.controls.pop_front();
                }
                self.controls.push_back(command);
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
    key_seq: u32,
    clip_seq: u32,
    last_clip_seq: Option<u32>,
    pending_clipboard: Option<String>,
    ctrl_seq: u32,
    audio: VecDeque<AudioSample>,
    /// Esito dell'apertura della sessione, se l'host ha già risposto.
    negotiated: Option<Negotiated>,
    /// Prossimo `Hello` da mandare (µs), finché l'host non risponde.
    next_hello_us: u64,
    /// Motivo della chiusura, se la sessione è stata chiusa (stato terminale).
    closed: Option<u8>,
    /// Frame consegnati al decoder.
    pub delivered: Vec<Delivered>,
    /// Ultimo rapporto dell'host (telemetria).
    host_report: Option<HostReport>,
    host_log: Option<HostLogAssembly>,
}

/// Log dell'host in arrivo a pezzi.
#[derive(Debug)]
struct HostLogAssembly {
    log_id: u32,
    parts: Vec<Option<Vec<u8>>>,
}

/// Log dell'host ricomposto dal client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostLogText {
    /// Testo (i pezzi mancanti sono segnati).
    pub text: String,
    /// Pezzi arrivati.
    pub received: u16,
    /// Pezzi totali.
    pub total: u16,
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
            key_seq: 0,
            clip_seq: 0,
            last_clip_seq: None,
            pending_clipboard: None,
            ctrl_seq: 0,
            audio: VecDeque::new(),
            negotiated: None,
            next_hello_us: 0,
            closed: None,
            delivered: Vec::new(),
            host_report: None,
            host_log: None,
        }
    }

    /// Esito dell'apertura della sessione (`Hello`/`HelloAck`), se l'host l'ha già confermata.
    pub fn negotiated(&self) -> Option<Negotiated> {
        self.negotiated
    }

    /// `true` se la sessione è stata chiusa: da quel momento il client non spedisce più nulla.
    pub fn is_closed(&self) -> bool {
        self.closed.is_some()
    }

    /// Motivo della chiusura, se c'è stata (0 = [`close_reason::NORMAL`]).
    pub fn close_reason(&self) -> Option<u8> {
        self.closed
    }

    /// Chiude la sessione: restituisce il datagramma `Close` da spedire, oppure `None` se era già chiusa.
    pub fn close(&mut self, reason: u8) -> Option<Vec<u8>> {
        if self.closed.is_some() {
            return None;
        }
        let out = self.encode(&Message::Close { reason });
        self.closed = Some(reason);
        out
    }

    /// Le funzioni che il client dichiara nell'apertura.
    fn caps(&self) -> u32 {
        // la parità la manda l'host, ma il ricevitore del client la sa ricostruire: per questo la annuncia
        self.cfg.codec.caps_bit()
            | self.cfg.extra_caps
            | caps::NACK
            | caps::KEYFRAME_REQUEST
            | caps::QUEUE_DELAY
            | caps::XOR_PARITY
            | caps::AUDIO_AAC
            | caps::CLIPBOARD
            | caps::SCREEN_POWER
            | caps::HOST_REPORT
            | caps::SYSTEM_KEYS
            | caps::HOST_LOG
            | caps::VIDEO_PAUSE
    }

    /// Frame consegnati dal ricevitore dall'ultima chiamata, nell'ordine di arrivo: chi usa il client per un'app li
    /// preleva a ogni giro (nelle prove restano in `delivered` per l'analisi finale).
    pub fn take_frames(&mut self) -> Vec<Delivered> {
        std::mem::take(&mut self.delivered)
    }

    /// Pacchetti audio arrivati dall'host dall'ultima chiamata, nell'ordine di ricezione.
    pub fn take_audio(&mut self) -> Vec<AudioSample> {
        self.audio.drain(..).collect()
    }

    /// Ultimo testo degli appunti ricevuto dall'host, se presente.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.pending_clipboard.take()
    }

    /// Spedisce il testo degli appunti del client all'host (troncato in UTF-8 a [`MAX_CLIPBOARD_BYTES`]).
    pub fn send_clipboard(&mut self, text: &str) -> Option<Vec<u8>> {
        if self.closed.is_some() || self.negotiated.is_none() {
            return None;
        }
        let clipped = truncate_utf8(text, MAX_CLIPBOARD_BYTES);
        let seq = self.clip_seq;
        self.clip_seq = self.clip_seq.wrapping_add(1);
        self.encode(&Message::Clipboard { clip_seq: seq, text: clipped })
    }

    /// Il log dell'host chiesto con `ControlCommand::SendLog`: completo appena sono arrivati tutti i pezzi, oppure,
    /// con `partial`, quello che c'è (i pezzi mancanti sono segnati nel testo). Una volta preso non torna più.
    pub fn take_host_log(&mut self, partial: bool) -> Option<HostLogText> {
        let a = self.host_log.as_ref()?;
        let total = a.parts.len() as u16;
        let received = a.parts.iter().filter(|p| p.is_some()).count() as u16;
        if received == 0 || (received < total && !partial) {
            return None;
        }
        let mut bytes = Vec::new();
        for (i, p) in a.parts.iter().enumerate() {
            match p {
                Some(b) => bytes.extend_from_slice(b),
                None => {
                    bytes.extend_from_slice(format!("\n[… pezzo {} di {} non arrivato …]\n", i + 1, total).as_bytes())
                }
            }
        }
        self.host_log = None;
        Some(HostLogText { text: String::from_utf8_lossy(&bytes).into_owned(), received, total })
    }

    /// Spedisce un comando di controllo remoto all'host (es. risveglio o spegnimento display fisico).
    pub fn send_control(&mut self, command: ControlCommand) -> Option<Vec<u8>> {
        if self.closed.is_some() || self.negotiated.is_none() {
            return None;
        }
        let seq = self.ctrl_seq;
        self.ctrl_seq = self.ctrl_seq.wrapping_add(1);
        self.encode(&Message::Control { ctrl_seq: seq, command })
    }

    /// Chiede all'host di fermare (`true`) o far ripartire (`false`) il video, per esempio quando l'app va in
    /// background. La pausa vale [`VIDEO_PAUSE_LEASE_US`]: va ripetuta (circa ogni secondo) finché serve. `None` se la
    /// sessione non è aperta o l'host non conosce la funzione (`caps::VIDEO_PAUSE`).
    pub fn video_pause(&mut self, paused: bool) -> Option<Vec<u8>> {
        if self.negotiated?.caps & caps::VIDEO_PAUSE == 0 {
            return None;
        }
        let command = if paused { ControlCommand::PauseVideo } else { ControlCommand::ResumeVideo };
        self.send_control(command)
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
        if self.closed.is_some() {
            return Vec::new();
        }
        let Ok((hdr, msg)) = decode(bytes) else { return Vec::new() };
        if hdr.conn_id != self.cfg.conn_id {
            return Vec::new();
        }
        let mut send = Vec::new();
        match msg {
            Message::HelloAck { proto, caps: peer_caps, nonce } => {
                // vale il primo ack di una versione che parliamo: le copie che arrivano dopo non cambiano nulla
                if self.negotiated.is_none() && proto == VERSION {
                    self.negotiated = Some(Negotiated { proto, caps: peer_caps, peer_nonce: nonce });
                }
            }
            Message::Close { reason } => {
                // la chiusura vale in qualunque momento, anche prima dell'apertura
                self.closed = Some(reason);
            }
            // cancello: finché l'host non ha confermato l'apertura si accettano solo `HelloAck` e `Close`; video e
            // risposte ai ping di chi non ha ancora aperto la sessione si ignorano
            _ if self.negotiated.is_none() => {}
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
            Message::Audio(a) => {
                if self.audio.len() == MAX_PENDING_AUDIO {
                    self.audio.pop_front();
                }
                self.audio.push_back(AudioSample {
                    seq: a.audio_seq,
                    codec: a.codec,
                    sample_rate: a.sample_rate,
                    channels: a.channels,
                    config: a.flags & audio_flags::CONFIG != 0,
                    capture_ts_us: a.capture_ts_us,
                    data: a.payload.to_vec(),
                });
            }
            Message::Clipboard { clip_seq, text } if is_newer_seq(self.last_clip_seq, clip_seq) => {
                self.last_clip_seq = Some(clip_seq);
                self.pending_clipboard = Some(text.to_owned());
            }
            Message::HostReport(r) => {
                self.host_report = Some(r);
            }
            Message::HostLog { log_id, part, parts, bytes } => {
                let fresh = self.host_log.as_ref().is_none_or(|a| a.log_id != log_id);
                if fresh {
                    self.host_log = Some(HostLogAssembly { log_id, parts: vec![None; usize::from(parts)] });
                }
                if let Some(a) = self.host_log.as_mut() {
                    if let Some(slot) = a.parts.get_mut(usize::from(part)) {
                        *slot = Some(bytes.to_vec());
                    }
                }
            }
            _ => {}
        }
        send
    }

    /// Ultimo rapporto dell'host (encoder, coda di invio, stato termico), se ne è arrivato uno.
    pub fn host_report(&self) -> Option<HostReport> {
        self.host_report
    }

    /// Da chiamare di frequente: ping di sincronia, timer del video. Restituisce i datagrammi da spedire.
    pub fn tick(&mut self, now_us: u64) -> Vec<Vec<u8>> {
        if self.closed.is_some() {
            return Vec::new();
        }
        let mut send = Vec::new();
        if self.negotiated.is_none() && now_us >= self.next_hello_us {
            // l'ack può perdersi: si ritenta finché non arriva (la ripetizione è innocua per l'host)
            self.next_hello_us = now_us + self.cfg.hello_retry_us;
            let hello = Message::Hello {
                role: Role::Client,
                proto_min: VERSION,
                proto_max: VERSION,
                caps: self.caps(),
                nonce: self.cfg.nonce,
                auth_token: self.cfg.auth_token,
            };
            if let Some(b) = self.encode(&hello) {
                send.push(b);
            }
        }
        if self.negotiated.is_none() {
            // cancello: finché l'host non conferma l'apertura si manda solo `Hello` (niente ping, timer video o
            // statistiche). Il primo ping, il primo tocco e le statistiche partono al giro successivo all'ack.
            return send;
        }
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

    /// Spedisce lo stato del tocco se è ora (240 Hz di default): `fingers` è lo stato corrente completo. Prima
    /// dell'apertura della sessione non parte nulla.
    pub fn touch_tick(&mut self, now_us: u64, fingers: &[Finger]) -> Option<Vec<u8>> {
        if self.closed.is_some() || self.negotiated.is_none() || now_us < self.next_touch_us {
            return None;
        }
        self.next_touch_us = now_us + self.cfg.touch_interval_us;
        let seq = self.touch_seq;
        self.touch_seq = self.touch_seq.wrapping_add(1);
        self.encode(&Message::Touch { state_seq: seq, t_client_us: now_us, fingers: fingers.to_vec() })
    }

    /// Spedisce un evento di tastiera: `code` è il codice HID, `down` dice premuto (`true`) o rilasciato (`false`).
    /// Sono eventi, non stato: vanno mandati tutti, anche due di seguito con lo stesso codice. Prima dell'apertura
    /// della sessione non parte nulla (l'evento non viene accodato né numerato).
    pub fn key(&mut self, now_us: u64, code: u16, down: bool) -> Option<Vec<u8>> {
        if self.closed.is_some() || self.negotiated.is_none() {
            return None;
        }
        let seq = self.key_seq;
        self.key_seq = self.key_seq.wrapping_add(1);
        self.encode(&Message::Key { key_seq: seq, t_client_us: now_us, code, down })
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

    /// Il decoder dell'app ha scartato o non è riuscito a decodificare un frame già consegnato: i frame successivi
    /// sarebbero corrotti fino al prossimo keyframe. Il ricevitore li scarta e chiede un keyframe all'host;
    /// restituisce il datagramma da spedire (`None` se una richiesta è già in corso o la sessione non è aperta).
    pub fn request_keyframe(&mut self, now_us: u64) -> Option<Vec<u8>> {
        if self.closed.is_some() || self.negotiated.is_none() {
            return None;
        }
        let outs = self.video.request_resync(now_us);
        let mut send = Vec::new();
        self.handle_outputs(now_us, outs, &mut send);
        send.into_iter().next()
    }

    /// Statistiche per l'host. Prima dell'apertura della sessione non parte nulla.
    pub fn stats_message(&mut self, now_us: u64) -> Option<Vec<u8>> {
        if self.closed.is_some() || self.negotiated.is_none() {
            return None;
        }
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
        // l'apertura è obbligatoria: senza ack il client manderebbe solo `Hello` e nessun ping
        let hello = client.tick(0).remove(0);
        let ack = host.on_datagram(5_000_000, &hello).remove(0);
        client.on_datagram(500, &ack);
        // il client manda il suo primo ping a t = 1000 µs; l'orologio dell'host è avanti di 5 s.
        let ping = client
            .tick(1_000)
            .into_iter()
            .find(|d| matches!(decode(d), Ok((_, Message::Ping { .. }))))
            .unwrap_or_default();
        assert!(!ping.is_empty(), "il ping parte subito");
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
        assert_eq!(host.negotiated(), None, "nessuna sessione aperta");
    }
}

#[cfg(test)]
mod negotiation {
    use super::*;

    fn hello_from(bytes: &[u8]) -> Option<Message<'_>> {
        match decode(bytes) {
            Ok((_, m)) => Some(m),
            Err(_) => None,
        }
    }

    /// Il primo datagramma del client è `Hello`: ruolo client, la versione che parliamo, il codec configurato.
    #[test]
    fn client_opens_with_hello() {
        let cfg = SessionConfig { codec: Codec::H264, nonce: [7; 16], ..SessionConfig::default() };
        let mut client = Client::new(cfg);
        let first = client.tick(0).remove(0);
        let Some(Message::Hello { role, proto_min, proto_max, caps: c, nonce, .. }) = hello_from(&first) else {
            panic!("il primo datagramma deve essere un Hello");
        };
        assert_eq!(role, Role::Client);
        assert_eq!((proto_min, proto_max), (VERSION, VERSION));
        assert_eq!(nonce, [7; 16]);
        assert_ne!(c & caps::H264, 0, "annuncia il codec configurato");
        assert_eq!(c & caps::H265, 0, "e non l'altro");
        assert_ne!(c & caps::NACK, 0);
        assert_ne!(c & caps::KEYFRAME_REQUEST, 0);
    }

    /// Handshake completo: una versione sola, caps comuni, e il client smette di rimandare l'Hello.
    #[test]
    fn handshake_agrees_on_protocol_and_common_caps() {
        let cfg = SessionConfig { nonce: [3; 16], ..SessionConfig::default() };
        let mut client = Client::new(cfg.clone());
        let mut host = Host::new(SessionConfig { nonce: [9; 16], ..cfg.clone() });

        let hello = client.tick(0).remove(0);
        let ack = host.on_datagram(5_000_000, &hello).remove(0);
        client.on_datagram(1_000, &ack);

        let n = client.negotiated().unwrap_or(Negotiated { proto: 200, caps: 0, peer_nonce: [0; 16] });
        assert_eq!(n.proto, VERSION);
        assert_eq!(n.peer_nonce, [9; 16], "il nonce dell'host viaggia nell'ack");
        assert_ne!(n.caps & caps::XOR_PARITY, 0, "l'host ha la parità attiva (fec_group > 0)");
        assert_ne!(n.caps & caps::H265, 0);
        assert_eq!(n.caps, host.negotiated().unwrap_or(n).caps, "i due lati vedono le stesse funzioni");

        assert!(host.negotiated().is_some_and(|h| h.peer_nonce == [3; 16]));
        // negoziata: i tick successivi non rimandano Hello
        for now in [2_000, 250_000, 1_000_000] {
            assert!(
                client.tick(now).iter().all(|d| !matches!(hello_from(d), Some(Message::Hello { .. }))),
                "niente più Hello dopo l'ack"
            );
        }
    }

    /// L'ack può perdersi: il client ritenta allo stesso intervallo finché non arriva, poi si ferma.
    #[test]
    fn hello_is_retried_until_ack() {
        let cfg = SessionConfig::default();
        let mut client = Client::new(cfg.clone());
        let retry = cfg.hello_retry_us;
        let hellos = |now: u64, c: &mut Client| {
            c.tick(now).into_iter().filter(|d| matches!(hello_from(d), Some(Message::Hello { .. }))).count()
        };
        assert_eq!(hellos(0, &mut client), 1);
        assert_eq!(hellos(retry / 2, &mut client), 0, "prima dell'intervallo non si ripete");
        assert_eq!(hellos(retry, &mut client), 1, "allo scadere si ritenta");

        let mut host = Host::new(cfg);
        let hello = client
            .tick(2 * retry)
            .into_iter()
            .find(|d| matches!(hello_from(d), Some(Message::Hello { .. })))
            .unwrap_or_default();
        let ack = host.on_datagram(0, &hello).remove(0);
        client.on_datagram(2 * retry + 1, &ack);
        assert!(client.negotiated().is_some());
        assert_eq!(hellos(3 * retry, &mut client), 0, "dopo l'ack non si ritenta più");
        // un ack duplicato non cambia la negoziazione
        let before = client.negotiated();
        client.on_datagram(3 * retry + 1, &ack);
        assert_eq!(client.negotiated(), before);
    }

    /// Un `Hello` col ruolo dell'host si ignora: la sessione la apre solo il client.
    #[test]
    fn hello_with_host_role_is_ignored() {
        let cfg = SessionConfig::default();
        let mut host = Host::new(cfg.clone());
        let bogus = Message::Hello {
            role: Role::Host,
            proto_min: VERSION,
            proto_max: VERSION,
            caps: u32::MAX,
            nonce: [1; 16],
            auth_token: None,
        }
        .encode(Header { conn_id: cfg.conn_id, seq: 0 })
        .unwrap_or_default();
        assert!(host.on_datagram(0, &bogus).is_empty(), "nessuna risposta");
        assert_eq!(host.negotiated(), None);
    }

    /// Nessuna versione in comune: l'host chiude con `BAD_PROTOCOL` e non negozia nulla.
    #[test]
    fn newer_protocol_is_refused() {
        let cfg = SessionConfig::default();
        let mut host = Host::new(cfg.clone());
        let future = Message::Hello {
            role: Role::Client,
            proto_min: VERSION + 1,
            proto_max: VERSION + 1,
            caps: 0,
            nonce: [2; 16],
            auth_token: None,
        }
        .encode(Header { conn_id: cfg.conn_id, seq: 0 })
        .unwrap_or_default();
        let out = host.on_datagram(0, &future);
        assert_eq!(out.len(), 1, "una sola risposta");
        assert!(
            matches!(hello_from(&out[0]), Some(Message::Close { reason: close_reason::BAD_PROTOCOL })),
            "chiusura per versione non comune"
        );
        assert_eq!(host.negotiated(), None);

        // un ack di una versione che non parliamo non apre la sessione
        let mut client = Client::new(cfg.clone());
        let ack = Message::HelloAck { proto: VERSION + 1, caps: 0, nonce: [0; 16] }
            .encode(Header { conn_id: cfg.conn_id, seq: 0 })
            .unwrap_or_default();
        client.on_datagram(0, &ack);
        assert_eq!(client.negotiated(), None);
    }

    /// Un client capace di decodificare entrambi i codec negozia H.264 quando l'host sceglie H.264.
    #[test]
    fn client_with_extra_caps_negotiates_fallback_codec() {
        let mut client = Client::new(SessionConfig {
            codec: Codec::H265,
            extra_caps: caps::H264 | caps::H265,
            ..SessionConfig::default()
        });
        let mut host = Host::new(SessionConfig { codec: Codec::H264, ..SessionConfig::default() });
        let hello = client.tick(0).remove(0);
        let ack = host.on_datagram(0, &hello).remove(0);
        client.on_datagram(1_000, &ack);
        let n = client.negotiated().expect("negoziata");
        assert_ne!(n.caps & caps::H264, 0, "H.264 negoziato");
        assert_eq!(n.caps & caps::H265, 0, "solo il codec reale dell'host");
    }
}

#[cfg(test)]
mod gate {
    use super::*;

    fn datagram(cfg: &SessionConfig, seq: u32, m: &Message<'_>) -> Vec<u8> {
        m.encode(Header { conn_id: cfg.conn_id, seq }).unwrap_or_default()
    }

    /// L'host non produce video (né un keyframe) finché non arriva un `Hello` valido.
    #[test]
    fn host_sends_video_only_after_hello() {
        let cfg = SessionConfig::default();
        let mut host = Host::new(cfg.clone());
        assert!(!host.wants_keyframe(0), "prima dell'apertura l'encoder non produce keyframe");
        assert!(host.send_frame(0, true, 0, &[1, 2, 3]).is_empty(), "niente video prima dell'apertura");
        assert_eq!(host.stats().frames_sent, 0, "il frame scartato non entra nelle statistiche");

        let mut client = Client::new(cfg);
        let hello = client.tick(0).remove(0);
        assert_eq!(host.on_datagram(0, &hello).len(), 1, "l'host risponde con l'ack");
        assert!(host.wants_keyframe(0), "aperta: serve subito un keyframe");
        assert!(!host.send_frame(0, true, 0, &[1, 2, 3]).is_empty(), "dopo l'apertura il video parte");
        assert_eq!(host.stats().frames_sent, 1);
    }

    /// L'host ignora ping, tocco, tastiera e richieste prima del `Hello`: nessuna risposta, nessun contatore.
    #[test]
    fn host_ignores_input_before_hello() {
        let cfg = SessionConfig::default();
        let mut host = Host::new(cfg.clone());
        let fingers = vec![Finger { id: 0, x: 1, y: 2, pressure: 3 }];
        let ping = datagram(&cfg, 0, &Message::Ping { id: 0, t_client_us: 0 });
        let touch = datagram(&cfg, 1, &Message::Touch { state_seq: 0, t_client_us: 0, fingers });
        let key = datagram(&cfg, 2, &Message::Key { key_seq: 0, t_client_us: 0, code: 4, down: true });
        let kreq = datagram(&cfg, 3, &Message::KeyframeRequest { reason: 0, last_good_frame: 0 });
        for d in [&ping, &touch, &key, &kreq] {
            assert!(host.on_datagram(0, d).is_empty(), "nessuna risposta prima dell'apertura");
        }
        assert_eq!(host.stats().touch_states, 0);
        assert_eq!(host.stats().key_events, 0);
        assert_eq!(host.stats().keyframe_requests, 0);
        assert!(host.take_touch().is_none());
        assert!(host.take_key().is_none());
        assert!(!host.wants_keyframe(0), "la richiesta di keyframe non apre il cancello");

        // con il `Hello` le stesse richieste vengono accettate
        let hello = datagram(
            &cfg,
            4,
            &Message::Hello {
                role: Role::Client,
                proto_min: VERSION,
                proto_max: VERSION,
                caps: 0,
                nonce: [1; 16],
                auth_token: None,
            },
        );
        assert_eq!(host.on_datagram(0, &hello).len(), 1, "l'host risponde all'Hello");
        assert_eq!(host.on_datagram(0, &ping).len(), 1, "ora il ping riceve il pong");
        assert!(host.on_datagram(0, &touch).is_empty(), "il tocco non richiede risposta");
        assert!(host.take_touch().is_some(), "ora il tocco arriva");
        host.on_datagram(0, &key);
        assert!(host.take_key().is_some(), "ora la tastiera arriva");
        host.on_datagram(0, &kreq);
        assert_eq!(host.stats().keyframe_requests, 1);
        assert!(host.wants_keyframe(0));
    }

    /// Il client manda solo `Hello` finché non riceve l'ack: niente ping, tocco, tastiera o statistiche.
    #[test]
    fn client_waits_for_ack_before_sending_anything_else() {
        let cfg = SessionConfig::default();
        let mut client = Client::new(cfg.clone());
        let fingers = [Finger { id: 0, x: 1, y: 2, pressure: 3 }];
        assert_eq!(client.tick(0).len(), 1, "il primo Hello parte subito");
        assert!(client.tick(1_000).is_empty(), "prima dell'intervallo non parte nulla");
        let out = client.tick(cfg.hello_retry_us);
        assert_eq!(out.len(), 1, "allo scadere si ritenta con un solo Hello");
        assert!(matches!(decode(&out[0]), Ok((_, Message::Hello { .. }))));
        assert!(client.touch_tick(1_000, &fingers).is_none(), "niente tocco prima dell'ack");
        assert!(client.key(1_000, 4, true).is_none(), "niente tastiera prima dell'ack");
        assert!(client.stats_message(1_000).is_none(), "niente statistiche prima dell'ack");

        let mut host = Host::new(cfg.clone());
        let hello = client
            .tick(2 * cfg.hello_retry_us)
            .into_iter()
            .find(|d| matches!(decode(d), Ok((_, Message::Hello { .. }))))
            .unwrap_or_default();
        let ack = host.on_datagram(0, &hello).remove(0);
        client.on_datagram(2 * cfg.hello_retry_us + 1, &ack);
        assert!(client.negotiated().is_some());

        let out = client.tick(2 * cfg.hello_retry_us + 2);
        assert!(out.iter().any(|d| matches!(decode(d), Ok((_, Message::Ping { .. })))), "dopo l'ack partono i ping");
        assert!(client.touch_tick(2 * cfg.hello_retry_us + 3, &fingers).is_some(), "e il tocco");
        assert!(client.key(2 * cfg.hello_retry_us + 3, 4, true).is_some(), "e la tastiera");
        assert!(client.stats_message(2 * cfg.hello_retry_us + 3).is_some(), "e le statistiche");
    }

    /// Il client scarta video e pong prima dell'ack; dopo l'ack lo stesso video viene consegnato.
    #[test]
    fn client_ignores_video_and_pong_before_ack() {
        let cfg = SessionConfig::default();
        let mut client = Client::new(cfg.clone());
        let mut host = Host::new(cfg.clone());
        let hello = client.tick(0).remove(0);
        let ack = host.on_datagram(0, &hello).remove(0);
        assert!(host.negotiated().is_some(), "l'host ha aperto la sessione");
        let video = host.send_frame(0, true, 0, &[1, 2, 3, 4]);
        assert!(!video.is_empty(), "l'host manda il video");

        for d in &video {
            assert!(client.on_datagram(0, d).is_empty(), "il video prima dell'ack si scarta");
        }
        assert!(client.delivered.is_empty(), "nessun frame consegnato");
        assert_eq!(client.video_stats().fragments, 0, "il ricevitore non ha visto frammenti");
        let pong = datagram(&cfg, 0, &Message::Pong { id: 0, t_client_us: 0, t_host_rx_us: 0, t_host_tx_us: 0 });
        client.on_datagram(0, &pong);
        assert_eq!(client.clock.len(), 0, "nessun campione d'orologio prima dell'ack");

        client.on_datagram(1, &ack);
        assert!(client.negotiated().is_some(), "ora la sessione è aperta");
        for d in &video {
            client.on_datagram(1, d);
        }
        assert_eq!(client.delivered.len(), 1, "dopo l'ack il frame arriva");
    }
}

#[cfg(test)]
mod keyboard {
    use super::*;

    /// Apre la sessione: prima dell'ack la tastiera non parte.
    fn opened(cfg: &SessionConfig) -> (Host, Client) {
        let mut client = Client::new(cfg.clone());
        let mut host = Host::new(cfg.clone());
        let hello = client.tick(0).remove(0);
        let ack = host.on_datagram(0, &hello).remove(0);
        client.on_datagram(0, &ack);
        assert!(client.negotiated().is_some());
        assert!(host.negotiated().is_some());
        (host, client)
    }

    #[test]
    fn press_and_release_arrive_in_order() {
        let cfg = SessionConfig::default();
        let (mut host, mut client) = opened(&cfg);
        for (code, down) in [(0x04u16, true), (0x04, false)] {
            let d = client.key(1_000, code, down).expect("datagramma di tastiera");
            host.on_datagram(0, &d);
        }
        let a = host.take_key().expect("prima pressione");
        assert_eq!((a.seq, a.code, a.down, a.t_client_us), (0, 0x04, true, 1_000));
        let b = host.take_key().expect("rilascio");
        assert_eq!((b.seq, b.code, b.down), (1, 0x04, false));
        assert_eq!(host.take_key(), None, "la coda si svuota");
        assert_eq!(host.stats().key_events, 2);
    }

    #[test]
    fn duplicates_and_stale_events_are_dropped() {
        let cfg = SessionConfig::default();
        let (mut host, mut client) = opened(&cfg);
        let press = client.key(0, 4, true).expect("pressione");
        let release = client.key(0, 4, false).expect("rilascio");
        host.on_datagram(0, &release);
        host.on_datagram(0, &press); // vecchio, arrivato dopo
        host.on_datagram(0, &release); // duplicato
        let k = host.take_key().expect("solo il rilascio");
        assert_eq!((k.seq, k.down), (1, false));
        assert_eq!(host.take_key(), None);
        assert_eq!(host.stats().key_events, 1);
    }

    #[test]
    fn queue_is_bounded_and_keeps_the_newest() {
        let cfg = SessionConfig::default();
        let (mut host, mut client) = opened(&cfg);
        let n = MAX_PENDING_KEYS + 5;
        for i in 0..n {
            let d = client.key(0, 4, i % 2 == 0).expect("datagramma di tastiera");
            host.on_datagram(0, &d);
        }
        let mut drained = 0;
        let mut last = None;
        while let Some(k) = host.take_key() {
            drained += 1;
            last = Some(k.seq);
        }
        assert_eq!(drained, MAX_PENDING_KEYS, "la coda non cresce oltre il limite");
        assert_eq!(last, Some((n - 1) as u32), "restano gli eventi più recenti");
    }

    #[test]
    fn closed_client_sends_no_keys() {
        let mut client = Client::new(SessionConfig::default());
        assert!(client.close(close_reason::NORMAL).is_some());
        assert!(client.key(0, 4, true).is_none(), "sessione chiusa: niente eventi");
    }
}

#[cfg(test)]
mod close {
    use super::*;

    #[test]
    fn client_close_reaches_the_host() {
        let cfg = SessionConfig::default();
        let mut client = Client::new(cfg.clone());
        let mut host = Host::new(cfg.clone());
        let d = client.close(close_reason::NORMAL).expect("datagramma di chiusura");
        assert!(client.is_closed());
        assert_eq!(client.close_reason(), Some(close_reason::NORMAL));
        assert!(host.on_datagram(0, &d).is_empty(), "alla chiusura non si risponde");
        assert!(host.is_closed());
        assert_eq!(host.close_reason(), Some(close_reason::NORMAL));

        // chiusa: niente risposte né video, anche a messaggi validi
        let ping =
            Message::Ping { id: 0, t_client_us: 0 }.encode(Header { conn_id: cfg.conn_id, seq: 0 }).unwrap_or_default();
        assert!(host.on_datagram(1_000, &ping).is_empty(), "nessun pong dopo la chiusura");
        assert!(host.send_frame(0, true, 0, &[1, 2, 3]).is_empty(), "nessun video dopo la chiusura");
        assert!(client.tick(1_000).is_empty(), "il client chiuso non manda niente");
    }

    #[test]
    fn host_close_reaches_the_client() {
        let cfg = SessionConfig::default();
        let mut host = Host::new(cfg.clone());
        let mut client = Client::new(cfg);
        let d = host.close(close_reason::NORMAL).expect("datagramma di chiusura");
        assert!(client.on_datagram(0, &d).is_empty());
        assert!(client.is_closed());
        assert_eq!(client.close_reason(), Some(close_reason::NORMAL));
        let fingers = [Finger { id: 0, x: 0, y: 0, pressure: 0 }];
        assert!(client.touch_tick(1_000, &fingers).is_none(), "niente tocco dopo la chiusura");
        assert!(client.key(1_000, 4, true).is_none(), "niente tastiera dopo la chiusura");
        assert!(client.stats_message(1_000).is_none(), "niente statistiche dopo la chiusura");
    }

    #[test]
    fn closing_twice_sends_nothing() {
        let mut host = Host::new(SessionConfig::default());
        assert!(host.close(close_reason::NORMAL).is_some());
        assert!(host.close(close_reason::NORMAL).is_none(), "la seconda chiusura non manda nulla");
        let mut client = Client::new(SessionConfig::default());
        assert!(client.close(close_reason::NORMAL).is_some());
        assert!(client.close(close_reason::NORMAL).is_none());
    }

    #[test]
    fn refused_protocol_does_not_close_the_host() {
        let cfg = SessionConfig::default();
        let mut host = Host::new(cfg.clone());
        let future = Message::Hello {
            role: Role::Client,
            proto_min: VERSION + 1,
            proto_max: VERSION + 1,
            caps: 0,
            nonce: [0; 16],
            auth_token: None,
        }
        .encode(Header { conn_id: cfg.conn_id, seq: 0 })
        .unwrap_or_default();
        assert_eq!(host.on_datagram(0, &future).len(), 1, "una chiusura per versione non comune");
        assert!(!host.is_closed(), "l'host resta aperto per un Hello corretto");
        // e infatti un Hello valido viene accettato
        let hello = Message::Hello {
            role: Role::Client,
            proto_min: VERSION,
            proto_max: VERSION,
            caps: 0,
            nonce: [1; 16],
            auth_token: None,
        }
        .encode(Header { conn_id: cfg.conn_id, seq: 1 })
        .unwrap_or_default();
        let ack = host.on_datagram(1_000, &hello);
        assert_eq!(ack.len(), 1, "l'host risponde all'Hello corretto");
        assert!(host.negotiated().is_some());
    }

    #[test]
    fn auth_token_rejects_unauthorized_and_accepts_paired_client() {
        let token = [0x42u8; 16];
        let mut host = Host::new(SessionConfig { auth_token: Some(token), ..SessionConfig::default() });
        let mut intruder = Client::new(SessionConfig { auth_token: Some([0x99; 16]), ..SessionConfig::default() });
        let mut unauthenticated = Client::new(SessionConfig::default());

        let bad_hello = intruder.tick(0).remove(0);
        assert!(!verify_client_hello(SessionConfig::default().conn_id, Some(token), &bad_hello));
        let reply = host.on_datagram(0, &bad_hello);
        assert_eq!(reply.len(), 1);
        assert!(matches!(decode(&reply[0]), Ok((_, Message::Close { reason: close_reason::UNAUTHORIZED }))));
        assert!(host.negotiated().is_none());
        assert!(!host.is_closed(), "un intruso non chiude l'host");

        let no_tok = unauthenticated.tick(0).remove(0);
        assert!(!verify_client_hello(SessionConfig::default().conn_id, Some(token), &no_tok));
        let reply2 = host.on_datagram(10, &no_tok);
        assert!(matches!(decode(&reply2[0]), Ok((_, Message::Close { reason: close_reason::UNAUTHORIZED }))));

        let mut paired = Client::new(SessionConfig { auth_token: Some(token), ..SessionConfig::default() });
        let ok_hello = paired.tick(0).remove(0);
        assert!(verify_client_hello(SessionConfig::default().conn_id, Some(token), &ok_hello));
        let ack = host.on_datagram(20, &ok_hello).remove(0);
        paired.on_datagram(30, &ack);
        assert!(host.negotiated().is_some());
        assert!(paired.negotiated().is_some());
    }

    #[test]
    fn audio_clipboard_and_control_work_after_open() {
        let cfg = SessionConfig { audio_enabled: true, ..SessionConfig::default() };
        let mut host = Host::new(cfg.clone());
        let mut client = Client::new(cfg);

        assert!(host.send_audio(AudioCodec::AacLc, 48_000, 2, true, 0, &[0x11, 0x90]).is_none());
        assert!(client.send_clipboard("prima").is_none());
        assert!(client.send_control(ControlCommand::ScreenOff).is_none());

        let hello = client.tick(0).remove(0);
        let ack = host.on_datagram(0, &hello).remove(0);
        client.on_datagram(1, &ack);
        assert_ne!(client.negotiated().expect("aperto").caps & caps::AUDIO_AAC, 0);

        let asc = host.send_audio(AudioCodec::AacLc, 48_000, 2, true, 100, &[0x11, 0x90]).expect("asc");
        let aac = host.send_audio(AudioCodec::AacLc, 48_000, 2, false, 21_433, &[1, 2, 3, 4]).expect("aac");
        client.on_datagram(2, &asc);
        client.on_datagram(3, &aac);
        let rx_audio = client.take_audio();
        assert_eq!(rx_audio.len(), 2);
        assert!(rx_audio[0].config && rx_audio[0].data == [0x11, 0x90]);
        assert!(!rx_audio[1].config && rx_audio[1].data == [1, 2, 3, 4]);

        let c2h = client.send_clipboard("da iPhone").expect("clip c->h");
        host.on_datagram(4, &c2h);
        host.on_datagram(5, &c2h); // duplicato ignorato
        assert_eq!(host.take_clipboard().as_deref(), Some("da iPhone"));
        assert_eq!(host.take_clipboard(), None);

        let h2c = host.send_clipboard("da Android").expect("clip h->c");
        client.on_datagram(6, &h2c);
        assert_eq!(client.take_clipboard().as_deref(), Some("da Android"));

        let ctrl = client.send_control(ControlCommand::ScreenOff).expect("screen off");
        host.on_datagram(7, &ctrl);
        host.on_datagram(8, &ctrl); // duplicato ignorato
        assert_eq!(host.take_control(), Some(ControlCommand::ScreenOff));
        assert_eq!(host.take_control(), None);
    }
}

#[cfg(test)]
mod resync {
    use super::*;

    /// Apre la sessione tra un client e un host appena creati.
    fn open(cfg: &SessionConfig) -> (Host, Client) {
        let mut host = Host::new(cfg.clone());
        let mut client = Client::new(cfg.clone());
        let hello = client.tick(0).remove(0);
        let ack = host.on_datagram(0, &hello).remove(0);
        client.on_datagram(0, &ack);
        assert!(client.negotiated().is_some());
        (host, client)
    }

    fn deliver(host: &mut Host, client: &mut Client, now_us: u64, id: u32, key: bool) -> usize {
        let before = client.delivered.len();
        for d in host.send_frame(id, key, now_us, &[id as u8; 300]) {
            client.on_datagram(now_us, &d);
        }
        client.delivered.len() - before
    }

    /// L'host può chiedere un keyframe da solo (coda del trasporto piena), ma solo a sessione aperta e rispettando la
    /// distanza minima tra keyframe.
    #[test]
    fn host_side_keyframe_request_respects_gate_and_gap() {
        let cfg = SessionConfig::default();
        let mut closed_host = Host::new(cfg.clone());
        closed_host.request_keyframe();
        assert!(!closed_host.wants_keyframe(1_000_000), "prima dell'apertura non si chiede nulla");

        let (mut host, _client) = open(&cfg);
        assert!(host.wants_keyframe(0));
        host.send_frame(0, true, 0, &[0; 100]);
        assert!(!host.wants_keyframe(0), "keyframe appena spedito");
        host.request_keyframe();
        assert!(!host.wants_keyframe(cfg.min_keyframe_gap_us - 1), "troppo presto dopo l'ultimo keyframe");
        assert!(host.wants_keyframe(cfg.min_keyframe_gap_us), "dopo la distanza minima sì");
    }

    /// Se l'app perde un frame consegnato, il client chiede un keyframe (una volta sola), scarta i frame non-key che
    /// sarebbero corrotti e riprende dal keyframe.
    #[test]
    fn client_resync_drops_delta_frames_until_keyframe() {
        let cfg = SessionConfig::default();
        let mut never_opened = Client::new(cfg.clone());
        assert!(never_opened.request_keyframe(0).is_none(), "sessione non aperta: niente richiesta");

        let (mut host, mut client) = open(&cfg);
        assert_eq!(deliver(&mut host, &mut client, 1_000, 0, true), 1);
        assert_eq!(deliver(&mut host, &mut client, 2_000, 1, false), 1);

        let req = client.request_keyframe(3_000).expect("richiesta di keyframe");
        assert!(client.request_keyframe(3_500).is_none(), "una richiesta è già in corso");
        let before = host.stats().keyframe_requests;
        host.on_datagram(3_600, &req);
        assert_eq!(host.stats().keyframe_requests, before + 1);

        assert_eq!(deliver(&mut host, &mut client, 4_000, 2, false), 0, "frame non-key scartato: sarebbe corrotto");
        assert_eq!(deliver(&mut host, &mut client, 400_000, 3, true), 1, "il keyframe riprende la riproduzione");
        assert_eq!(deliver(&mut host, &mut client, 410_000, 4, false), 1, "e i frame successivi tornano normali");

        let again = cfg.receiver.key_retry_us + 3_000;
        assert!(client.request_keyframe(again).is_some(), "dopo il keyframe si può richiedere di nuovo");
    }
}

#[cfg(test)]
mod host_report {
    use super::*;

    /// Il rapporto dell'host parte solo a sessione aperta, arriva al client e porta il conteggio dei keyframe.
    #[test]
    fn host_report_reaches_client_after_open() {
        let cfg = SessionConfig::default();
        let mut host = Host::new(cfg.clone());
        let mut client = Client::new(cfg);
        let sample = HostReport { encoder_fps_x10: 600, width: 1080, height: 2400, ..HostReport::default() };
        assert!(host.host_report(sample).is_none(), "prima dell'apertura non parte nulla");

        let hello = client.tick(0).remove(0);
        let ack = host.on_datagram(0, &hello).remove(0);
        client.on_datagram(0, &ack);
        assert_ne!(client.negotiated().map_or(0, |n| n.caps) & caps::HOST_REPORT, 0, "funzione concordata");

        host.send_frame(0, true, 0, &[1; 64]);
        host.send_frame(1, false, 0, &[2; 64]);
        host.send_frame(2, true, 0, &[3; 64]);
        assert!(client.host_report().is_none());
        let d = host.host_report(sample).expect("rapporto");
        client.on_datagram(10, &d);
        let got = client.host_report().expect("ricevuto");
        assert_eq!(got.encoder_fps_x10, 600);
        assert_eq!((got.width, got.height), (1080, 2400));
        assert_eq!(got.keyframes, 2, "i keyframe li conta l'host");
        assert_eq!(host.stats().keyframes_sent, 2);
    }
}

#[cfg(test)]
mod host_log {
    use super::*;

    fn opened() -> (Host, Client) {
        let cfg = SessionConfig::default();
        let mut client = Client::new(cfg.clone());
        let mut host = Host::new(cfg);
        let hello = client.tick(0).remove(0);
        let ack = host.on_datagram(0, &hello).remove(0);
        client.on_datagram(0, &ack);
        assert_ne!(client.negotiated().map_or(0, |n| n.caps) & caps::HOST_LOG, 0, "funzione concordata");
        (host, client)
    }

    fn sample_log() -> String {
        (0..120).map(|i| format!("20:42:{i:02} · Rete in affanno · è città №{i}\n")).collect()
    }

    /// Il client chiede il log, l'host lo spedisce a pezzi (due copie) e il client lo ricompone uguale, accenti compresi
    /// anche se un pezzo taglia un carattere a metà.
    #[test]
    fn log_travels_in_chunks_and_is_reassembled() {
        let (mut host, mut client) = opened();
        let req = client.send_control(ControlCommand::SendLog).expect("richiesta");
        host.on_datagram(1_000, &req);
        assert_eq!(host.take_control(), Some(ControlCommand::SendLog));
        let text = sample_log();
        let datagrams = host.send_log(&text);
        let parts = text.len().div_ceil(MAX_HOST_LOG_CHUNK);
        assert_eq!(datagrams.len(), parts * 2, "ogni pezzo esce due volte");
        assert!(datagrams.iter().all(|d| d.len() <= phantom_proto::MAX_DATAGRAM));
        // arriva solo la seconda copia, in ordine inverso
        for d in datagrams[parts..].iter().rev() {
            client.on_datagram(2_000, d);
        }
        let got = client.take_host_log(false).expect("log completo");
        assert_eq!(got.text, text);
        assert_eq!((got.received, got.total), (parts as u16, parts as u16));
        assert!(client.take_host_log(true).is_none(), "una volta preso non torna");
    }

    /// Se manca un pezzo, il log completo non c'è; quello parziale sì, con il buco segnato.
    #[test]
    fn missing_parts_are_marked_in_a_partial_log() {
        let (mut host, mut client) = opened();
        let text = sample_log();
        let datagrams = host.send_log(&text);
        let parts = datagrams.len() / 2;
        for (i, d) in datagrams[..parts].iter().enumerate() {
            if i != 1 {
                client.on_datagram(2_000, d);
            }
        }
        assert!(client.take_host_log(false).is_none());
        let got = client.take_host_log(true).expect("log parziale");
        assert_eq!((got.received, got.total), (parts as u16 - 1, parts as u16));
        assert!(got.text.contains(&format!("pezzo 2 di {parts} non arrivato")));
    }

    /// Prima dell'apertura non parte nulla; un log enorme tiene la parte finale (la più recente).
    #[test]
    fn log_needs_an_open_session_and_keeps_the_tail() {
        let mut host = Host::new(SessionConfig::default());
        assert!(host.send_log("ciao").is_empty());
        let (mut host, mut client) = opened();
        let big = "x".repeat(200_000) + "FINE";
        let datagrams = host.send_log(&big);
        assert_eq!(datagrams.len(), usize::from(MAX_HOST_LOG_PARTS) * 2);
        for d in &datagrams {
            client.on_datagram(2_000, d);
        }
        let got = client.take_host_log(false).expect("log");
        assert!(got.text.ends_with("FINE"));
        assert_eq!(got.text.len(), MAX_HOST_LOG_CHUNK * usize::from(MAX_HOST_LOG_PARTS));
    }
}

#[cfg(test)]
mod video_pause {
    use super::*;

    fn opened() -> (Host, Client) {
        let cfg = SessionConfig::default();
        let mut client = Client::new(cfg.clone());
        let mut host = Host::new(cfg);
        let hello = client.tick(0).remove(0);
        let ack = host.on_datagram(0, &hello).remove(0);
        client.on_datagram(0, &ack);
        assert_ne!(client.negotiated().map_or(0, |n| n.caps) & caps::VIDEO_PAUSE, 0, "funzione concordata");
        (host, client)
    }

    /// In background l'host non manda video né chiede keyframe; al ritorno riparte subito da un keyframe, anche se
    /// l'ultimo è appena partito.
    #[test]
    fn paused_host_sends_nothing_and_resumes_with_a_keyframe() {
        let (mut host, mut client) = opened();
        assert!(!host.send_frame(1, true, 10_000, &[1, 2, 3]).is_empty());
        assert!(!host.wants_keyframe(20_000));
        host.on_datagram(20_000, &client.video_pause(true).expect("pausa"));
        assert_eq!(host.take_control(), None, "la pausa non arriva all'app come comando");
        assert!(host.video_paused(30_000));
        assert!(host.send_frame(2, false, 40_000, &[4, 5, 6]).is_empty(), "niente video in pausa");
        host.request_keyframe();
        assert!(!host.wants_keyframe(50_000), "niente keyframe in pausa");
        host.on_datagram(60_000, &client.video_pause(false).expect("ripresa"));
        assert!(!host.video_paused(60_000));
        assert!(host.wants_keyframe(60_000), "si riparte da un keyframe senza aspettare il distanziamento");
        assert!(!host.send_frame(3, true, 70_000, &[7]).is_empty());
    }

    /// Se il client sparisce o la ripresa si perde, la pausa scade da sola; se il client la rinnova, resta.
    #[test]
    fn pause_expires_unless_renewed() {
        let (mut host, mut client) = opened();
        host.on_datagram(1_000_000, &client.video_pause(true).expect("pausa"));
        host.on_datagram(3_000_000, &client.video_pause(true).expect("rinnovo"));
        assert!(host.video_paused(1_000_000 + VIDEO_PAUSE_LEASE_US + 500_000), "rinnovata: ancora in pausa");
        let after = 3_000_000 + VIDEO_PAUSE_LEASE_US;
        assert!(!host.video_paused(after), "scaduta");
        assert!(host.wants_keyframe(after));
        assert!(!host.send_frame(1, true, after + 1, &[1]).is_empty());
    }

    /// Con un host che non conosce la pausa il client non manda nulla.
    #[test]
    fn old_host_never_receives_the_command() {
        let cfg = SessionConfig::default();
        let mut client = Client::new(cfg.clone());
        let hello = client.tick(0).remove(0);
        let Ok((h, Message::Hello { proto_max, nonce, .. })) = decode(&hello) else { panic!("hello") };
        let old_caps = caps::H264 | caps::HOST_LOG;
        let ack = Message::HelloAck { proto: proto_max, caps: old_caps, nonce }.encode(h).expect("ack");
        client.on_datagram(0, &ack);
        assert!(client.negotiated().is_some());
        assert!(client.video_pause(true).is_none());
    }
}
