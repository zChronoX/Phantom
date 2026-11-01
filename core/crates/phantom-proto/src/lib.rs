//! Protocollo di Phantom v0: formato su filo dei datagrammi tra host (Android) e client (iPhone).
//!
//! Principi:
//! - **Nessun panico**: ogni lettura è controllata nei limiti; dati malformati danno un `DecodeError`, mai un crash
//!   (il parser è esposto alla rete).
//! - **Little-endian**, campi a larghezza fissa, nessuna allocazione in lettura (i payload sono prestiti dal buffer).
//! - **Indipendente dal trasporto**: un datagramma è un'unità autonoma di al massimo [`MAX_DATAGRAM`] byte, adatta sia
//!   a UDP sia ai datagrammi QUIC (fase 2). La cifratura (fase 5) avvolgerà questo formato senza cambiarlo.
//! - **Tolleranza alle perdite**: il tocco è inviato come *stato completo delle dita*, non come eventi incrementali;
//!   il video è frammentato con un frammento di parità XOR opzionale.
//!
//! Intestazione comune (12 byte):
//!
//! | offset | campo     | note                                          |
//! |--------|-----------|-----------------------------------------------|
//! | 0      | `magic`   | `0x50` (`P`)                                  |
//! | 1      | `version` | 0                                             |
//! | 2      | `kind`    | tipo di messaggio ([`Kind`])                  |
//! | 3      | `flags`   | riservato, 0                                  |
//! | 4      | `conn_id` | identificatore di sessione (u32)              |
//! | 8      | `seq`     | contatore dei datagrammi del mittente (u32)   |

use core::fmt;

/// Byte iniziale di ogni datagramma.
pub const MAGIC: u8 = 0x50;
/// Versione del formato su filo implementata da questo crate.
pub const VERSION: u8 = 0;
/// Dimensione dell'intestazione comune.
pub const HEADER_LEN: usize = 12;
/// Dimensione massima di un datagramma: sta nel MTU minimo di QUIC (1200 byte) senza frammentazione IP.
pub const MAX_DATAGRAM: usize = 1200;
/// Intestazione specifica del messaggio video, escluso il payload.
pub const VIDEO_HEADER_LEN: usize = 28;
/// Byte di video trasportabili in un singolo frammento.
pub const MAX_VIDEO_PAYLOAD: usize = MAX_DATAGRAM - HEADER_LEN - VIDEO_HEADER_LEN;
/// Numero massimo di dita in un messaggio di tocco.
pub const MAX_FINGERS: usize = 10;
/// Numero massimo di frammenti indicati in un NACK.
pub const MAX_NACK: usize = 32;

/// Tipo di messaggio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Kind {
    /// Apertura della sessione (client → host).
    Hello = 1,
    /// Risposta all'apertura (host → client).
    HelloAck = 2,
    /// Richiesta di sincronia degli orologi (client → host).
    Ping = 3,
    /// Risposta alla richiesta di sincronia (host → client).
    Pong = 4,
    /// Un frammento di un frame video, o il suo frammento di parità (host → client).
    Video = 5,
    /// Richiesta di ritrasmissione di frammenti video (client → host).
    Nack = 6,
    /// Richiesta di un nuovo keyframe (client → host).
    KeyframeRequest = 7,
    /// Stato completo delle dita sul touchscreen (client → host).
    Touch = 8,
    /// Evento di tastiera (client → host).
    Key = 9,
    /// Statistiche di ricezione (client → host).
    Stats = 10,
    /// Chiusura della sessione (entrambi i versi).
    Close = 11,
}

impl Kind {
    fn from_u8(v: u8) -> Option<Kind> {
        Some(match v {
            1 => Kind::Hello,
            2 => Kind::HelloAck,
            3 => Kind::Ping,
            4 => Kind::Pong,
            5 => Kind::Video,
            6 => Kind::Nack,
            7 => Kind::KeyframeRequest,
            8 => Kind::Touch,
            9 => Kind::Key,
            10 => Kind::Stats,
            11 => Kind::Close,
            _ => return None,
        })
    }
}

/// Errori di lettura di un datagramma. Nessuno di questi è un bug: arrivano da dati di rete non fidati.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Il datagramma è più corto di quanto il tipo richiede.
    Truncated,
    /// Il primo byte non è [`MAGIC`].
    BadMagic,
    /// Versione del formato non supportata.
    BadVersion(u8),
    /// Tipo di messaggio sconosciuto.
    BadKind(u8),
    /// Un campo ha un valore non ammesso (indica quale).
    BadField(&'static str),
    /// Il datagramma supera [`MAX_DATAGRAM`].
    TooLarge,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "datagramma troncato"),
            DecodeError::BadMagic => write!(f, "magic non valido"),
            DecodeError::BadVersion(v) => write!(f, "versione {v} non supportata"),
            DecodeError::BadKind(k) => write!(f, "tipo di messaggio {k} sconosciuto"),
            DecodeError::BadField(n) => write!(f, "campo non valido: {n}"),
            DecodeError::TooLarge => write!(f, "datagramma troppo grande"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Intestazione comune di un datagramma.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Identificatore di sessione.
    pub conn_id: u32,
    /// Contatore dei datagrammi del mittente.
    pub seq: u32,
}

/// Ruolo di chi apre la sessione.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Role {
    /// Telefono Android che cattura lo schermo.
    Host = 1,
    /// iPhone che mostra lo schermo e manda l'input.
    Client = 2,
}

/// Codec video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Codec {
    /// H.264 / AVC.
    H264 = 1,
    /// H.265 / HEVC.
    H265 = 2,
}

/// Flag di un frammento video.
pub mod video_flags {
    /// Il frame è un keyframe.
    pub const KEY: u8 = 1;
    /// Il frame contiene i parametri del codec (SPS/PPS/VPS).
    pub const CONFIG: u8 = 2;
    /// Questo frammento è la parità XOR di un gruppo di frammenti dati del frame.
    pub const PARITY: u8 = 4;
    /// Questo frammento è una ritrasmissione richiesta con un NACK.
    pub const RETRANSMIT: u8 = 8;
}

/// Azione di un dito.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TouchAction {
    /// Il dito è appoggiato (stato corrente).
    Down = 1,
}

/// Un dito nello stato completo del touchscreen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Finger {
    /// Identificatore stabile del dito durante il contatto.
    pub id: u8,
    /// Ascissa normalizzata: 0 = bordo sinistro, 65535 = bordo destro.
    pub x: u16,
    /// Ordinata normalizzata: 0 = bordo alto, 65535 = bordo basso.
    pub y: u16,
    /// Pressione normalizzata 0–255.
    pub pressure: u8,
}

/// Un messaggio decodificato. I payload sono prestiti dal buffer di ingresso.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message<'a> {
    /// Apertura della sessione.
    Hello {
        /// Ruolo di chi scrive.
        role: Role,
        /// Versione minima del protocollo accettata.
        proto_min: u8,
        /// Versione massima del protocollo accettata.
        proto_max: u8,
        /// Funzioni supportate (bit).
        caps: u32,
        /// Valore casuale scelto da chi scrive.
        nonce: [u8; 16],
    },
    /// Risposta all'apertura.
    HelloAck {
        /// Versione scelta.
        proto: u8,
        /// Funzioni concordate (bit).
        caps: u32,
        /// Valore casuale dell'host.
        nonce: [u8; 16],
    },
    /// Richiesta di sincronia degli orologi.
    Ping {
        /// Numero progressivo della richiesta.
        id: u32,
        /// Orologio del client alla partenza (µs).
        t_client_us: u64,
    },
    /// Risposta di sincronia degli orologi.
    Pong {
        /// Numero della richiesta.
        id: u32,
        /// Orologio del client alla partenza della richiesta (µs), ripetuto.
        t_client_us: u64,
        /// Orologio dell'host alla ricezione (µs).
        t_host_rx_us: u64,
        /// Orologio dell'host alla risposta (µs).
        t_host_tx_us: u64,
    },
    /// Frammento di un frame video.
    Video(VideoFragment<'a>),
    /// Richiesta di ritrasmissione.
    Nack {
        /// Frame interessato.
        frame_id: u32,
        /// Indici dei frammenti mancanti.
        missing: Vec<u16>,
    },
    /// Richiesta di keyframe.
    KeyframeRequest {
        /// Motivo (0 = perdita, 1 = avvio, 2 = richiesta esplicita, 3 = ripetizione: il keyframe atteso non è arrivato).
        reason: u8,
        /// Ultimo frame ricevuto e decodificato correttamente.
        last_good_frame: u32,
    },
    /// Stato completo delle dita.
    Touch {
        /// Numero progressivo dello stato (cresce a ogni invio).
        state_seq: u32,
        /// Orologio del client al momento del campione (µs).
        t_client_us: u64,
        /// Dita appoggiate in questo istante.
        fingers: Vec<Finger>,
    },
    /// Evento di tastiera.
    Key {
        /// Numero progressivo.
        key_seq: u32,
        /// Orologio del client (µs).
        t_client_us: u64,
        /// Codice del tasto (HID usage).
        code: u16,
        /// `true` = premuto, `false` = rilasciato.
        down: bool,
    },
    /// Statistiche di ricezione.
    Stats(Stats),
    /// Chiusura della sessione.
    Close {
        /// Motivo (0 = normale).
        reason: u8,
    },
}

/// Frammento di un frame video.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoFragment<'a> {
    /// Numero del frame (cresce di uno a ogni frame).
    pub frame_id: u32,
    /// Posizione del frammento nel frame; per la parità vale `frag_count` + indice del gruppo.
    pub frag_index: u16,
    /// Numero di frammenti dati del frame (parità esclusa).
    pub frag_count: u16,
    /// Combinazione di [`video_flags`].
    pub flags: u8,
    /// Codec.
    pub codec: Codec,
    /// Dimensione di un frammento dati pieno usata per dividere il frame.
    pub frag_size: u16,
    /// Dimensione totale del frame in byte.
    pub total_len: u32,
    /// Istante di cattura sull'orologio dell'host (µs).
    pub capture_ts_us: u64,
    /// Numero di frammenti dati coperti da ciascun frammento di parità (0 = il frame non ha parità).
    pub fec_group: u16,
    /// Dati del frammento.
    pub payload: &'a [u8],
}

impl VideoFragment<'_> {
    /// `true` se è un frammento di parità.
    pub fn is_parity(&self) -> bool {
        self.flags & video_flags::PARITY != 0
    }
}

/// Numero di gruppi di parità di un frame con `frag_count` frammenti dati e gruppi da `fec_group` (0 se non c'è parità).
pub fn parity_groups(frag_count: u16, fec_group: u16) -> u16 {
    if fec_group == 0 {
        0
    } else {
        frag_count.div_ceil(fec_group)
    }
}

/// Valore di [`Stats::queue_delay_us`] quando il client non riesce a misurare la coda.
pub const QUEUE_DELAY_UNKNOWN: u32 = u32::MAX;

/// Statistiche di ricezione inviate dal client. I contatori sono cumulativi: l'host ne fa le differenze.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    /// Byte di payload video ricevuti (cumulativo, a 32 bit con avvolgimento: l'host ne fa le differenze).
    pub rx_bytes: u32,
    /// Frame completi consegnati al decoder.
    pub frames_ok: u32,
    /// Frame persi per cause di rete (scaduti, troppo vecchi, troppi in volo). Non include quelli scartati in attesa del
    /// keyframe: sono la conseguenza di una perdita già contata.
    pub frames_lost: u32,
    /// Frame recuperati con la parità.
    pub frames_fec: u32,
    /// Ritardo di coda stimato: RTT recente meno RTT minimo (µs); [`QUEUE_DELAY_UNKNOWN`] se non misurabile.
    pub queue_delay_us: u32,
    /// RTT recente (µs); 0 se non misurato.
    pub rtt_us: u32,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            rx_bytes: 0,
            frames_ok: 0,
            frames_lost: 0,
            frames_fec: 0,
            queue_delay_us: QUEUE_DELAY_UNKNOWN,
            rtt_us: 0,
        }
    }
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Lettore con controllo dei limiti: ogni lettura fallita dà `Truncated`.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Truncated)?;
        let s = self.buf.get(self.pos..end).ok_or(DecodeError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
    fn arr16(&mut self) -> Result<[u8; 16], DecodeError> {
        let b = self.take(16)?;
        let mut a = [0u8; 16];
        a.copy_from_slice(b);
        Ok(a)
    }
    fn rest(&mut self) -> &'a [u8] {
        let s = &self.buf[self.pos..];
        self.pos = self.buf.len();
        s
    }
    fn done(&self) -> bool {
        self.pos == self.buf.len()
    }
}

impl Message<'_> {
    /// Tipo del messaggio.
    pub fn kind(&self) -> Kind {
        match self {
            Message::Hello { .. } => Kind::Hello,
            Message::HelloAck { .. } => Kind::HelloAck,
            Message::Ping { .. } => Kind::Ping,
            Message::Pong { .. } => Kind::Pong,
            Message::Video(_) => Kind::Video,
            Message::Nack { .. } => Kind::Nack,
            Message::KeyframeRequest { .. } => Kind::KeyframeRequest,
            Message::Touch { .. } => Kind::Touch,
            Message::Key { .. } => Kind::Key,
            Message::Stats(_) => Kind::Stats,
            Message::Close { .. } => Kind::Close,
        }
    }

    /// Scrive il datagramma (intestazione + messaggio) in coda a `out`.
    ///
    /// Restituisce `Err(TooLarge)` se supera [`MAX_DATAGRAM`] (e in tal caso `out` non viene modificato).
    pub fn encode_into(&self, hdr: Header, out: &mut Vec<u8>) -> Result<(), DecodeError> {
        let start = out.len();
        out.push(MAGIC);
        out.push(VERSION);
        out.push(self.kind() as u8);
        out.push(0);
        put_u32(out, hdr.conn_id);
        put_u32(out, hdr.seq);
        match self {
            Message::Hello { role, proto_min, proto_max, caps, nonce } => {
                out.push(*role as u8);
                out.push(*proto_min);
                out.push(*proto_max);
                put_u32(out, *caps);
                out.extend_from_slice(nonce);
            }
            Message::HelloAck { proto, caps, nonce } => {
                out.push(*proto);
                put_u32(out, *caps);
                out.extend_from_slice(nonce);
            }
            Message::Ping { id, t_client_us } => {
                put_u32(out, *id);
                put_u64(out, *t_client_us);
            }
            Message::Pong { id, t_client_us, t_host_rx_us, t_host_tx_us } => {
                put_u32(out, *id);
                put_u64(out, *t_client_us);
                put_u64(out, *t_host_rx_us);
                put_u64(out, *t_host_tx_us);
            }
            Message::Video(v) => {
                put_u32(out, v.frame_id);
                put_u16(out, v.frag_index);
                put_u16(out, v.frag_count);
                out.push(v.flags);
                out.push(v.codec as u8);
                put_u16(out, v.frag_size);
                put_u32(out, v.total_len);
                put_u64(out, v.capture_ts_us);
                put_u16(out, v.fec_group);
                put_u16(out, v.payload.len() as u16);
                out.extend_from_slice(v.payload);
            }
            Message::Nack { frame_id, missing } => {
                put_u32(out, *frame_id);
                let n = missing.len().min(MAX_NACK);
                out.push(n as u8);
                for m in missing.iter().take(n) {
                    put_u16(out, *m);
                }
            }
            Message::KeyframeRequest { reason, last_good_frame } => {
                out.push(*reason);
                put_u32(out, *last_good_frame);
            }
            Message::Touch { state_seq, t_client_us, fingers } => {
                put_u32(out, *state_seq);
                put_u64(out, *t_client_us);
                let n = fingers.len().min(MAX_FINGERS);
                out.push(n as u8);
                for f in fingers.iter().take(n) {
                    out.push(f.id);
                    put_u16(out, f.x);
                    put_u16(out, f.y);
                    out.push(f.pressure);
                }
            }
            Message::Key { key_seq, t_client_us, code, down } => {
                put_u32(out, *key_seq);
                put_u64(out, *t_client_us);
                put_u16(out, *code);
                out.push(u8::from(*down));
            }
            Message::Stats(s) => {
                put_u32(out, s.rx_bytes);
                put_u32(out, s.frames_ok);
                put_u32(out, s.frames_lost);
                put_u32(out, s.frames_fec);
                put_u32(out, s.queue_delay_us);
                put_u32(out, s.rtt_us);
            }
            Message::Close { reason } => out.push(*reason),
        }
        if out.len() - start > MAX_DATAGRAM {
            out.truncate(start);
            return Err(DecodeError::TooLarge);
        }
        Ok(())
    }

    /// Comodità: il datagramma come nuovo vettore.
    pub fn encode(&self, hdr: Header) -> Result<Vec<u8>, DecodeError> {
        let mut v = Vec::with_capacity(HEADER_LEN + 64);
        self.encode_into(hdr, &mut v)?;
        Ok(v)
    }
}

/// Legge un datagramma completo. Nessun panico per qualsiasi ingresso.
pub fn decode(buf: &[u8]) -> Result<(Header, Message<'_>), DecodeError> {
    if buf.len() > MAX_DATAGRAM {
        return Err(DecodeError::TooLarge);
    }
    let mut r = Reader::new(buf);
    if r.u8()? != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let version = r.u8()?;
    if version != VERSION {
        return Err(DecodeError::BadVersion(version));
    }
    let kind_raw = r.u8()?;
    let kind = Kind::from_u8(kind_raw).ok_or(DecodeError::BadKind(kind_raw))?;
    let flags = r.u8()?;
    if flags != 0 {
        return Err(DecodeError::BadField("flags"));
    }
    let hdr = Header { conn_id: r.u32()?, seq: r.u32()? };

    let msg = match kind {
        Kind::Hello => {
            let role = match r.u8()? {
                1 => Role::Host,
                2 => Role::Client,
                _ => return Err(DecodeError::BadField("role")),
            };
            Message::Hello { role, proto_min: r.u8()?, proto_max: r.u8()?, caps: r.u32()?, nonce: r.arr16()? }
        }
        Kind::HelloAck => Message::HelloAck { proto: r.u8()?, caps: r.u32()?, nonce: r.arr16()? },
        Kind::Ping => Message::Ping { id: r.u32()?, t_client_us: r.u64()? },
        Kind::Pong => {
            Message::Pong { id: r.u32()?, t_client_us: r.u64()?, t_host_rx_us: r.u64()?, t_host_tx_us: r.u64()? }
        }
        Kind::Video => {
            let frame_id = r.u32()?;
            let frag_index = r.u16()?;
            let frag_count = r.u16()?;
            let flags = r.u8()?;
            let codec = match r.u8()? {
                1 => Codec::H264,
                2 => Codec::H265,
                _ => return Err(DecodeError::BadField("codec")),
            };
            let frag_size = r.u16()?;
            let total_len = r.u32()?;
            let capture_ts_us = r.u64()?;
            let fec_group = r.u16()?;
            let plen = r.u16()? as usize;
            let payload = r.take(plen)?;
            if frag_count == 0 || frag_size == 0 || usize::from(frag_size) > MAX_VIDEO_PAYLOAD {
                return Err(DecodeError::BadField("frag_count/frag_size"));
            }
            let parity = flags & video_flags::PARITY != 0;
            if parity {
                let groups = parity_groups(frag_count, fec_group);
                if fec_group == 0
                    || frag_index < frag_count
                    || u32::from(frag_index) >= u32::from(frag_count) + u32::from(groups)
                {
                    return Err(DecodeError::BadField("frag_index"));
                }
            } else if frag_index >= frag_count {
                return Err(DecodeError::BadField("frag_index"));
            }
            if u64::from(frag_count) * u64::from(frag_size) < u64::from(total_len) {
                return Err(DecodeError::BadField("total_len"));
            }
            Message::Video(VideoFragment {
                frame_id,
                frag_index,
                frag_count,
                flags,
                codec,
                frag_size,
                total_len,
                capture_ts_us,
                fec_group,
                payload,
            })
        }
        Kind::Nack => {
            let frame_id = r.u32()?;
            let n = usize::from(r.u8()?);
            if n > MAX_NACK {
                return Err(DecodeError::BadField("nack count"));
            }
            let mut missing = Vec::with_capacity(n);
            for _ in 0..n {
                missing.push(r.u16()?);
            }
            Message::Nack { frame_id, missing }
        }
        Kind::KeyframeRequest => Message::KeyframeRequest { reason: r.u8()?, last_good_frame: r.u32()? },
        Kind::Touch => {
            let state_seq = r.u32()?;
            let t_client_us = r.u64()?;
            let n = usize::from(r.u8()?);
            if n > MAX_FINGERS {
                return Err(DecodeError::BadField("finger count"));
            }
            let mut fingers = Vec::with_capacity(n);
            for _ in 0..n {
                fingers.push(Finger { id: r.u8()?, x: r.u16()?, y: r.u16()?, pressure: r.u8()? });
            }
            Message::Touch { state_seq, t_client_us, fingers }
        }
        Kind::Key => {
            let key_seq = r.u32()?;
            let t_client_us = r.u64()?;
            let code = r.u16()?;
            let down = match r.u8()? {
                0 => false,
                1 => true,
                _ => return Err(DecodeError::BadField("down")),
            };
            Message::Key { key_seq, t_client_us, code, down }
        }
        Kind::Stats => Message::Stats(Stats {
            rx_bytes: r.u32()?,
            frames_ok: r.u32()?,
            frames_lost: r.u32()?,
            frames_fec: r.u32()?,
            queue_delay_us: r.u32()?,
            rtt_us: r.u32()?,
        }),
        Kind::Close => Message::Close { reason: r.u8()? },
    };
    // Dopo un messaggio a lunghezza fissa non devono restare byte (tranne il video, che ha già letto il suo payload).
    if !r.done() {
        let _ = r.rest();
        return Err(DecodeError::BadField("byte in eccesso"));
    }
    Ok((hdr, msg))
}

/// Calcola la lunghezza del frammento dati `index` di un frame di `total_len` byte diviso in pezzi da `frag_size`.
pub fn fragment_len(total_len: u32, frag_size: u16, index: u16) -> usize {
    let start = usize::from(index) * usize::from(frag_size);
    let total = total_len as usize;
    if start >= total {
        0
    } else {
        usize::from(frag_size).min(total - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn roundtrip(m: &Message<'_>) {
        let hdr = Header { conn_id: 0xDEAD_BEEF, seq: 42 };
        let bytes = m.encode(hdr).expect("codifica");
        let (h2, m2) = decode(&bytes).expect("decodifica");
        assert_eq!(h2, hdr);
        assert_eq!(&m2, m);
    }

    #[test]
    fn hello_roundtrip() {
        roundtrip(&Message::Hello {
            role: Role::Client,
            proto_min: 0,
            proto_max: 3,
            caps: 0xA5A5_0001,
            nonce: [7; 16],
        });
        roundtrip(&Message::HelloAck { proto: 0, caps: 1, nonce: [9; 16] });
    }

    #[test]
    fn ping_pong_roundtrip() {
        roundtrip(&Message::Ping { id: 1, t_client_us: u64::MAX });
        roundtrip(&Message::Pong { id: 2, t_client_us: 5, t_host_rx_us: 6, t_host_tx_us: 7 });
    }

    #[test]
    fn video_roundtrip_and_limits() {
        let payload = vec![0xAB; MAX_VIDEO_PAYLOAD];
        let v = VideoFragment {
            frame_id: 7,
            frag_index: 0,
            frag_count: 3,
            flags: video_flags::KEY,
            codec: Codec::H265,
            frag_size: MAX_VIDEO_PAYLOAD as u16,
            total_len: (MAX_VIDEO_PAYLOAD * 2 + 5) as u32,
            capture_ts_us: 123_456_789,
            fec_group: 2,
            payload: &payload,
        };
        let bytes = Message::Video(v.clone()).encode(Header { conn_id: 1, seq: 2 }).unwrap();
        assert_eq!(bytes.len(), MAX_DATAGRAM, "un frammento pieno riempie esattamente il datagramma");
        let (_, m) = decode(&bytes).unwrap();
        assert_eq!(m, Message::Video(v));
        // un payload di un byte in più non entra
        let too_big = vec![0u8; MAX_VIDEO_PAYLOAD + 1];
        let v2 = VideoFragment {
            frame_id: 0,
            frag_index: 0,
            frag_count: 1,
            flags: 0,
            codec: Codec::H264,
            frag_size: MAX_VIDEO_PAYLOAD as u16,
            total_len: 10,
            capture_ts_us: 0,
            fec_group: 0,
            payload: &too_big,
        };
        let mut out = vec![1, 2, 3];
        assert_eq!(Message::Video(v2).encode_into(Header { conn_id: 0, seq: 0 }, &mut out), Err(DecodeError::TooLarge));
        assert_eq!(out, vec![1, 2, 3], "in caso di errore il buffer non cambia");
    }

    #[test]
    fn touch_state_roundtrip() {
        let fingers =
            vec![Finger { id: 0, x: 0, y: 65535, pressure: 255 }, Finger { id: 3, x: 32768, y: 100, pressure: 0 }];
        roundtrip(&Message::Touch { state_seq: 99, t_client_us: 1_000_000, fingers });
        roundtrip(&Message::Touch { state_seq: 100, t_client_us: 1_004_166, fingers: vec![] });
    }

    #[test]
    fn other_messages_roundtrip() {
        roundtrip(&Message::Nack { frame_id: 5, missing: vec![0, 4, 9] });
        roundtrip(&Message::KeyframeRequest { reason: 1, last_good_frame: 77 });
        roundtrip(&Message::Key { key_seq: 3, t_client_us: 9, code: 0x04, down: true });
        roundtrip(&Message::Stats(Stats {
            rx_bytes: 1,
            frames_ok: 2,
            frames_lost: 3,
            frames_fec: 4,
            queue_delay_us: 5,
            rtt_us: 6,
        }));
        roundtrip(&Message::Stats(Stats::default()));
        roundtrip(&Message::Close { reason: 0 });
    }

    #[test]
    fn rejects_garbage_precisely() {
        assert_eq!(decode(&[]), Err(DecodeError::Truncated));
        assert_eq!(decode(&[0x51, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0]), Err(DecodeError::BadMagic));
        assert_eq!(decode(&[MAGIC, 9, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0]), Err(DecodeError::BadVersion(9)));
        assert_eq!(decode(&[MAGIC, 0, 200, 0, 0, 0, 0, 0, 0, 0, 0, 0]), Err(DecodeError::BadKind(200)));
        assert_eq!(decode(&[MAGIC, 0, 3, 1, 0, 0, 0, 0, 0, 0, 0, 0]), Err(DecodeError::BadField("flags")));
        assert_eq!(decode(&vec![MAGIC; MAX_DATAGRAM + 1]), Err(DecodeError::TooLarge));
        // Ping troncato a metà
        let mut p = Message::Ping { id: 1, t_client_us: 2 }.encode(Header { conn_id: 0, seq: 0 }).unwrap();
        p.truncate(p.len() - 3);
        assert_eq!(decode(&p), Err(DecodeError::Truncated));
        // byte in eccesso
        let mut p = Message::Close { reason: 0 }.encode(Header { conn_id: 0, seq: 0 }).unwrap();
        p.push(0);
        assert_eq!(decode(&p), Err(DecodeError::BadField("byte in eccesso")));
    }

    #[test]
    fn video_field_validation() {
        let payload = [1u8; 10];
        let mk = |frag_index, frag_count, flags, frag_size, total_len| {
            Message::Video(VideoFragment {
                frame_id: 1,
                frag_index,
                frag_count,
                flags,
                codec: Codec::H264,
                frag_size,
                total_len,
                capture_ts_us: 0,
                fec_group: 4,
                payload: &payload,
            })
            .encode(Header { conn_id: 0, seq: 0 })
            .unwrap()
        };
        assert!(decode(&mk(0, 1, 0, 100, 10)).is_ok());
        assert_eq!(decode(&mk(1, 1, 0, 100, 10)), Err(DecodeError::BadField("frag_index")));
        assert_eq!(decode(&mk(0, 0, 0, 100, 10)), Err(DecodeError::BadField("frag_count/frag_size")));
        assert_eq!(decode(&mk(0, 1, 0, 0, 10)), Err(DecodeError::BadField("frag_count/frag_size")));
        assert_eq!(decode(&mk(0, 1, 0, 100, 1000)), Err(DecodeError::BadField("total_len")));
        // la parità ha indice ≥ frag_count e < frag_count + gruppi (qui fec_group = 4, frag_count = 1 → 1 gruppo)
        assert!(decode(&mk(1, 1, video_flags::PARITY, 100, 10)).is_ok(), "parità del primo gruppo");
        assert_eq!(decode(&mk(0, 1, video_flags::PARITY, 100, 10)), Err(DecodeError::BadField("frag_index")));
        assert_eq!(
            decode(&mk(2, 1, video_flags::PARITY, 100, 10)),
            Err(DecodeError::BadField("frag_index")),
            "un solo gruppo"
        );
    }

    #[test]
    fn parity_group_count() {
        assert_eq!(parity_groups(10, 0), 0);
        assert_eq!(parity_groups(10, 4), 3);
        assert_eq!(parity_groups(8, 4), 2);
        assert_eq!(parity_groups(1, 8), 1);
    }

    #[test]
    fn fragment_len_matches_split() {
        assert_eq!(fragment_len(2500, 1000, 0), 1000);
        assert_eq!(fragment_len(2500, 1000, 2), 500);
        assert_eq!(fragment_len(2500, 1000, 3), 0);
        assert_eq!(fragment_len(0, 1000, 0), 0);
    }

    proptest! {
        /// Qualsiasi sequenza di byte dà un risultato (Ok o Err) senza panico.
        #[test]
        fn decode_never_panics(data in proptest::collection::vec(any::<u8>(), 0..1300)) {
            let _ = decode(&data);
        }

        /// Dati quasi validi (intestazione giusta, resto casuale) non fanno mai panico.
        #[test]
        fn decode_with_valid_header_never_panics(kind in 0u8..16, rest in proptest::collection::vec(any::<u8>(), 0..200)) {
            let mut d = vec![MAGIC, VERSION, kind, 0, 1, 2, 3, 4, 5, 6, 7, 8];
            d.extend_from_slice(&rest);
            let _ = decode(&d);
        }

        /// Ciò che si codifica si rilegge identico, per tocchi con un numero qualsiasi di dita.
        #[test]
        fn touch_roundtrip_prop(seq in any::<u32>(), t in any::<u64>(), fs in proptest::collection::vec((any::<u8>(), any::<u16>(), any::<u16>(), any::<u8>()), 0..=MAX_FINGERS)) {
            let fingers: Vec<Finger> = fs.into_iter().map(|(id, x, y, pressure)| Finger { id, x, y, pressure }).collect();
            roundtrip(&Message::Touch { state_seq: seq, t_client_us: t, fingers });
        }

        /// Frammenti video validi si rileggono identici.
        #[test]
        fn video_roundtrip_prop(frame_id in any::<u32>(), cnt in 1u16..200, frag_size in 1u16..=(MAX_VIDEO_PAYLOAD as u16), ts in any::<u64>(), len in 0usize..=MAX_VIDEO_PAYLOAD) {
            let payload = vec![0x5A; len];
            let total = u32::from(cnt) * u32::from(frag_size);
            let v = VideoFragment { frame_id, frag_index: cnt - 1, frag_count: cnt, flags: video_flags::KEY, codec: Codec::H264, frag_size, total_len: total, capture_ts_us: ts, fec_group: 3, payload: &payload };
            roundtrip(&Message::Video(v));
        }
    }
}
