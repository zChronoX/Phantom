//! Trasporto P2P integrato per Phantom basato su `iroh` (QUIC + UDP hole punching + relay cifrato E2E).
//!
//! Permette all'iPhone in 5G/4G o su qualsiasi Wi-Fi esterno di raggiungere il telefono Android a casa
//! **senza VPN esterne (niente Tailscale) e senza aprire porte sul router**: l'identità è la chiave
//! crittografica Ed25519 persistente dell'host (`EndpointId`), autenticata automaticamente da TLS 1.3.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use iroh::endpoint::{presets, Connection, IdleTimeout, QuicTransportConfig, VarInt};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use tokio::runtime::Runtime;

/// Protocollo applicativo annunciato in ALPN su `iroh` (identico al trasporto QUIC diretto).
pub const ALPN: &[u8] = b"phantom/0";

/// Errore del trasporto P2P (`iroh`).
#[derive(Debug)]
pub enum P2pError {
    /// Chiave segreta Ed25519 non valida (attesi esattamente 32 byte).
    InvalidSecretKey,
    /// `EndpointId` remoto non valido.
    InvalidEndpointId(String),
    /// Nessuna risposta entro il timeout richiesto.
    Timeout,
    /// L'endpoint è stato chiuso e non accetta più connessioni.
    EndpointClosed,
    /// L'altro lato non supporta i datagrammi QUIC.
    DatagramsUnsupported,
    /// Il datagramma supera il massimo consentito in questo momento.
    DatagramTooLarge {
        /// Lunghezza del datagramma inviato.
        len: usize,
        /// Massimo consentito dalla connessione.
        max: usize,
    },
    /// Il datagramma ricevuto supera la capacità del buffer passato dal chiamante.
    BufferTooSmall {
        /// Lunghezza del datagramma ricevuto.
        len: usize,
        /// Capacità del buffer locale.
        capacity: usize,
    },
    /// Connessione chiusa o handshake fallito.
    Connection(String),
    /// Errore locale di bind, runtime o invio.
    Transport(String),
}

impl fmt::Display for P2pError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            P2pError::InvalidSecretKey => write!(f, "chiave P2P non valida (attesi 32 byte)"),
            P2pError::InvalidEndpointId(msg) => write!(f, "EndpointId P2P non valido: {msg}"),
            P2pError::Timeout => write!(f, "nessuna risposta P2P entro il timeout"),
            P2pError::EndpointClosed => write!(f, "endpoint P2P chiuso"),
            P2pError::DatagramsUnsupported => write!(f, "datagrammi QUIC non supportati sul link P2P"),
            P2pError::DatagramTooLarge { len, max } => write!(f, "datagramma di {len} byte, massimo {max}"),
            P2pError::BufferTooSmall { len, capacity } => {
                write!(f, "datagramma di {len} byte, buffer di {capacity}")
            }
            P2pError::Connection(msg) => write!(f, "{msg}"),
            P2pError::Transport(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for P2pError {}

/// Genera una nuova chiave segreta Ed25519 (32 byte) da salvare sull'host Android.
pub fn generate_secret_key() -> [u8; 32] {
    SecretKey::generate().to_bytes()
}

/// Ricava la stringa `EndpointId` pubblica corrispondente a una chiave segreta di 32 byte.
pub fn endpoint_id_from_secret_key(secret_key: &[u8]) -> Result<String, P2pError> {
    let arr: [u8; 32] = secret_key.try_into().map_err(|_| P2pError::InvalidSecretKey)?;
    Ok(SecretKey::from_bytes(&arr).public().to_string())
}

/// Endpoint P2P (`iroh`) utilizzabile sia lato host (in ascolto con chiave persistente) sia lato client.
#[derive(Debug)]
pub struct P2pEndpoint {
    endpoint: Endpoint,
    online: bool,
    // Ultimo campo: l'endpoint viene chiuso prima del runtime Tokio.
    runtime: Arc<Runtime>,
}

impl P2pEndpoint {
    /// Apre un endpoint host P2P con chiave segreta persistente (32 byte), discovery globale n0 e relay.
    pub fn bind_host(secret_key: &[u8], wait_online: Duration) -> Result<Self, P2pError> {
        let arr: [u8; 32] = secret_key.try_into().map_err(|_| P2pError::InvalidSecretKey)?;
        let sk = SecretKey::from_bytes(&arr);
        let runtime = new_runtime()?;
        let tc = transport_config();
        let (endpoint, online) = runtime.block_on(async {
            let ep = Endpoint::builder(presets::N0)
                .secret_key(sk)
                .alpns(vec![ALPN.to_vec()])
                .transport_config(tc)
                .bind()
                .await
                .map_err(|e| P2pError::Transport(e.to_string()))?;
            let online = !wait_online.is_zero() && tokio::time::timeout(wait_online, ep.online()).await.is_ok();
            Ok::<_, P2pError>((ep, online))
        })?;
        Ok(Self { endpoint, online, runtime: Arc::new(runtime) })
    }

    /// Apre un endpoint client P2P pronto per collegarsi tramite `EndpointId` (da 5G/4G o qualsiasi Wi-Fi).
    pub fn bind_client(wait_online: Duration) -> Result<Self, P2pError> {
        let runtime = new_runtime()?;
        let tc = transport_config();
        let (endpoint, online) = runtime.block_on(async {
            let ep = Endpoint::builder(presets::N0)
                .transport_config(tc)
                .bind()
                .await
                .map_err(|e| P2pError::Transport(e.to_string()))?;
            let online = !wait_online.is_zero() && tokio::time::timeout(wait_online, ep.online()).await.is_ok();
            Ok::<_, P2pError>((ep, online))
        })?;
        Ok(Self { endpoint, online, runtime: Arc::new(runtime) })
    }

    /// Identificatore pubblico (`EndpointId`) di questo nodo, usato dal client per raggiungerlo ovunque.
    pub fn endpoint_id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// `true` se l'endpoint ha completato la registrazione al relay entro `wait_online`.
    pub fn is_online(&self) -> bool {
        self.online
    }

    /// Aspetta una connessione P2P in ingresso entro `timeout`.
    pub fn accept(&self, timeout: Duration) -> Result<P2pLink, P2pError> {
        let connection = self.runtime.block_on(async {
            let incoming = tokio::time::timeout(timeout, self.endpoint.accept())
                .await
                .map_err(|_| P2pError::Timeout)?
                .ok_or(P2pError::EndpointClosed)?;
            tokio::time::timeout(timeout, incoming)
                .await
                .map_err(|_| P2pError::Timeout)?
                .map_err(|e| P2pError::Connection(e.to_string()))
        })?;
        Ok(P2pLink { connection, runtime: Arc::clone(&self.runtime) })
    }

    /// Si collega a un host remoto tramite il suo `EndpointId` (P2P diretto con hole punching o relay cifrato E2E).
    pub fn connect(&self, remote_endpoint_id: &str, timeout: Duration) -> Result<P2pLink, P2pError> {
        let id =
            EndpointId::from_str(remote_endpoint_id.trim()).map_err(|e| P2pError::InvalidEndpointId(e.to_string()))?;
        self.connect_addr(id, timeout)
    }

    fn connect_addr(&self, addr: impl Into<EndpointAddr>, timeout: Duration) -> Result<P2pLink, P2pError> {
        let target: EndpointAddr = addr.into();
        let connection = self.runtime.block_on(async {
            tokio::time::timeout(timeout, self.endpoint.connect(target, ALPN))
                .await
                .map_err(|_| P2pError::Timeout)?
                .map_err(|e| P2pError::Connection(e.to_string()))
        })?;
        Ok(P2pLink { connection, runtime: Arc::clone(&self.runtime) })
    }

    /// Notifica a `iroh` un cambio di interfaccia di rete (es. passaggio Wi-Fi ↔ 5G/4G) per migrare i percorsi.
    pub fn network_change(&self) {
        self.runtime.block_on(async { self.endpoint.network_change().await });
    }
}

impl Drop for P2pEndpoint {
    fn drop(&mut self) {
        self.runtime.block_on(async {
            let _ = tokio::time::timeout(Duration::from_millis(400), self.endpoint.close()).await;
        });
    }
}

/// Connessione P2P aperta che trasporta datagrammi QUIC non affidabili (stessa semantica di `QuicLink`).
#[derive(Debug)]
pub struct P2pLink {
    connection: Connection,
    // Ultimo campo: la connessione si chiude prima che il runtime sparisca.
    runtime: Arc<Runtime>,
}

impl P2pLink {
    /// Spedisce un datagramma, se rientra in [`P2pLink::max_datagram_size`].
    pub fn send_datagram(&self, datagram: &[u8]) -> Result<(), P2pError> {
        let max = self.max_datagram_size().ok_or(P2pError::DatagramsUnsupported)?;
        if datagram.len() > max {
            return Err(P2pError::DatagramTooLarge { len: datagram.len(), max });
        }
        let payload = Bytes::copy_from_slice(datagram);
        self.connection.send_datagram(payload).map_err(|e| P2pError::Transport(e.to_string()))
    }

    /// Attende un datagramma fino a `timeout` e lo copia in `buf`.
    pub fn recv_datagram(&self, buf: &mut [u8], timeout: Duration) -> Result<usize, P2pError> {
        let received =
            self.runtime.block_on(async { tokio::time::timeout(timeout, self.connection.read_datagram()).await });
        let datagram = match received {
            Err(_) => return Err(P2pError::Timeout),
            Ok(Err(error)) => return Err(P2pError::Connection(error.to_string())),
            Ok(Ok(datagram)) => datagram,
        };
        if datagram.len() > buf.len() {
            return Err(P2pError::BufferTooSmall { len: datagram.len(), capacity: buf.len() });
        }
        buf[..datagram.len()].copy_from_slice(&datagram);
        Ok(datagram.len())
    }

    /// Spazio libero (byte) nel buffer dei datagrammi in uscita.
    pub fn datagram_send_buffer_space(&self) -> usize {
        self.connection.datagram_send_buffer_space()
    }

    /// Massimo datagramma applicativo accettato in questo momento.
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.connection.max_datagram_size()
    }

    /// Chiude la connessione con codice e motivo applicativo.
    pub fn close(&self, code: u32, reason: &[u8]) {
        let _guard = self.runtime.enter();
        self.connection.close(VarInt::from_u32(code), reason);
    }

    /// `true` se la connessione P2P è già chiusa.
    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }
}

impl Drop for P2pLink {
    fn drop(&mut self) {
        let _guard = self.runtime.enter();
        self.connection.close(VarInt::from_u32(0), b"");
    }
}

fn new_runtime() -> Result<Runtime, P2pError> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("phantom-p2p")
        .enable_all()
        .build()
        .map_err(|e| P2pError::Transport(e.to_string()))
}

/// Coda dei datagrammi in uscita: stesso ragionamento di `phantom-quic` (contiene un keyframe, non 0,7 s di video).
const DATAGRAM_SEND_BUFFER: usize = 512 * 1024;

fn transport_config() -> QuicTransportConfig {
    let mut builder = QuicTransportConfig::builder()
        .datagram_receive_buffer_size(Some(1 << 20))
        .datagram_send_buffer_size(DATAGRAM_SEND_BUFFER)
        // BBR (v3) invece di Cubic: le perdite casuali di Wi-Fi e 5G non svuotano la finestra di invio.
        .congestion_controller_factory(Arc::new(noq_proto::congestion::Bbr3Config::default()))
        .keep_alive_interval(Duration::from_secs(3));
    if let Ok(timeout) = IdleTimeout::try_from(Duration::from_secs(15)) {
        builder = builder.max_idle_timeout(Some(timeout));
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_key_roundtrip_and_invalid_lengths() {
        let key = generate_secret_key();
        let id1 = endpoint_id_from_secret_key(&key).expect("id valido");
        let id2 = endpoint_id_from_secret_key(&key).expect("id deterministico");
        assert_eq!(id1, id2);
        assert!(EndpointId::from_str(&id1).is_ok());
        assert!(matches!(endpoint_id_from_secret_key(&[1, 2, 3]), Err(P2pError::InvalidSecretKey)));
    }

    #[test]
    fn minimal_p2p_loopback_exchanges_datagrams() {
        use std::net::{Ipv4Addr, SocketAddr};

        let key = generate_secret_key();
        let sk = SecretKey::from_bytes(&key);
        let rt_host = new_runtime().expect("rt host");
        let host_ep = rt_host
            .block_on(async {
                Endpoint::builder(presets::Minimal)
                    .secret_key(sk)
                    .alpns(vec![ALPN.to_vec()])
                    .transport_config(transport_config())
                    .bind()
                    .await
            })
            .expect("bind minimal host");
        let port = host_ep.bound_sockets().into_iter().find(SocketAddr::is_ipv4).expect("socket ipv4").port();
        let host_addr = EndpointAddr::from(host_ep.id()).with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        let host = P2pEndpoint { endpoint: host_ep, online: false, runtime: Arc::new(rt_host) };

        let rt_client = new_runtime().expect("rt client");
        let client_ep = rt_client
            .block_on(async { Endpoint::builder(presets::Minimal).transport_config(transport_config()).bind().await })
            .expect("bind minimal client");
        let client = P2pEndpoint { endpoint: client_ep, online: false, runtime: Arc::new(rt_client) };

        let (tx, rx) = std::sync::mpsc::channel();
        let acceptor = std::thread::spawn(move || {
            let link = host.accept(Duration::from_secs(5)).expect("accept");
            let mut buf = [0u8; 64];
            let n = link.recv_datagram(&mut buf, Duration::from_secs(5)).expect("recv host");
            assert_eq!(&buf[..n], b"ciao-p2p");
            link.send_datagram(b"pong-p2p").expect("send host");
            tx.send(link.max_datagram_size()).expect("send report");
            std::thread::sleep(Duration::from_millis(50));
            link.close(0, b"");
        });

        let client_link = client.connect_addr(host_addr, Duration::from_secs(5)).expect("connect minimal");
        client_link.send_datagram(b"ciao-p2p").expect("send client");
        let mut buf = [0u8; 64];
        let n = client_link.recv_datagram(&mut buf, Duration::from_secs(5)).expect("recv client");
        assert_eq!(&buf[..n], b"pong-p2p");
        let host_max = rx.recv_timeout(Duration::from_secs(5)).expect("host report");
        assert!(host_max.unwrap_or(0) >= 600);
        client_link.close(0, b"");
        acceptor.join().expect("join acceptor");
    }
}
