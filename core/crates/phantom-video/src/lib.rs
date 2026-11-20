//! Video su datagrammi: frammentazione, parità XOR, riassemblaggio a bassa latenza, NACK e richieste di keyframe.
//!
//! Lato host ([`VideoSender`]): un frame codificato diventa una serie di datagrammi da al massimo
//! [`phantom_proto::MAX_DATAGRAM`] byte, con un frammento di parità XOR ogni `fec_group` frammenti dati; gli ultimi
//! frame restano in una cache per rispondere ai NACK.
//!
//! Lato client ([`VideoReceiver`]): i frammenti si raccolgono per frame, anche fuori ordine e duplicati; un frammento
//! mancante per gruppo si ricostruisce con la parità; per gli altri si chiede la ritrasmissione dopo un breve
//! ritardo. I frame escono **in ordine**, perché il decoder dipende dai precedenti: se un frame non arriva in tempo
//! si scartano i successivi fino al prossimo keyframe e si chiede un keyframe all'host. Un keyframe completo
//! «salta avanti»: rende obsoleto tutto ciò che lo precede, quindi si consegna subito.
//!
//! Il tempo è sempre passato dal chiamante (`now_us`): nessuna lettura dell'orologio, nessun thread, nessun sonno. È ciò
//! che rende il codice provabile in modo deterministico col simulatore di rete.

use phantom_proto::{
    self as proto, parity_groups, video_flags, Codec, Header, Message, VideoFragment, MAX_NACK, MAX_VIDEO_PAYLOAD,
};
use std::collections::{BTreeMap, VecDeque};

/// Frame codificato prodotto dall'encoder dell'host.
#[derive(Clone, Copy, Debug)]
pub struct EncodedFrame<'a> {
    /// Numero progressivo del frame.
    pub frame_id: u32,
    /// Codec.
    pub codec: Codec,
    /// `true` se è un keyframe (decodificabile da solo).
    pub key: bool,
    /// `true` se contiene i parametri del codec.
    pub config: bool,
    /// Istante di cattura sull'orologio dell'host (µs).
    pub capture_ts_us: u64,
    /// Dati codificati.
    pub data: &'a [u8],
}

/// Un frammento pronto da spedire (senza intestazione di sessione).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Piece {
    index: u16,
    flags: u8,
    payload: Vec<u8>,
}

/// Un frame già frammentato.
#[derive(Clone, Debug)]
pub struct FramePackets {
    frame_id: u32,
    codec: Codec,
    base_flags: u8,
    frag_count: u16,
    frag_size: u16,
    total_len: u32,
    capture_ts_us: u64,
    fec_group: u16,
    pieces: Vec<Piece>, // prima i dati, poi la parità
}

impl FramePackets {
    /// Numero di frammenti dati.
    pub fn data_fragments(&self) -> u16 {
        self.frag_count
    }
    /// Numero totale di datagrammi (dati + parità).
    pub fn len(&self) -> usize {
        self.pieces.len()
    }
    /// `true` se non ci sono frammenti (non succede mai: ogni frame ha almeno un frammento).
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }
    /// Il datagramma `i`-esimo (dati e poi parità), pronto da spedire.
    pub fn datagram(&self, i: usize, hdr: Header, retransmit: bool) -> Option<Vec<u8>> {
        let p = self.pieces.get(i)?;
        let mut flags = self.base_flags | p.flags;
        if retransmit {
            flags |= video_flags::RETRANSMIT;
        }
        let v = VideoFragment {
            frame_id: self.frame_id,
            frag_index: p.index,
            frag_count: self.frag_count,
            flags,
            codec: self.codec,
            frag_size: self.frag_size,
            total_len: self.total_len,
            capture_ts_us: self.capture_ts_us,
            fec_group: self.fec_group,
            payload: &p.payload,
        };
        Message::Video(v).encode(hdr).ok()
    }
}

/// Divide un frame in frammenti e calcola la parità XOR di ogni gruppo di `fec_group` frammenti (0 = nessuna parità).
pub fn fragment(frame: &EncodedFrame<'_>, frag_size: u16, fec_group: u16) -> FramePackets {
    let frag_size = frag_size.clamp(1, MAX_VIDEO_PAYLOAD as u16);
    let size = usize::from(frag_size);
    let n = frame.data.len().div_ceil(size).max(1);
    let frag_count = u16::try_from(n).unwrap_or(u16::MAX);
    let mut base_flags = 0;
    if frame.key {
        base_flags |= video_flags::KEY;
    }
    if frame.config {
        base_flags |= video_flags::CONFIG;
    }
    let mut pieces: Vec<Piece> = (0..usize::from(frag_count))
        .map(|i| {
            let start = (i * size).min(frame.data.len());
            let end = ((i + 1) * size).min(frame.data.len());
            Piece { index: i as u16, flags: 0, payload: frame.data[start..end].to_vec() }
        })
        .collect();
    for g in 0..parity_groups(frag_count, fec_group) {
        let first = usize::from(g) * usize::from(fec_group);
        let last = (first + usize::from(fec_group)).min(usize::from(frag_count));
        let mut parity = vec![0u8; size];
        for p in &pieces[first..last] {
            for (d, s) in parity.iter_mut().zip(&p.payload) {
                *d ^= s;
            }
        }
        pieces.push(Piece { index: frag_count + g, flags: video_flags::PARITY, payload: parity });
    }
    FramePackets {
        frame_id: frame.frame_id,
        codec: frame.codec,
        base_flags,
        frag_count,
        frag_size,
        total_len: frame.data.len() as u32,
        capture_ts_us: frame.capture_ts_us,
        fec_group,
        pieces,
    }
}

// ───────────────────────────────────────────────────────────── lato host

/// Lato host: frammenta, spedisce e risponde ai NACK.
///
/// Le ritrasmissioni hanno un **budget**: a collegamento saturo ogni NACK farebbe peggiorare la saturazione (una
/// tempesta di ritrasmissioni). Si concede al massimo il 50% della velocità di invio recente (e almeno 1 Mbps).
#[derive(Debug)]
pub struct VideoSender {
    conn_id: u32,
    seq: u32,
    /// Dimensione dei frammenti dati.
    pub frag_size: u16,
    /// Frammenti dati per gruppo di parità (0 = nessuna parità).
    pub fec_group: u16,
    cache: VecDeque<FramePackets>,
    cache_frames: usize,
    /// Frammenti ritrasmessi finora.
    pub retransmitted: u64,
    /// Frammenti la cui ritrasmissione è stata negata per il budget.
    pub retransmit_denied: u64,
    last_frame_us: Option<u64>,
    rate_bytes_per_s: f64,
    tokens: f64,
    last_refill_us: u64,
}

const RETX_FRACTION: f64 = 0.5;
const RETX_MIN_BYTES_PER_S: f64 = 125_000.0;
const RETX_BUCKET_BYTES: f64 = 65_536.0;

impl VideoSender {
    /// Crea il lato host. `cache_frames` = quanti frame recenti tenere per i NACK.
    pub fn new(conn_id: u32, frag_size: u16, fec_group: u16, cache_frames: usize) -> Self {
        VideoSender {
            conn_id,
            seq: 0,
            frag_size,
            fec_group,
            cache: VecDeque::new(),
            cache_frames: cache_frames.max(1),
            retransmitted: 0,
            retransmit_denied: 0,
            last_frame_us: None,
            rate_bytes_per_s: 0.0,
            tokens: RETX_BUCKET_BYTES / 2.0,
            last_refill_us: 0,
        }
    }

    fn header(&mut self) -> Header {
        let h = Header { conn_id: self.conn_id, seq: self.seq };
        self.seq = self.seq.wrapping_add(1);
        h
    }

    /// Frammenta un frame e restituisce i datagrammi da spedire, nell'ordine (dati poi parità).
    pub fn send_frame(&mut self, frame: &EncodedFrame<'_>) -> Vec<Vec<u8>> {
        let fp = fragment(frame, self.frag_size, self.fec_group);
        let out: Vec<Vec<u8>> = (0..fp.len()).filter_map(|i| fp.datagram(i, self.header(), false)).collect();
        // velocità di invio recente (media mobile), per dimensionare il budget delle ritrasmissioni
        if let Some(last) = self.last_frame_us {
            let dt = frame.capture_ts_us.saturating_sub(last).max(1) as f64 / 1e6;
            let inst = out.iter().map(Vec::len).sum::<usize>() as f64 / dt.max(0.001);
            self.rate_bytes_per_s = if self.rate_bytes_per_s == 0.0 {
                inst
            } else {
                self.rate_bytes_per_s * 0.98 + inst.min(self.rate_bytes_per_s * 20.0) * 0.02
            };
        }
        self.last_frame_us = Some(frame.capture_ts_us);
        if self.cache.len() == self.cache_frames {
            self.cache.pop_front();
        }
        self.cache.push_back(fp);
        out
    }

    /// Risponde a un NACK: i datagrammi richiesti (lista vuota = tutto il frame, parità esclusa), nei limiti del
    /// budget. `now_us` è l'orologio dell'host. Frame non più in cache: nessuna risposta.
    pub fn on_nack(&mut self, now_us: u64, frame_id: u32, missing: &[u16]) -> Vec<Vec<u8>> {
        let dt = now_us.saturating_sub(self.last_refill_us) as f64 / 1e6;
        self.last_refill_us = now_us;
        let rate = (self.rate_bytes_per_s * RETX_FRACTION).max(RETX_MIN_BYTES_PER_S);
        self.tokens = (self.tokens + rate * dt.min(1.0)).min(RETX_BUCKET_BYTES);
        let Some(idx) = self.cache.iter().position(|f| f.frame_id == frame_id) else { return Vec::new() };
        let wanted: Vec<usize> = if missing.is_empty() {
            (0..usize::from(self.cache[idx].frag_count)).collect()
        } else {
            missing.iter().map(|m| usize::from(*m)).collect()
        };
        let mut out = Vec::new();
        for i in wanted {
            let hdr = self.header();
            if let Some(d) = self.cache[idx].datagram(i, hdr, true) {
                if self.tokens < d.len() as f64 {
                    self.retransmit_denied += 1;
                    continue;
                }
                self.tokens -= d.len() as f64;
                self.retransmitted += 1;
                out.push(d);
            }
        }
        out
    }
}

// ─────────────────────────────────────────────────────────── lato client

/// Impostazioni del lato client.
#[derive(Clone, Debug)]
pub struct ReceiverConfig {
    /// Dopo quanto dal primo frammento di un frame incompleto si chiede la ritrasmissione (µs).
    pub nack_delay_us: u64,
    /// Intervallo tra due richieste per lo stesso frame (µs).
    pub nack_retry_us: u64,
    /// Numero massimo di richieste per frame.
    pub max_nacks: u8,
    /// Ogni quanto si ripete la richiesta di keyframe finché ne manca uno (µs; almeno 2 RTT): la richiesta o il
    /// keyframe stesso possono andare persi, e senza ripetizione l'immagine resterebbe ferma.
    pub key_retry_us: u64,
    /// Un frame incompleto senza nuovi frammenti da tanto tempo è dato per perso (µs).
    pub max_frame_age_us: u64,
    /// Età massima assoluta dal primo frammento, anche se i frammenti continuano ad arrivare (µs): a collegamento
    /// lento un keyframe grosso impiega tempo ma non deve bloccare per sempre.
    pub max_frame_total_us: u64,
    /// Frame in volo al massimo (oltre, si scartano i più vecchi).
    pub max_in_flight: usize,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        ReceiverConfig {
            nack_delay_us: 3_000,
            nack_retry_us: 8_000,
            max_nacks: 3,
            key_retry_us: 500_000,
            max_frame_age_us: 60_000,
            max_frame_total_us: 400_000,
            max_in_flight: 64,
        }
    }
}

/// Un frame completo, pronto per il decoder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledFrame {
    /// Numero del frame.
    pub frame_id: u32,
    /// Codec.
    pub codec: Codec,
    /// Keyframe.
    pub key: bool,
    /// Contiene i parametri del codec.
    pub config: bool,
    /// Istante di cattura sull'orologio dell'host (µs).
    pub capture_ts_us: u64,
    /// Dati codificati.
    pub data: Vec<u8>,
    /// Arrivo del primo frammento (orologio client, µs).
    pub first_rx_us: u64,
    /// Istante in cui il frame è diventato completo (orologio client, µs).
    pub complete_us: u64,
    /// Almeno un frammento è stato ricostruito con la parità.
    pub recovered_fec: bool,
    /// Almeno un frammento è arrivato per ritrasmissione.
    pub retransmitted: bool,
}

/// Perché un frame è stato scartato.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// Non è diventato completo entro `max_frame_age_us`.
    Timeout,
    /// È arrivato in attesa di un keyframe: senza i frame precedenti il decoder lo mostrerebbe sbagliato.
    WaitingKeyframe,
    /// Reso obsoleto da un keyframe arrivato dopo.
    Stale,
    /// Troppi frame in volo.
    Overflow,
}

/// Ciò che il ricevitore chiede di fare al chiamante.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// Un frame da decodificare, in ordine.
    Frame(AssembledFrame),
    /// Spedire un NACK all'host (lista vuota = tutto il frame).
    Nack {
        /// Frame interessato.
        frame_id: u32,
        /// Frammenti dati mancanti.
        missing: Vec<u16>,
    },
    /// Spedire una richiesta di keyframe all'host.
    RequestKeyframe {
        /// Ultimo frame consegnato.
        last_good_frame: u32,
        /// 0 = perdita, 1 = avvio.
        reason: u8,
    },
    /// Un frame è stato scartato (per le statistiche).
    Dropped {
        /// Frame scartato.
        frame_id: u32,
        /// Motivo.
        reason: DropReason,
    },
}

/// Contatori del ricevitore.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReceiverStats {
    /// Frame consegnati.
    pub frames_ok: u64,
    /// Frame scartati per timeout.
    pub dropped_timeout: u64,
    /// Frame scartati in attesa di keyframe.
    pub dropped_waiting_key: u64,
    /// Frame resi obsoleti da un keyframe.
    pub dropped_stale: u64,
    /// Frame scartati per eccesso di frame in volo.
    pub dropped_overflow: u64,
    /// Frame in cui almeno un frammento è stato ricostruito con la parità.
    pub fec_recovered_frames: u64,
    /// NACK emessi.
    pub nacks: u64,
    /// Richieste di keyframe emesse (ripetizioni comprese).
    pub keyframe_requests: u64,
    /// Di cui ripetizioni, perché il keyframe atteso non era arrivato.
    pub key_retries: u64,
    /// Frammenti arrivati per un frame già consegnato o scartato.
    pub late_fragments: u64,
    /// Frammenti già ricevuti.
    pub duplicate_fragments: u64,
    /// Frammenti ricevuti in totale.
    pub fragments: u64,
    /// Byte di payload di tutti i frammenti ricevuti (dati, parità, ritrasmissioni, duplicati): con il tempo dà la
    /// velocità con cui il collegamento sta davvero consegnando.
    pub bytes: u64,
}

#[derive(Debug)]
struct FrameState {
    codec: Codec,
    flags: u8,
    capture_ts_us: u64,
    frag_count: u16,
    frag_size: u16,
    total_len: u32,
    fec_group: u16,
    data: Vec<Option<Vec<u8>>>,
    parity: Vec<Option<Vec<u8>>>,
    have: u16,
    highest_idx: Option<u16>,
    first_rx_us: u64,
    last_rx_us: u64,
    nacks: u8,
    last_nack_us: u64,
    recovered_fec: bool,
    retransmitted: bool,
    complete_us: Option<u64>,
}

impl FrameState {
    fn is_complete(&self) -> bool {
        self.have == self.frag_count
    }
    fn missing(&self) -> Vec<u16> {
        self.data.iter().enumerate().filter(|(_, d)| d.is_none()).map(|(i, _)| i as u16).collect()
    }
    fn is_key(&self) -> bool {
        self.flags & video_flags::KEY != 0
    }

    /// Ricostruisce un frammento mancante per gruppo se la parità del gruppo c'è ed è l'unico mancante.
    fn try_fec(&mut self) -> bool {
        let mut recovered = false;
        for g in 0..self.parity.len() {
            let first = g * usize::from(self.fec_group);
            let last = (first + usize::from(self.fec_group)).min(usize::from(self.frag_count));
            let missing: Vec<usize> = (first..last).filter(|&i| self.data[i].is_none()).collect();
            if missing.len() != 1 {
                continue;
            }
            let Some(par) = self.parity[g].as_ref() else { continue };
            let mut buf = par.clone();
            for d in self.data[first..last].iter().flatten() {
                for (b, s) in buf.iter_mut().zip(d) {
                    *b ^= s;
                }
            }
            let len = proto::fragment_len(self.total_len, self.frag_size, missing[0] as u16);
            buf.truncate(len);
            self.data[missing[0]] = Some(buf);
            self.have += 1;
            self.recovered_fec = true;
            recovered = true;
        }
        recovered
    }
}

fn newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < (1 << 31)
}

/// Lato client: raccoglie i frammenti e produce frame in ordine.
#[derive(Debug)]
pub struct VideoReceiver {
    cfg: ReceiverConfig,
    frames: BTreeMap<u32, FrameState>,
    next_deliver: Option<u32>,
    highest_seen: Option<u32>,
    need_key: bool,
    last_good: u32,
    gap_since: BTreeMap<u32, u64>, // frame mai visti (persi per intero) con l'istante in cui ci si è accorti
    gap_nacks: BTreeMap<u32, (u8, u64)>,
    stats: ReceiverStats,
    asked_start_key: bool,
    last_key_request_us: Option<u64>,
    rtt_us: u64,
}

impl VideoReceiver {
    /// Crea il ricevitore.
    pub fn new(cfg: ReceiverConfig) -> Self {
        VideoReceiver {
            cfg,
            frames: BTreeMap::new(),
            next_deliver: None,
            highest_seen: None,
            need_key: true,
            last_good: 0,
            gap_since: BTreeMap::new(),
            gap_nacks: BTreeMap::new(),
            stats: ReceiverStats::default(),
            asked_start_key: false,
            last_key_request_us: None,
            rtt_us: 0,
        }
    }

    /// Comunica il tempo di andata e ritorno stimato: le richieste di ritrasmissione per lo stesso frammento non
    /// vengono ripetute prima di 1,5 RTT (la risposta è ancora in viaggio).
    pub fn set_rtt_us(&mut self, rtt_us: u64) {
        self.rtt_us = rtt_us;
    }

    /// Contatori.
    pub fn stats(&self) -> ReceiverStats {
        self.stats
    }

    /// Riceve un frammento (già decodificato dal protocollo).
    pub fn on_fragment(&mut self, now_us: u64, v: &VideoFragment<'_>) -> Vec<Output> {
        let mut out = Vec::new();
        self.stats.fragments += 1;
        self.stats.bytes += v.payload.len() as u64;
        if self.next_deliver.is_none() {
            self.next_deliver = Some(v.frame_id);
            if !self.asked_start_key {
                self.asked_start_key = true;
                self.stats.keyframe_requests += 1;
                self.last_key_request_us = Some(now_us);
                out.push(Output::RequestKeyframe { last_good_frame: 0, reason: 1 });
            }
        }
        let next = self.next_deliver.unwrap_or(v.frame_id);
        // frammento di un frame già consegnato o scartato
        if newer(next, v.frame_id) {
            self.stats.late_fragments += 1;
            return out;
        }
        // frame mai visti tra l'ultimo noto e questo: persi per intero
        if let Some(hs) = self.highest_seen {
            if newer(v.frame_id, hs) {
                let mut id = hs.wrapping_add(1);
                while newer(v.frame_id, id) {
                    if !self.frames.contains_key(&id) {
                        self.gap_since.entry(id).or_insert(now_us);
                    }
                    id = id.wrapping_add(1);
                    if self.gap_since.len() > self.cfg.max_in_flight {
                        break;
                    }
                }
            }
        }
        if self.highest_seen.is_none_or(|hs| newer(v.frame_id, hs)) {
            self.highest_seen = Some(v.frame_id);
        }
        self.gap_since.remove(&v.frame_id);
        self.gap_nacks.remove(&v.frame_id);

        let st = self.frames.entry(v.frame_id).or_insert_with(|| FrameState {
            codec: v.codec,
            flags: 0,
            capture_ts_us: v.capture_ts_us,
            frag_count: v.frag_count,
            frag_size: v.frag_size,
            total_len: v.total_len,
            fec_group: v.fec_group,
            data: vec![None; usize::from(v.frag_count)],
            parity: vec![None; usize::from(parity_groups(v.frag_count, v.fec_group))],
            have: 0,
            highest_idx: None,
            first_rx_us: now_us,
            last_rx_us: now_us,
            nacks: 0,
            last_nack_us: 0,
            recovered_fec: false,
            retransmitted: false,
            complete_us: None,
        });
        // un frammento incoerente con gli altri dello stesso frame si ignora
        if st.frag_count != v.frag_count
            || st.frag_size != v.frag_size
            || st.total_len != v.total_len
            || st.fec_group != v.fec_group
        {
            return out;
        }
        st.flags |= v.flags & (video_flags::KEY | video_flags::CONFIG);
        st.last_rx_us = now_us;
        if v.flags & video_flags::RETRANSMIT != 0 {
            st.retransmitted = true;
        }
        if v.is_parity() {
            let g = usize::from(v.frag_index - v.frag_count);
            if st.parity[g].is_some() {
                self.stats.duplicate_fragments += 1;
            } else if v.payload.len() == usize::from(st.frag_size) {
                st.parity[g] = Some(v.payload.to_vec());
            }
        } else {
            let i = usize::from(v.frag_index);
            if st.data[i].is_some() {
                self.stats.duplicate_fragments += 1;
            } else if v.payload.len() == proto::fragment_len(st.total_len, st.frag_size, v.frag_index) {
                st.data[i] = Some(v.payload.to_vec());
                st.have += 1;
                st.highest_idx = Some(st.highest_idx.map_or(v.frag_index, |h| h.max(v.frag_index)));
            }
        }
        if !st.is_complete() {
            st.try_fec();
        }
        if st.is_complete() && st.complete_us.is_none() {
            st.complete_us = Some(now_us);
            if st.recovered_fec {
                self.stats.fec_recovered_frames += 1;
            }
        }
        self.deliver(now_us, &mut out);
        self.enforce_limits(now_us, &mut out);
        out
    }

    /// Da chiamare spesso (ogni ms o a ogni giro del ciclo): scade i frame, emette NACK.
    pub fn poll(&mut self, now_us: u64) -> Vec<Output> {
        let mut out = Vec::new();
        // NACK dei frammenti mancanti
        let retry = self.cfg.nack_retry_us.max(self.rtt_us.saturating_mul(3) / 2);
        let ids: Vec<u32> = self.frames.keys().copied().collect();
        for id in ids {
            let Some(st) = self.frames.get_mut(&id) else { continue };
            if st.is_complete() {
                continue;
            }
            let age = now_us.saturating_sub(st.first_rx_us);
            let idle = now_us.saturating_sub(st.last_rx_us);
            let due = if st.nacks == 0 {
                age >= self.cfg.nack_delay_us
            } else {
                now_us.saturating_sub(st.last_nack_us) >= retry
            };
            if !due || st.nacks >= self.cfg.max_nacks {
                continue;
            }
            // «Buchi» = mancanti con indice minore dell'ultimo arrivato: sono davvero persi o riordinati. I mancanti in
            // coda possono essere ancora in viaggio: si chiedono solo se il frame non riceve nulla da un po'.
            let all = st.missing();
            let wanted: Vec<u16> = if idle >= self.cfg.nack_delay_us {
                all
            } else {
                all.into_iter().filter(|m| st.highest_idx.is_some_and(|h| *m < h)).collect()
            };
            if wanted.is_empty() {
                continue;
            }
            st.nacks += 1;
            st.last_nack_us = now_us;
            for chunk in wanted.chunks(MAX_NACK) {
                self.stats.nacks += 1;
                out.push(Output::Nack { frame_id: id, missing: chunk.to_vec() });
            }
        }
        // frame persi per intero: si chiede di rimandarli (lista vuota)
        let gaps: Vec<(u32, u64)> = self.gap_since.iter().map(|(k, v)| (*k, *v)).collect();
        for (id, since) in gaps {
            let (n, last) = self.gap_nacks.get(&id).copied().unwrap_or((0, 0));
            let due = if n == 0 {
                now_us.saturating_sub(since) >= self.cfg.nack_delay_us
            } else {
                now_us.saturating_sub(last) >= retry
            };
            if due && n < self.cfg.max_nacks {
                self.gap_nacks.insert(id, (n + 1, now_us));
                self.stats.nacks += 1;
                out.push(Output::Nack { frame_id: id, missing: Vec::new() });
            }
        }
        self.deliver(now_us, &mut out);
        self.enforce_limits(now_us, &mut out);
        // in attesa di un keyframe che non arriva: la richiesta (o il keyframe) si è persa, si ripete
        if self.need_key && self.next_deliver.is_some() {
            let every = self.cfg.key_retry_us.max(self.rtt_us.saturating_mul(2));
            if self.last_key_request_us.is_none_or(|t| now_us.saturating_sub(t) >= every) {
                self.stats.key_retries += 1;
                self.request_keyframe(now_us, 3, &mut out);
            }
        }
        out
    }

    fn drop_frame(&mut self, id: u32, reason: DropReason, out: &mut Vec<Output>) {
        self.frames.remove(&id);
        self.gap_since.remove(&id);
        self.gap_nacks.remove(&id);
        match reason {
            DropReason::Timeout => self.stats.dropped_timeout += 1,
            DropReason::WaitingKeyframe => self.stats.dropped_waiting_key += 1,
            DropReason::Stale => self.stats.dropped_stale += 1,
            DropReason::Overflow => self.stats.dropped_overflow += 1,
        }
        out.push(Output::Dropped { frame_id: id, reason });
    }

    /// Il decoder dell'app ha perso un frame già consegnato (coda del renderer piena, errore di decodifica): da qui in
    /// poi i frame non-key sarebbero corrotti, quindi si scartano fino al prossimo keyframe e se ne chiede uno
    /// (motivo 2, richiesta esplicita). Se una richiesta è partita da meno di `key_retry_us` non se ne manda un'altra:
    /// ci pensa già la ripetizione automatica in [`VideoReceiver::poll`].
    pub fn request_resync(&mut self, now_us: u64) -> Vec<Output> {
        let mut out = Vec::new();
        let recent = self.last_key_request_us.is_some_and(|t| now_us.saturating_sub(t) < self.cfg.key_retry_us);
        if self.need_key && recent {
            return out;
        }
        self.need_key = true;
        self.request_keyframe(now_us, 2, &mut out);
        out
    }

    /// `reason`: 0 = perdita, 1 = avvio, 2 = richiesta esplicita dell'app, 3 = ripetizione (il keyframe atteso non è
    /// arrivato).
    fn request_keyframe(&mut self, now_us: u64, reason: u8, out: &mut Vec<Output>) {
        self.stats.keyframe_requests += 1;
        self.last_key_request_us = Some(now_us);
        out.push(Output::RequestKeyframe { last_good_frame: self.last_good, reason });
    }

    /// Consegna in ordine ciò che si può, scartando ciò che non si può più aspettare.
    fn deliver(&mut self, now_us: u64, out: &mut Vec<Output>) {
        loop {
            let Some(next) = self.next_deliver else { return };

            // 1) Un keyframe completo più avanti rende obsoleto tutto ciò che lo precede.
            let ff = self
                .frames
                .iter()
                .filter(|(id, st)| st.is_key() && st.is_complete() && (**id == next || newer(**id, next)))
                .map(|(id, _)| *id)
                .min_by_key(|id| id.wrapping_sub(next));
            if let Some(k) = ff {
                if k != next && (self.need_key || self.frames.get(&next).is_none_or(|s| !s.is_complete())) {
                    let stale: Vec<u32> =
                        self.frames.keys().copied().filter(|id| newer(k, *id) && !newer(next, *id)).collect();
                    for id in stale {
                        self.drop_frame(id, DropReason::Stale, out);
                    }
                    let gaps: Vec<u32> = self.gap_since.keys().copied().filter(|id| newer(k, *id)).collect();
                    for id in gaps {
                        self.gap_since.remove(&id);
                        self.gap_nacks.remove(&id);
                    }
                    self.next_deliver = Some(k);
                    continue;
                }
            }

            // 2) Il frame atteso
            match self.frames.get(&next) {
                Some(st) if st.is_complete() => {
                    let Some(st) = self.frames.remove(&next) else { return };
                    if self.need_key && !st.flags_is_key() {
                        self.stats.dropped_waiting_key += 1;
                        out.push(Output::Dropped { frame_id: next, reason: DropReason::WaitingKeyframe });
                    } else {
                        self.need_key = false;
                        self.last_good = next;
                        self.stats.frames_ok += 1;
                        let data: Vec<u8> = st.data.into_iter().flatten().flatten().collect();
                        out.push(Output::Frame(AssembledFrame {
                            frame_id: next,
                            codec: st.codec,
                            key: st.flags & video_flags::KEY != 0,
                            config: st.flags & video_flags::CONFIG != 0,
                            capture_ts_us: st.capture_ts_us,
                            data,
                            first_rx_us: st.first_rx_us,
                            complete_us: st.complete_us.unwrap_or(now_us),
                            recovered_fec: st.recovered_fec,
                            retransmitted: st.retransmitted,
                        }));
                    }
                    self.next_deliver = Some(next.wrapping_add(1));
                }
                Some(st) => {
                    let stalled = now_us.saturating_sub(st.last_rx_us) > self.cfg.max_frame_age_us;
                    let too_old = now_us.saturating_sub(st.first_rx_us) > self.cfg.max_frame_total_us;
                    if stalled || too_old {
                        self.drop_frame(next, DropReason::Timeout, out);
                        if !self.need_key {
                            self.need_key = true;
                            self.request_keyframe(now_us, 0, out);
                        }
                        self.next_deliver = Some(next.wrapping_add(1));
                    } else {
                        return;
                    }
                }
                None => {
                    // Frame mai arrivato: se esistono frame successivi lo si aspetta fino al timeout, poi si passa oltre.
                    let later = self.highest_seen.is_some_and(|hs| newer(hs, next));
                    if !later {
                        return;
                    }
                    let since = self.gap_since.get(&next).copied().unwrap_or(now_us);
                    if now_us.saturating_sub(since) > self.cfg.max_frame_age_us {
                        self.gap_since.remove(&next);
                        self.gap_nacks.remove(&next);
                        self.stats.dropped_timeout += 1;
                        out.push(Output::Dropped { frame_id: next, reason: DropReason::Timeout });
                        if !self.need_key {
                            self.need_key = true;
                            self.request_keyframe(now_us, 0, out);
                        }
                        self.next_deliver = Some(next.wrapping_add(1));
                    } else {
                        return;
                    }
                }
            }
        }
    }

    fn enforce_limits(&mut self, now_us: u64, out: &mut Vec<Output>) {
        while self.frames.len() > self.cfg.max_in_flight {
            let Some((&oldest, _)) = self.frames.iter().next() else { break };
            self.drop_frame(oldest, DropReason::Overflow, out);
            if self.next_deliver.is_some_and(|n| !newer(n, oldest) && n == oldest) {
                self.next_deliver = Some(oldest.wrapping_add(1));
            }
            if !self.need_key {
                self.need_key = true;
                self.request_keyframe(now_us, 0, out);
            }
        }
    }
}

impl FrameState {
    fn flags_is_key(&self) -> bool {
        self.is_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_proto::decode;

    fn frame(id: u32, _key: bool, len: usize) -> (Vec<u8>, u32) {
        let data: Vec<u8> =
            (0..len).map(|i| (i as u32).wrapping_mul(31).wrapping_add(id.wrapping_mul(7)) as u8).collect();
        (data, id)
    }

    fn feed(rx: &mut VideoReceiver, now: u64, dgram: &[u8]) -> Vec<Output> {
        match decode(dgram).unwrap().1 {
            Message::Video(v) => rx.on_fragment(now, &v),
            other => panic!("non è video: {other:?}"),
        }
    }

    fn encoded(id: u32, key: bool, data: &[u8]) -> EncodedFrame<'_> {
        EncodedFrame { frame_id: id, codec: Codec::H264, key, config: key, capture_ts_us: u64::from(id) * 8_333, data }
    }

    #[test]
    fn fragmentation_sizes_and_roundtrip() {
        let (data, _) = frame(1, true, 5000);
        let fp = fragment(&encoded(1, true, &data), 1164, 0);
        assert_eq!(fp.data_fragments(), 5);
        assert_eq!(fp.len(), 5, "senza parità");
        let fp = fragment(&encoded(1, true, &data), 1164, 2);
        assert_eq!(fp.len(), 5 + 3, "tre gruppi di parità");
        let mut rx = VideoReceiver::new(ReceiverConfig::default());
        let mut outs = Vec::new();
        for i in 0..fp.len() {
            let d = fp.datagram(i, Header { conn_id: 1, seq: i as u32 }, false).unwrap();
            assert!(d.len() <= phantom_proto::MAX_DATAGRAM);
            outs.extend(feed(&mut rx, 1_000 + i as u64, &d));
        }
        let got: Vec<&AssembledFrame> =
            outs.iter().filter_map(|o| if let Output::Frame(f) = o { Some(f) } else { None }).collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, data);
        assert!(got[0].key && got[0].config);
        assert_eq!(got[0].capture_ts_us, 8_333);
    }

    #[test]
    fn empty_and_tiny_frames() {
        for len in [0usize, 1, 1163, 1164, 1165] {
            let (data, _) = frame(9, true, len);
            let mut tx = VideoSender::new(5, 1164, 0, 4);
            let mut rx = VideoReceiver::new(ReceiverConfig::default());
            let mut outs = Vec::new();
            for d in tx.send_frame(&encoded(9, true, &data)) {
                outs.extend(feed(&mut rx, 10, &d));
            }
            let f =
                outs.iter().find_map(|o| if let Output::Frame(f) = o { Some(f.clone()) } else { None }).expect("frame");
            assert_eq!(f.data, data, "lunghezza {len}");
        }
    }

    #[test]
    fn out_of_order_and_duplicates_are_handled() {
        let (data, _) = frame(1, true, 10_000);
        let mut tx = VideoSender::new(1, 1000, 0, 4);
        let mut dg = tx.send_frame(&encoded(1, true, &data));
        dg.reverse();
        let dup = dg[3].clone();
        dg.insert(2, dup);
        let mut rx = VideoReceiver::new(ReceiverConfig::default());
        let mut frames = 0;
        for d in &dg {
            for o in feed(&mut rx, 100, d) {
                if let Output::Frame(f) = o {
                    assert_eq!(f.data, data);
                    frames += 1;
                }
            }
        }
        assert_eq!(frames, 1);
        assert_eq!(rx.stats().duplicate_fragments, 1);
    }

    #[test]
    fn fec_recovers_one_loss_per_group_without_nack() {
        let (data, _) = frame(1, true, 8_000); // 8 frammenti da 1000
        let mut tx = VideoSender::new(1, 1000, 4, 4); // gruppi da 4 → 2 parità
        let dg = tx.send_frame(&encoded(1, true, &data));
        assert_eq!(dg.len(), 10);
        let mut rx = VideoReceiver::new(ReceiverConfig::default());
        let mut frame_out = None;
        for (i, d) in dg.iter().enumerate() {
            if i == 1 || i == 6 {
                continue; // un frammento perso in ciascun gruppo
            }
            for o in feed(&mut rx, 50, d) {
                if let Output::Frame(f) = o {
                    frame_out = Some(f);
                }
            }
        }
        let f = frame_out.expect("recuperato con la parità");
        assert_eq!(f.data, data);
        assert!(f.recovered_fec);
        assert_eq!(rx.stats().fec_recovered_frames, 1);
        assert!(rx.poll(1_000_000).iter().all(|o| !matches!(o, Output::Nack { .. })), "nessun NACK necessario");
    }

    #[test]
    fn nack_asks_for_missing_fragments_and_retransmission_completes() {
        let (data, _) = frame(1, true, 8_000);
        let mut tx = VideoSender::new(1, 1000, 0, 4);
        let dg = tx.send_frame(&encoded(1, true, &data));
        let mut rx = VideoReceiver::new(ReceiverConfig::default());
        for (i, d) in dg.iter().enumerate() {
            if i == 2 || i == 5 {
                continue;
            }
            feed(&mut rx, 1_000, d);
        }
        assert!(rx.poll(2_000).is_empty(), "prima del ritardo non si chiede nulla");
        let outs = rx.poll(1_000 + 3_000);
        let Some(Output::Nack { frame_id, missing }) = outs.into_iter().find(|o| matches!(o, Output::Nack { .. }))
        else {
            panic!("serviva un NACK")
        };
        assert_eq!((frame_id, missing.clone()), (1, vec![2, 5]));
        let re = tx.on_nack(10_000, frame_id, &missing);
        assert_eq!(re.len(), 2);
        assert_eq!(tx.retransmitted, 2);
        let mut got = None;
        for d in &re {
            for o in feed(&mut rx, 9_000, d) {
                if let Output::Frame(f) = o {
                    got = Some(f);
                }
            }
        }
        let f = got.expect("completo dopo la ritrasmissione");
        assert_eq!(f.data, data);
        assert!(f.retransmitted);
    }

    #[test]
    fn nack_retries_are_bounded() {
        let (data, _) = frame(1, true, 3_000);
        let mut tx = VideoSender::new(1, 1000, 0, 4);
        let dg = tx.send_frame(&encoded(1, true, &data));
        let cfg = ReceiverConfig { max_frame_age_us: 10_000_000, ..ReceiverConfig::default() };
        let mut rx = VideoReceiver::new(cfg);
        feed(&mut rx, 0, &dg[0]);
        feed(&mut rx, 0, &dg[1]);
        let mut nacks = 0;
        for t in (0..200_000).step_by(1_000) {
            nacks += rx.poll(t).iter().filter(|o| matches!(o, Output::Nack { .. })).count();
        }
        assert_eq!(nacks, 3, "al massimo max_nacks richieste per frame");
    }

    #[test]
    fn lost_frame_times_out_requests_keyframe_and_skips_deltas_until_key() {
        let mut tx = VideoSender::new(1, 1000, 0, 8);
        let mut rx = VideoReceiver::new(ReceiverConfig::default());
        let mut outs: Vec<Output> = Vec::new();
        let mk = |id: u32, key: bool| frame(id, key, 2_500).0;
        // (frame, keyframe, perso per intero, istante di arrivo in µs)
        for (id, key, lost, now) in [
            (0u32, true, false, 0u64),
            (1, false, true, 8_333),
            (2, false, false, 16_666),
            (3, false, false, 25_000),
            (4, true, false, 90_000),
        ] {
            let data = mk(id, key);
            let dg = tx.send_frame(&encoded(id, key, &data));
            if id == 4 {
                // prima dell'arrivo del keyframe il tempo è passato: il frame 1 non è mai arrivato e va scartato
                outs.extend(rx.poll(80_000));
            }
            if lost {
                continue;
            }
            for d in dg {
                outs.extend(feed(&mut rx, now, &d));
            }
            outs.extend(rx.poll(now));
        }
        let ids: Vec<u32> =
            outs.iter().filter_map(|o| if let Output::Frame(f) = o { Some(f.frame_id) } else { None }).collect();
        assert_eq!(ids, vec![0, 4], "consegnati solo il keyframe iniziale e il successivo");
        assert!(outs.iter().any(|o| matches!(o, Output::Dropped { frame_id: 1, reason: DropReason::Timeout })));
        assert!(outs.iter().any(|o| matches!(o, Output::Dropped { frame_id: 2, reason: DropReason::WaitingKeyframe })));
        assert!(outs.iter().any(|o| matches!(o, Output::Dropped { frame_id: 3, reason: DropReason::WaitingKeyframe })));
        assert!(
            outs.iter().any(|o| matches!(o, Output::RequestKeyframe { reason: 0, .. })),
            "richiesta di keyframe dopo la perdita"
        );
        assert!(
            outs.iter().any(|o| matches!(o, Output::Nack { frame_id: 1, missing } if missing.is_empty())),
            "un frame perso per intero si richiede con NACK vuoto"
        );
    }

    #[test]
    fn lost_keyframe_is_requested_again_until_it_arrives() {
        // il keyframe richiesto dopo una perdita va perso a sua volta: senza ripetizione l'immagine resterebbe ferma
        let mut tx = VideoSender::new(1, 1000, 0, 8);
        let mut rx = VideoReceiver::new(ReceiverConfig::default());
        let mut outs: Vec<Output> = Vec::new();
        let d0 = frame(0, true, 2_000).0;
        for d in tx.send_frame(&encoded(0, true, &d0)) {
            outs.extend(feed(&mut rx, 0, &d));
        }
        // il frame 1 non arriva mai; il 2 e il 3 arrivano ma restano scartati in attesa di un keyframe
        for (id, now) in [(2u32, 16_666u64), (3, 25_000)] {
            let data = frame(id, false, 1_500).0;
            for d in tx.send_frame(&encoded(id, false, &data)) {
                outs.extend(feed(&mut rx, now, &d));
            }
            outs.extend(rx.poll(now));
        }
        outs.extend(rx.poll(100_000));
        let requests = |outs: &[Output]| outs.iter().filter(|o| matches!(o, Output::RequestKeyframe { .. })).count();
        let first = requests(&outs);
        assert!(first >= 1, "la prima richiesta c'è");
        // il keyframe non arriva: passano 400 ms, nessuna ripetizione; oltre i 500 ms ne parte una
        let mut later = Vec::new();
        later.extend(rx.poll(400_000));
        assert_eq!(requests(&later), 0, "troppo presto per ripetere");
        later.extend(rx.poll(650_000));
        assert_eq!(requests(&later), 1, "dopo 500 ms si richiede di nuovo");
        assert!(later.iter().any(|o| matches!(o, Output::RequestKeyframe { reason: 3, .. })), "motivo: ripetizione");
        later.extend(rx.poll(700_000));
        assert_eq!(requests(&later), 1, "e non a ogni giro");
        later.extend(rx.poll(1_200_000));
        assert_eq!(requests(&later), 2, "poi ogni 500 ms finché serve");
        // il keyframe finalmente arriva: si riparte e le richieste cessano
        let k = frame(9, true, 1_800).0;
        let mut got = Vec::new();
        for d in tx.send_frame(&encoded(9, true, &k)) {
            got.extend(feed(&mut rx, 1_250_000, &d));
        }
        assert!(got.iter().any(|o| matches!(o, Output::Frame(f) if f.frame_id == 9)));
        let mut after = rx.poll(2_000_000);
        after.extend(rx.poll(3_000_000));
        assert_eq!(requests(&after), 0, "con il keyframe arrivato non si chiede più nulla");
        assert_eq!(rx.stats().key_retries, 2);
    }

    #[test]
    fn complete_keyframe_fast_forwards_over_incomplete_frames() {
        let mut tx = VideoSender::new(1, 1000, 0, 8);
        let mut rx = VideoReceiver::new(ReceiverConfig::default());
        let mut outs = Vec::new();
        // 0 key ok; 1 a metà (resta incompleto); 2 è un keyframe completo → si salta subito a 2
        let d0 = frame(0, true, 1_500).0;
        for d in tx.send_frame(&encoded(0, true, &d0)) {
            outs.extend(feed(&mut rx, 0, &d));
        }
        let d1 = frame(1, false, 3_500).0;
        let dg1 = tx.send_frame(&encoded(1, false, &d1));
        outs.extend(feed(&mut rx, 8_000, &dg1[0]));
        let d2 = frame(2, true, 1_500).0;
        for d in tx.send_frame(&encoded(2, true, &d2)) {
            outs.extend(feed(&mut rx, 9_000, &d));
        }
        let ids: Vec<u32> =
            outs.iter().filter_map(|o| if let Output::Frame(f) = o { Some(f.frame_id) } else { None }).collect();
        assert_eq!(ids, vec![0, 2]);
        assert!(outs.iter().any(|o| matches!(o, Output::Dropped { frame_id: 1, reason: DropReason::Stale })));
        // un frammento tardivo del frame 1 si ignora
        let before = rx.stats().late_fragments;
        feed(&mut rx, 10_000, &dg1[1]);
        assert_eq!(rx.stats().late_fragments, before + 1);
    }

    #[test]
    fn delta_frames_flow_in_order_when_nothing_is_lost() {
        let mut tx = VideoSender::new(1, 1000, 4, 8);
        let mut rx = VideoReceiver::new(ReceiverConfig::default());
        let mut ids = Vec::new();
        for id in 0..50u32 {
            let data = frame(id, id % 30 == 0, 3_000).0;
            for d in tx.send_frame(&encoded(id, id % 30 == 0, &data)) {
                for o in feed(&mut rx, u64::from(id) * 8_333, &d) {
                    if let Output::Frame(f) = o {
                        assert_eq!(f.data, data);
                        ids.push(f.frame_id);
                    }
                }
            }
        }
        assert_eq!(ids, (0..50).collect::<Vec<_>>());
        assert_eq!(rx.stats().frames_ok, 50);
    }

    #[test]
    fn overflow_is_bounded() {
        let cfg = ReceiverConfig { max_in_flight: 4, max_frame_age_us: 10_000_000, ..ReceiverConfig::default() };
        let mut tx = VideoSender::new(1, 1000, 0, 64);
        let mut rx = VideoReceiver::new(cfg);
        for id in 0..40u32 {
            let data = frame(id, id == 0, 3_000).0;
            let dg = tx.send_frame(&encoded(id, id == 0, &data));
            feed(&mut rx, u64::from(id), &dg[0]); // solo il primo frammento di ogni frame: nessuno si completa
        }
        assert!(rx.frames.len() <= 4);
        assert!(rx.stats().dropped_overflow > 0);
    }

    #[test]
    fn corrupt_or_inconsistent_fragments_do_not_panic() {
        let mut rx = VideoReceiver::new(ReceiverConfig::default());
        let payload = vec![1u8; 10];
        // lunghezza del payload diversa da quella attesa dal frammento: ignorato
        let v = VideoFragment {
            frame_id: 1,
            frag_index: 0,
            frag_count: 2,
            flags: video_flags::KEY,
            codec: Codec::H264,
            frag_size: 100,
            total_len: 150,
            capture_ts_us: 0,
            fec_group: 0,
            payload: &payload,
        };
        rx.on_fragment(0, &v);
        // stesso frame con parametri diversi: ignorato
        let v2 = VideoFragment {
            frame_id: 1,
            frag_index: 1,
            frag_count: 3,
            flags: 0,
            codec: Codec::H264,
            frag_size: 100,
            total_len: 250,
            capture_ts_us: 0,
            fec_group: 0,
            payload: &payload,
        };
        rx.on_fragment(1, &v2);
        assert_eq!(rx.stats().frames_ok, 0);
    }
}
