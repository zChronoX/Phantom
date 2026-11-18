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

use phantom_core::phantom_proto::{caps, Codec, Finger, MAX_DATAGRAM, MAX_FINGERS};
use phantom_core::{Client, Delivered, Host, KeyEvent, Negotiated, SessionConfig};

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
    }
}

impl From<SessionSettings> for SessionConfig {
    fn from(s: SessionSettings) -> Self {
        // il nonce del protocollo è fisso a 16 byte: qui si completa o si tronca, senza errori da gestire nell'app
        let mut nonce = [0u8; 16];
        let n = s.nonce.len().min(nonce.len());
        nonce[..n].copy_from_slice(&s.nonce[..n]);
        SessionConfig {
            conn_id: s.conn_id,
            codec: s.codec.into(),
            fec_group: s.fec_group,
            start_bitrate_bps: s.start_bitrate_bps,
            min_bitrate_bps: s.min_bitrate_bps,
            max_bitrate_bps: s.max_bitrate_bps,
            touch_interval_us: s.touch_interval_us,
            nonce,
            ..SessionConfig::default()
        }
    }
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
    /// Eventi di tastiera ricevuti.
    pub key_events: u64,
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
            key_events: s.key_events,
            bitrate_decreases: s.bitrate_decreases,
            target_bitrate_bps: g.host.target_bitrate_bps(),
        }
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
}
