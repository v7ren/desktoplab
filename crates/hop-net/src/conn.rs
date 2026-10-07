//! One QUIC endpoint per machine. Motion is a datagram. Control and each file
//! transfer are their own streams, and transfer streams sit at a lower priority
//! so a multi-gigabyte copy does not stall the cursor.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hop_proto::{decode_frame, encode_frame, StreamMsg};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{RecvStream, SendStream};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, Error as TlsError, SignatureScheme};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

use crate::identity::Identity;

const ALPN: &[u8] = b"devhop";
pub const CONTROL_PRIORITY: i32 = 100;
pub const TRANSFER_PRIORITY: i32 = 0;

#[derive(Debug, Error)]
pub enum NetError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls: {0}")]
    Tls(String),
    #[error("connection: {0}")]
    Conn(String),
    #[error("endpoint closed")]
    Closed,
    #[error("mdns: {0}")]
    Mdns(String),
    #[error("identity: {0}")]
    Identity(#[from] crate::identity::IdentityError),
}

pub struct Endpoint {
    inner: quinn::Endpoint,
    client: quinn::ClientConfig,
    pub fingerprint: String,
}

impl Endpoint {
    pub fn bind(identity: &Identity, port: u16) -> Result<Self, NetError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let server = server_config(identity)?;
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let inner = quinn::Endpoint::server(server, addr)?;
        Ok(Self {
            inner,
            client: client_config(identity)?,
            fingerprint: identity.fingerprint.clone(),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.inner.local_addr().map_err(NetError::Io)
    }

    pub async fn accept(&self) -> Result<Link, NetError> {
        let incoming = self.inner.accept().await.ok_or(NetError::Closed)?;
        let conn = incoming
            .await
            .map_err(|err| NetError::Conn(err.to_string()))?;
        Link::from_conn(conn)
    }

    pub async fn dial(&self, addr: SocketAddr) -> Result<Link, NetError> {
        let connecting = self
            .inner
            .connect_with(self.client.clone(), addr, "devhop")
            .map_err(|err| NetError::Conn(err.to_string()))?;
        let conn = connecting
            .await
            .map_err(|err| NetError::Conn(err.to_string()))?;
        Link::from_conn(conn)
    }
}

/// The device with the lexicographically smaller id opens the connection.
pub fn should_dial(local_id: &str, remote_id: &str) -> bool {
    local_id < remote_id
}

#[derive(Clone)]
pub struct Link {
    conn: quinn::Connection,
    pub peer_fingerprint: String,
    pub peer_addr: SocketAddr,
}

impl Link {
    fn from_conn(conn: quinn::Connection) -> Result<Self, NetError> {
        let peer_addr = conn.remote_address();
        let peer_fingerprint = fingerprint_of(&conn)?;
        Ok(Self {
            conn,
            peer_fingerprint,
            peer_addr,
        })
    }

    pub fn send_datagram(&self, bytes: &[u8]) -> Result<(), NetError> {
        self.conn
            .send_datagram(Bytes::copy_from_slice(bytes))
            .map_err(|err| NetError::Conn(err.to_string()))
    }

    pub async fn read_datagram(&self) -> Result<Vec<u8>, NetError> {
        let data = self
            .conn
            .read_datagram()
            .await
            .map_err(|err| NetError::Conn(err.to_string()))?;
        Ok(data.to_vec())
    }

    pub async fn open_control(&self) -> Result<(SendStream, RecvStream), NetError> {
        self.open_bi(CONTROL_PRIORITY).await
    }

    pub async fn open_transfer(&self) -> Result<(SendStream, RecvStream), NetError> {
        self.open_bi(TRANSFER_PRIORITY).await
    }

    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), NetError> {
        self.conn
            .accept_bi()
            .await
            .map_err(|err| NetError::Conn(err.to_string()))
    }

    async fn open_bi(&self, priority: i32) -> Result<(SendStream, RecvStream), NetError> {
        let (send, recv) = self
            .conn
            .open_bi()
            .await
            .map_err(|err| NetError::Conn(err.to_string()))?;
        let _ = send.set_priority(priority);
        Ok((send, recv))
    }

    pub fn close(&self) {
        self.conn.close(0u32.into(), b"bye");
    }

    /// `quinn::Connection` is cheap to clone and is how a transfer task opens
    /// its own stream without blocking mouse datagrams.
    pub fn connection(&self) -> quinn::Connection {
        self.conn.clone()
    }
}

pub async fn write_msg(send: &mut SendStream, msg: &StreamMsg) -> Result<(), NetError> {
    let frame = encode_frame(msg).map_err(|err| NetError::Conn(err.to_string()))?;
    send.write_all(&frame)
        .await
        .map_err(|err| NetError::Conn(err.to_string()))?;
    send.flush()
        .await
        .map_err(|err| NetError::Conn(err.to_string()))?;
    Ok(())
}

pub struct FrameReader {
    buf: Vec<u8>,
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameReader {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub async fn next(&mut self, recv: &mut RecvStream) -> Result<Option<StreamMsg>, NetError> {
        loop {
            match decode_frame(&self.buf) {
                Ok(Some((msg, len))) => {
                    self.buf.drain(..len);
                    return Ok(Some(msg));
                }
                Ok(None) => {}
                Err(err) => return Err(NetError::Conn(err.to_string())),
            }
            let mut tmp = [0u8; 16 * 1024];
            match recv.read(&mut tmp).await {
                Ok(Some(n)) if n > 0 => self.buf.extend_from_slice(&tmp[..n]),
                Ok(_) => {
                    if self.buf.is_empty() {
                        return Ok(None);
                    }
                    return Err(NetError::Conn("truncated frame".into()));
                }
                Err(err) => return Err(NetError::Conn(err.to_string())),
            }
        }
    }
}

pub async fn read_msg(recv: &mut RecvStream) -> Result<Option<StreamMsg>, NetError> {
    FrameReader::new().next(recv).await
}

fn fingerprint_of(conn: &quinn::Connection) -> Result<String, NetError> {
    let identity = conn
        .peer_identity()
        .ok_or_else(|| NetError::Conn("peer presented no certificate".into()))?;
    let chain = identity
        .downcast::<Vec<CertificateDer<'static>>>()
        .map_err(|_| NetError::Conn("unexpected certificate type".into()))?;
    let first = chain
        .first()
        .ok_or_else(|| NetError::Conn("empty certificate chain".into()))?;
    Ok(crate::identity::fingerprint(first.as_ref()))
}

fn transport() -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        Duration::from_secs(30).try_into().expect("idle timeout"),
    ));
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    transport.datagram_receive_buffer_size(Some(1024 * 1024));
    transport.datagram_send_buffer_size(1024 * 1024);
    Arc::new(transport)
}

fn server_config(identity: &Identity) -> Result<quinn::ServerConfig, NetError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(AcceptSelfSigned {
        provider: Arc::clone(&provider),
    });
    let mut crypto = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|err| NetError::Tls(err.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![identity.certificate()], identity.private_key()?)
        .map_err(|err| NetError::Tls(err.to_string()))?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let quic = QuicServerConfig::try_from(crypto).map_err(|err| NetError::Tls(err.to_string()))?;
    let mut server = quinn::ServerConfig::with_crypto(Arc::new(quic));
    server.transport_config(transport());
    Ok(server)
}

fn client_config(identity: &Identity) -> Result<quinn::ClientConfig, NetError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(AcceptSelfSigned {
        provider: Arc::clone(&provider),
    });
    let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|err| NetError::Tls(err.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![identity.certificate()], identity.private_key()?)
        .map_err(|err| NetError::Tls(err.to_string()))?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let quic = QuicClientConfig::try_from(crypto).map_err(|err| NetError::Tls(err.to_string()))?;
    let mut client = quinn::ClientConfig::new(Arc::new(quic));
    client.transport_config(transport());
    Ok(client)
}

#[derive(Debug)]
struct AcceptSelfSigned {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl AcceptSelfSigned {
    fn check(der: &[u8]) -> Result<(), TlsError> {
        let (_, cert) = x509_parser::parse_x509_certificate(der)
            .map_err(|_| TlsError::General("certificate did not parse".into()))?;
        cert.verify_signature(None)
            .map_err(|_| TlsError::General("certificate is not self-signed".into()))?;
        Ok(())
    }
}

impl ServerCertVerifier for AcceptSelfSigned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Self::check(end_entity.as_ref())?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ClientCertVerifier for AcceptSelfSigned {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, TlsError> {
        Self::check(end_entity.as_ref())?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heartbeat::RttProbe;
    use crate::identity::Identity;
    use hop_proto::{encode_datagram, Datagram, StreamMsg};
    use std::time::Instant;

    async fn pair() -> (Link, Link) {
        let server_id = Identity::generate("server").unwrap();
        let client_id = Identity::generate("client").unwrap();
        let server = Endpoint::bind(&server_id, 0).unwrap();
        let mut addr = server.local_addr().unwrap();
        if addr.ip().is_unspecified() {
            addr.set_ip(std::net::Ipv4Addr::LOCALHOST.into());
        }
        let client = Endpoint::bind(&client_id, 0).unwrap();
        let dial = tokio::spawn(async move { client.dial(addr).await });
        let accepted = tokio::time::timeout(Duration::from_secs(4), server.accept()).await;
        let dialed = tokio::time::timeout(Duration::from_secs(4), dial).await;
        let server_link = match accepted {
            Ok(Ok(link)) => link,
            Ok(Err(err)) => panic!("accept failed: {err}"),
            Err(_) => panic!("accept timed out"),
        };
        let client_link = match dialed {
            Ok(Ok(Ok(link))) => link,
            Ok(Ok(Err(err))) => panic!("dial failed: {err}"),
            Ok(Err(err)) => panic!("dial task failed: {err}"),
            Err(_) => panic!("dial timed out"),
        };
        (client_link, server_link)
    }

    #[tokio::test]
    async fn loopback_measures_rtt_and_exchanges_a_hello() {
        let (client, server) = pair().await;
        assert_eq!(client.peer_fingerprint.len(), 64);
        let hello = StreamMsg::Hello {
            version: hop_proto::PROTO_VERSION,
            device_id: "c".into(),
            name: "client".into(),
            os: hop_proto::OsKind::Windows,
        };
        let client_io = client.clone();
        let writing = tokio::spawn(async move {
            let (mut cs, cr) = client_io.open_control().await.unwrap();
            write_msg(&mut cs, &hello).await.unwrap();
            (cs, cr)
        });
        let (ss, mut sr) = tokio::time::timeout(Duration::from_secs(2), server.accept_bi())
            .await
            .expect("accept control")
            .unwrap();
        let got = tokio::time::timeout(Duration::from_secs(2), read_msg(&mut sr))
            .await
            .expect("read hello")
            .unwrap()
            .unwrap();
        assert!(matches!(got, StreamMsg::Hello { device_id, .. } if device_id == "c"));
        let (_cs, cr) = writing.await.unwrap();
        let _ = (cr, ss);

        let mut probe = RttProbe::default();
        let start = Instant::now();
        let seq = 1u64;
        probe.sent(seq, 0);
        let beat = Datagram::Heartbeat {
            seq,
            echo: None,
            sent_us: 0,
        };
        client
            .send_datagram(&encode_datagram(&beat).unwrap())
            .unwrap();
        let raw = server.read_datagram().await.unwrap();
        let msg = hop_proto::decode_datagram(&raw).unwrap();
        let Datagram::Heartbeat { seq, .. } = msg else {
            panic!("expected heartbeat");
        };
        let rtt = start.elapsed();
        let sample = probe.echo(seq, rtt.as_micros() as u64).unwrap();
        assert!(sample < 500_000, "rtt {sample}us");
        assert!(probe.p95_us().unwrap() < 500_000);
    }

    #[tokio::test]
    async fn a_datagram_arrives_while_a_stream_is_busy() {
        let (client, server) = pair().await;
        let (mut send, _recv) = client.open_transfer().await.unwrap();
        let payload = vec![9u8; 256 * 1024];
        let writing = tokio::spawn(async move {
            for _ in 0..8 {
                if send.write_all(&payload).await.is_err() {
                    break;
                }
            }
        });
        let beat = encode_datagram(&Datagram::Motion {
            seq: 3,
            dx: 1,
            dy: 2,
            sent_us: 5,
        })
        .unwrap();
        client.send_datagram(&beat).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(2), server.read_datagram())
            .await
            .expect("datagram timed out behind the transfer stream")
            .unwrap();
        assert_eq!(got, beat);
        drop(server);
        let _ = writing.await;
    }

    #[test]
    fn the_lower_id_dials() {
        assert!(should_dial("a", "b"));
        assert!(!should_dial("b", "a"));
        assert!(!should_dial("same", "same"));
    }
}
