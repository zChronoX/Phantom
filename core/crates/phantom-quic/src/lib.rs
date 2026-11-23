//! Trasporto QUIC (fase 2): datagrammi opachi sopra QUIC.
//!
//! Il core (`phantom-core`) resta una macchina a stati pura: non crea thread,
//! non legge orologi e non fa I/O. Questo crate si occupa solo del trasporto:
//! apre una connessione QUIC, la autentica fissando il certificato dell'host e
//! sposta datagrammi non affidabili (RFC 9221). Il protocollo Phantom
//! (`Hello`/`HelloAck`, video, tocco, tastiera, `Close`) continua a vivere nel
//! core, identico a com'era su UDP.
//!
//! L'API è bloccante perché l'app (Swift e Kotlin) ha già i propri thread:
//! `accept`/`connect` e `recv_datagram` aspettano con un timeout esplicito,
//! mentre `send_datagram` è immediato. Dentro c'è un runtime Tokio privato per
//! endpoint, con due thread di lavoro.
//!
//! Sicurezza: QUIC richiede TLS 1.3. Ogni host genera un certificato
//! autofirmato effimero; il client lo riceve fuori banda (per ora in memoria,
//! nella fase 5 con l'accoppiamento) e lo **fissa**. Non c'è una PKI e non si
//! accettano certificati diversi da quello fissato.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Connection, Endpoint, IdleTimeout, ServerConfig, TransportConfig, VarInt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::runtime::Runtime;

/// Protocollo applicativo annunciato in ALPN: host e client devono usare lo
/// stesso valore, altrimenti il handshake fallisce.
pub const ALPN: &[u8] = b"phantom/0";

/// Nome usato dal client per il certificato autofirmato dell'host.
const SERVER_NAME: &str = "localhost";

/// Errore del trasporto QUIC.
#[derive(Debug)]
pub enum QuicError {
    /// Errore di I/O (bind, indirizzo, runtime Tokio).
    Io(io::Error),
    /// Errore TLS del handshake o della configurazione.
    Tls(rustls::Error),
    /// Configurazione QUIC non valida (cifrari iniziali assenti).
    TlsConfig(String),
    /// Il certificato da fissare è vuoto.
    EmptyCertificate,
    /// Generazione del certificato autofirmato fallita.
    Certificate(rcgen::Error),
    /// Connessione non avviata (indirizzo o nome del server non validi).
    Connect(quinn::ConnectError),
    /// Connessione chiusa o handshake fallito.
    Connection(quinn::ConnectionError),
    /// Datagramma non accettato dal buffer di invio di QUIC.
    SendDatagram(quinn::SendDatagramError),
    /// Nessuna risposta entro il timeout.
    Timeout,
    /// L'endpoint è stato chiuso: non arriveranno altre connessioni.
    EndpointClosed,
    /// L'altro lato non supporta i datagrammi QUIC.
    DatagramsUnsupported,
    /// Il datagramma supera il massimo consentito da questa connessione.
    DatagramTooLarge {
        /// Lunghezza del datagramma che si è provato a mandare.
        len: usize,
        /// Massimo consentito in questo momento (`Connection::max_datagram_size`).
        max: usize,
    },
    /// Il buffer di ricezione è più piccolo del datagramma arrivato; il
    /// datagramma è stato scartato.
    BufferTooSmall {
        /// Lunghezza del datagramma arrivato.
        len: usize,
        /// Capacità del buffer passato a `recv_datagram`.
        capacity: usize,
    },
}

impl fmt::Display for QuicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QuicError::Io(e) => write!(f, "errore di I/O: {e}"),
            QuicError::Tls(e) => write!(f, "errore TLS: {e}"),
            QuicError::TlsConfig(e) => write!(f, "configurazione QUIC non valida: {e}"),
            QuicError::EmptyCertificate => write!(f, "il certificato da fissare è vuoto"),
            QuicError::Certificate(e) => write!(f, "certificato autofirmato non generato: {e}"),
            QuicError::Connect(e) => write!(f, "connessione non avviata: {e}"),
            QuicError::Connection(e) => write!(f, "connessione chiusa: {e}"),
            QuicError::SendDatagram(e) => write!(f, "datagramma non inviato: {e}"),
            QuicError::Timeout => write!(f, "nessuna risposta entro il timeout"),
            QuicError::EndpointClosed => write!(f, "endpoint chiuso"),
            QuicError::DatagramsUnsupported => write!(f, "l'altro lato non supporta i datagrammi QUIC"),
            QuicError::DatagramTooLarge { len, max } => {
                write!(f, "datagramma di {len} byte, massimo {max}")
            }
            QuicError::BufferTooSmall { len, capacity } => {
                write!(f, "datagramma di {len} byte, buffer di {capacity}")
            }
        }
    }
}

impl std::error::Error for QuicError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            QuicError::Io(e) => Some(e),
            QuicError::Tls(e) => Some(e),
            QuicError::Certificate(e) => Some(e),
            QuicError::Connect(e) => Some(e),
            QuicError::Connection(e) => Some(e),
            QuicError::SendDatagram(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for QuicError {
    fn from(error: io::Error) -> Self {
        QuicError::Io(error)
    }
}

/// Identità TLS persistente dell'host (certificato X.509 DER e chiave privata PKCS#8 DER).
///
/// Generata una sola volta e salvata sul dispositivo Android, consente all'iPhone di fissare
/// lo stesso certificato tra un riavvio e l'altro (in LAN o da remoto su 5G/4G).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostIdentity {
    /// Certificato X.509 autofirmato in formato DER.
    pub cert_der: Vec<u8>,
    /// Chiave privata PKCS#8 in formato DER.
    pub key_der: Vec<u8>,
}

/// Genera una nuova identità TLS autofirmata (`cert_der` + `key_der`) pronta per essere salvata e
/// passata a [`HostEndpoint::bind_with_identity`].
pub fn generate_host_identity() -> Result<HostIdentity, QuicError> {
    let generated = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_owned()]).map_err(QuicError::Certificate)?;
    let cert_der = generated.cert.der().to_vec();
    let key_der = generated.key_pair.serialize_der();
    Ok(HostIdentity { cert_der, key_der })
}

/// Endpoint dell'host (server QUIC).
///
/// Può usare un certificato effimero ([`HostEndpoint::bind`]) oppure un'identità persistente
/// salvata sul dispositivo ([`HostEndpoint::bind_with_identity`]): il client fissa
/// [`HostEndpoint::certificate_der`] per autenticare l'host.
#[derive(Debug)]
pub struct HostEndpoint {
    endpoint: Endpoint,
    certificate: Vec<u8>,
    // Ultimo campo: si chiude l'endpoint prima che il runtime sparisca.
    runtime: Arc<Runtime>,
}

impl HostEndpoint {
    /// Apre un endpoint in ascolto su `addr` (`127.0.0.1:0` per una porta
    /// effimera) e genera un nuovo certificato autofirmato.
    pub fn bind(addr: SocketAddr) -> Result<Self, QuicError> {
        let identity = generate_host_identity()?;
        Self::bind_with_identity(addr, identity.cert_der, identity.key_der)
    }

    /// Apre un endpoint in ascolto su `addr` usando un certificato X.509 DER e una chiave privata
    /// PKCS#8 DER persistenti (generati con [`generate_host_identity`]).
    pub fn bind_with_identity(addr: SocketAddr, cert_der: Vec<u8>, key_der: Vec<u8>) -> Result<Self, QuicError> {
        if cert_der.is_empty() || key_der.is_empty() {
            return Err(QuicError::EmptyCertificate);
        }
        let runtime = new_runtime()?;
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der));
        let server = server_config(&cert_der, key)?;
        let endpoint = {
            let _guard = runtime.enter();
            Endpoint::server(server, addr).map_err(QuicError::Io)?
        };
        Ok(HostEndpoint { endpoint, certificate: cert_der, runtime: Arc::new(runtime) })
    }

    /// Indirizzo locale su cui l'endpoint è in ascolto.
    pub fn local_addr(&self) -> Result<SocketAddr, QuicError> {
        self.endpoint.local_addr().map_err(QuicError::Io)
    }

    /// Certificato (DER) da consegnare al client e fissare lì.
    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate
    }

    /// Aspetta una connessione e completa il handshake entro `timeout`.
    pub fn accept(&self, timeout: Duration) -> Result<QuicLink, QuicError> {
        let connection = self.runtime.block_on(async {
            let incoming = tokio::time::timeout(timeout, self.endpoint.accept())
                .await
                .map_err(|_| QuicError::Timeout)?
                .ok_or(QuicError::EndpointClosed)?;
            tokio::time::timeout(HANDSHAKE_TIMEOUT, incoming)
                .await
                .map_err(|_| QuicError::Timeout)?
                .map_err(QuicError::Connection)
        })?;
        Ok(QuicLink { connection, runtime: Arc::clone(&self.runtime) })
    }
}

impl Drop for HostEndpoint {
    fn drop(&mut self) {
        let _guard = self.runtime.enter();
        self.endpoint.close(VarInt::from_u32(0), b"");
    }
}

/// Endpoint del client.
///
/// Il certificato dell'host viene fissato a ogni [`ClientEndpoint::connect`]:
/// non esiste una PKI e nessun altro certificato è accettato.
#[derive(Debug)]
pub struct ClientEndpoint {
    endpoint: Endpoint,
    // Ultimo campo: si chiude l'endpoint prima che il runtime sparisca.
    runtime: Arc<Runtime>,
}

impl ClientEndpoint {
    /// Apre un endpoint locale su `addr` (`127.0.0.1:0` per una porta
    /// effimera). La connessione si apre con [`ClientEndpoint::connect`].
    pub fn bind(addr: SocketAddr) -> Result<Self, QuicError> {
        let runtime = new_runtime()?;
        let endpoint = {
            let _guard = runtime.enter();
            Endpoint::client(addr).map_err(QuicError::Io)?
        };
        Ok(ClientEndpoint { endpoint, runtime: Arc::new(runtime) })
    }

    /// Si collega all'host, verifica che il suo certificato sia esattamente
    /// `pinned_certificate_der` e completa il handshake entro `timeout`.
    pub fn connect(
        &self,
        addr: SocketAddr,
        server_name: &str,
        pinned_certificate_der: &[u8],
        timeout: Duration,
    ) -> Result<QuicLink, QuicError> {
        let config = client_config(pinned_certificate_der)?;
        let connecting = {
            // `connect_with` avvia subito il task della connessione: serve il
            // contesto del runtime, non solo quello del futuro che si aspetta.
            let _guard = self.runtime.enter();
            self.endpoint.connect_with(config, addr, server_name).map_err(QuicError::Connect)?
        };
        let connection = self.runtime.block_on(async {
            match tokio::time::timeout(timeout, connecting).await {
                Err(_) => Err(QuicError::Timeout),
                Ok(result) => result.map_err(QuicError::Connection),
            }
        })?;
        Ok(QuicLink { connection, runtime: Arc::clone(&self.runtime) })
    }
}

impl Drop for ClientEndpoint {
    fn drop(&mut self) {
        let _guard = self.runtime.enter();
        self.endpoint.close(VarInt::from_u32(0), b"");
    }
}

/// Connessione QUIC aperta: sposta datagrammi non affidabili e non ordinati.
#[derive(Debug)]
pub struct QuicLink {
    connection: Connection,
    // Ultimo campo: si chiude la connessione prima che il runtime sparisca.
    runtime: Arc<Runtime>,
}

impl QuicLink {
    /// Manda un datagramma. Se supera il massimo corrente
    /// ([`QuicLink::max_datagram_size`]) non viene inviato.
    pub fn send_datagram(&self, datagram: &[u8]) -> Result<(), QuicError> {
        let max = self.max_datagram_size().ok_or(QuicError::DatagramsUnsupported)?;
        if datagram.len() > max {
            return Err(QuicError::DatagramTooLarge { len: datagram.len(), max });
        }
        self.connection.send_datagram(Bytes::copy_from_slice(datagram)).map_err(QuicError::SendDatagram)
    }

    /// Aspetta un datagramma entro `timeout` e lo copia in `buf`.
    ///
    /// Se il datagramma è più grande del buffer viene scartato e restituisce
    /// [`QuicError::BufferTooSmall`]: i datagrammi sono unità autonome, non si
    /// può leggerne una parte.
    pub fn recv_datagram(&self, buf: &mut [u8], timeout: Duration) -> Result<usize, QuicError> {
        let received =
            self.runtime.block_on(async { tokio::time::timeout(timeout, self.connection.read_datagram()).await });
        let datagram = match received {
            Err(_) => return Err(QuicError::Timeout),
            Ok(Err(error)) => return Err(QuicError::Connection(error)),
            Ok(Ok(datagram)) => datagram,
        };
        if datagram.len() > buf.len() {
            return Err(QuicError::BufferTooSmall { len: datagram.len(), capacity: buf.len() });
        }
        buf[..datagram.len()].copy_from_slice(&datagram);
        Ok(datagram.len())
    }

    /// Spazio libero (byte) nel buffer dei datagrammi in uscita. Se un datagramma è più grande di questo spazio,
    /// spedirlo fa scartare datagrammi più vecchi ancora in coda.
    pub fn datagram_send_buffer_space(&self) -> usize {
        self.connection.datagram_send_buffer_space()
    }

    /// Massimo datagramma accettato dalla connessione, secondo la stima del
    /// path MTU. Può crescere durante la connessione; `None` se i datagrammi
    /// non sono supportati.
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.connection.max_datagram_size()
    }

    /// Statistiche del percorso (RTT, finestra di congestione, perdite).
    pub fn link_stats(&self) -> LinkStats {
        let p = self.connection.stats().path;
        LinkStats {
            rtt_us: u64::try_from(p.rtt.as_micros()).unwrap_or(u64::MAX),
            cwnd_bytes: p.cwnd,
            sent_packets: p.sent_packets,
            lost_packets: p.lost_packets,
            congestion_events: p.congestion_events,
            via_relay: false,
        }
    }

    /// Chiude subito la connessione con un codice applicativo.
    pub fn close(&self, code: u32, reason: &[u8]) {
        let _guard = self.runtime.enter();
        self.connection.close(VarInt::from_u32(code), reason);
    }

    /// `true` se la connessione è stata chiusa da un lato o dall'altro.
    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }
}

impl Drop for QuicLink {
    fn drop(&mut self) {
        let _guard = self.runtime.enter();
        self.connection.close(VarInt::from_u32(0), b"");
    }
}

fn new_runtime() -> Result<Runtime, QuicError> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("phantom-quic")
        .enable_all()
        .build()
        .map_err(QuicError::Io)
}

/// Byte di datagrammi in uscita che QUIC tiene in coda prima di scartare i più vecchi.
///
/// Prima era 1 MiB: a 12 Mbit/s sono circa 0,7 s di video che possono aspettare in coda sul telefono, con la latenza
/// che sale e il controllo del bitrate che se ne accorge tardi. 512 KiB contengono ancora un keyframe 1080p ad alto
/// bitrate (se la coda fosse più corta del keyframe, QUIC ne butterebbe via l'inizio appena spedito), ma dimezzano la
/// coda possibile. Il resto lo fa BBR, che non svuota la finestra a ogni perdita casuale del Wi-Fi.
pub const DATAGRAM_SEND_BUFFER: usize = 512 * 1024;

/// Tempo massimo per completare l'handshake di una connessione in arrivo. È separato dall'attesa di una nuova
/// connessione (`accept(timeout)`, di solito poche centinaia di ms): prima si usava lo stesso timeout anche per
/// l'handshake, e una connessione appena un po' lenta veniva abbandonata a metà.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Statistiche del percorso di una connessione (per la diagnosi degli stalli).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// RTT stimato da QUIC (µs).
    pub rtt_us: u64,
    /// Finestra di congestione (byte).
    pub cwnd_bytes: u64,
    /// Pacchetti QUIC spediti (cumulativo).
    pub sent_packets: u64,
    /// Pacchetti QUIC persi (cumulativo).
    pub lost_packets: u64,
    /// Eventi di congestione (cumulativo).
    pub congestion_events: u64,
    /// Il percorso scelto passa da un relay (solo P2P).
    pub via_relay: bool,
}

/// Byte di datagrammi in arrivo tenuti in attesa che l'app li legga.
pub const DATAGRAM_RECEIVE_BUFFER: usize = 1 << 20;

fn transport_config() -> Arc<TransportConfig> {
    let mut config = TransportConfig::default();
    // I datagrammi sono opzionali: senza buffer di ricezione QUIC li rifiuta.
    config.datagram_receive_buffer_size(Some(DATAGRAM_RECEIVE_BUFFER)).datagram_send_buffer_size(DATAGRAM_SEND_BUFFER);
    // BBR invece di Cubic: Cubic dimezza la finestra a ogni perdita, e sul Wi-Fi di un telefono le perdite sono
    // spesso casuali, non congestione. Con Cubic i datagrammi si accumulavano in coda (latenza) e poi venivano
    // scartati (frame rotti → keyframe → ancora più coda). Il controllo del bitrate vero resta quello di Phantom.
    config.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    // Keep-alive e timeout calibrati anche per reti mobili (4G/5G / NAT / Tailscale).
    config.keep_alive_interval(Some(Duration::from_secs(3)));
    if let Ok(timeout) = IdleTimeout::try_from(Duration::from_secs(15)) {
        config.max_idle_timeout(Some(timeout));
    }
    Arc::new(config)
}

fn server_config(certificate: &[u8], key: PrivateKeyDer<'static>) -> Result<ServerConfig, QuicError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(QuicError::Tls)?
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(certificate.to_vec())], key)
        .map_err(QuicError::Tls)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let quic = QuicServerConfig::try_from(tls).map_err(|e| QuicError::TlsConfig(e.to_string()))?;
    let mut config = ServerConfig::with_crypto(Arc::new(quic));
    config.transport_config(transport_config());
    Ok(config)
}

fn client_config(pinned_certificate_der: &[u8]) -> Result<ClientConfig, QuicError> {
    if pinned_certificate_der.is_empty() {
        return Err(QuicError::EmptyCertificate);
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinnedCertificate {
        provider: Arc::clone(&provider),
        pinned: CertificateDer::from(pinned_certificate_der.to_vec()),
    });
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(QuicError::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let quic = QuicClientConfig::try_from(tls).map_err(|e| QuicError::TlsConfig(e.to_string()))?;
    let mut config = ClientConfig::new(Arc::new(quic));
    config.transport_config(transport_config());
    Ok(config)
}

/// Verifica che il certificato dell'host sia esattamente quello fissato.
#[derive(Debug)]
struct PinnedCertificate {
    provider: Arc<rustls::crypto::CryptoProvider>,
    pinned: CertificateDer<'static>,
}

impl ServerCertVerifier for PinnedCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.pinned.as_ref() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("phantom: certificato dell'host diverso da quello fissato".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}
