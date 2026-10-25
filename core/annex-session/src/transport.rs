//! Connection setup for both transports. Everything above this module sees the same thing: a
//! reliable control channel (reader/writer), the peer's certificate fingerprint, the TLS
//! exporter for pairing, and — on QUIC — an unreliable datagram channel for video.
//!
//! - **QUIC** (preferred): TLS 1.3 inside QUIC, BBR congestion control, video as FEC-protected
//!   datagrams (`annex_proto::fragment`), control on one bidirectional stream.
//! - **TCP + TLS 1.3**: fallback when UDP is blocked; video records on the same stream.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use annex_crypto::{pairing::EXPORTER_LABEL, peer_fingerprint};
use rustls::pki_types::{CertificateDer, ServerName};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::{Core, SessionError};

pub(crate) type BoxRead = Box<dyn AsyncRead + Unpin + Send>;
pub(crate) type BoxWrite = Box<dyn AsyncWrite + Unpin + Send>;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Quic,
    Tcp,
}

/// An authenticated connection, ready for Hello.
pub(crate) struct Established {
    pub kind: TransportKind,
    pub reader: BoxRead,
    pub writer: BoxWrite,
    pub fingerprint: String,
    pub exporter: [u8; 32],
    pub remote: SocketAddr,
    /// The QUIC connection (datagrams, RTT); None on TCP.
    pub quic: Option<quinn::Connection>,
}

pub(crate) fn quic_transport() -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(2)));
    t.max_idle_timeout(Some(Duration::from_secs(15).try_into().expect("valid idle timeout")));
    t.datagram_receive_buffer_size(Some(8 << 20));
    t.datagram_send_buffer_size(4 << 20);
    t.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    Arc::new(t)
}

pub(crate) fn quic_server_config(core: &Core) -> Result<quinn::ServerConfig, SessionError> {
    let tls = annex_crypto::server_config(core.identity())?;
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .map_err(|e| SessionError::Violation(format!("quic tls config: {e}")))?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    cfg.transport_config(quic_transport());
    Ok(cfg)
}

fn quic_peer_fingerprint(conn: &quinn::Connection) -> Option<String> {
    let identity = conn.peer_identity()?;
    let certs = identity.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    peer_fingerprint(Some(certs.as_slice()))
}

fn quic_exporter(conn: &quinn::Connection) -> Result<[u8; 32], SessionError> {
    let mut out = [0u8; 32];
    conn.export_keying_material(&mut out, EXPORTER_LABEL, b"")
        .map_err(|_| SessionError::Violation("tls exporter unavailable".into()))?;
    Ok(out)
}

/// Host side of a QUIC connection: wait for the viewer's control stream.
pub(crate) async fn accept_quic(incoming: quinn::Incoming) -> Result<Established, SessionError> {
    let remote = canonical(incoming.remote_address());
    let conn = timeout(HANDSHAKE_TIMEOUT, incoming)
        .await
        .map_err(|_| SessionError::Timeout("quic handshake"))?
        .map_err(|e| SessionError::Io(std::io::Error::other(e)))?;
    let fingerprint =
        quic_peer_fingerprint(&conn).ok_or_else(|| SessionError::Violation("peer sent no certificate".into()))?;
    let exporter = quic_exporter(&conn)?;
    let (send, recv) = timeout(HANDSHAKE_TIMEOUT, conn.accept_bi())
        .await
        .map_err(|_| SessionError::Timeout("control stream"))?
        .map_err(|e| SessionError::Io(std::io::Error::other(e)))?;
    Ok(Established {
        kind: TransportKind::Quic,
        reader: Box::new(recv),
        writer: Box::new(send),
        fingerprint,
        exporter,
        remote,
        quic: Some(conn),
    })
}

/// Viewer side of a QUIC connection.
pub(crate) async fn connect_quic(
    core: &Core,
    addr: SocketAddr,
    expected: Option<String>,
) -> Result<Established, SessionError> {
    let bind: SocketAddr = if addr.is_ipv6() { "[::]:0".parse().unwrap() } else { "0.0.0.0:0".parse().unwrap() };
    let endpoint = quinn::Endpoint::client(bind)?;
    connect_quic_on(core, &endpoint, addr, expected).await
}

/// Viewer side of a QUIC connection on a given endpoint (an internet connection's socket).
pub(crate) async fn connect_quic_on(
    core: &Core,
    endpoint: &quinn::Endpoint,
    addr: SocketAddr,
    expected: Option<String>,
) -> Result<Established, SessionError> {
    let tls = annex_crypto::client_config(core.identity(), expected)?;
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|e| SessionError::Violation(format!("quic tls config: {e}")))?;
    let mut client = quinn::ClientConfig::new(Arc::new(crypto));
    client.transport_config(quic_transport());
    let connecting = endpoint
        .connect_with(client, addr, annex_crypto::SERVER_NAME)
        .map_err(|e| SessionError::Io(std::io::Error::other(e)))?;
    let conn = timeout(HANDSHAKE_TIMEOUT, connecting)
        .await
        .map_err(|_| SessionError::Timeout("quic handshake"))?
        .map_err(|e| SessionError::Io(std::io::Error::other(e)))?;
    let fingerprint =
        quic_peer_fingerprint(&conn).ok_or_else(|| SessionError::Violation("host sent no certificate".into()))?;
    let exporter = quic_exporter(&conn)?;
    let (send, recv) = conn.open_bi().await.map_err(|e| SessionError::Io(std::io::Error::other(e)))?;
    Ok(Established {
        kind: TransportKind::Quic,
        reader: Box::new(recv),
        writer: Box::new(send),
        fingerprint,
        exporter,
        remote: addr,
        quic: Some(conn),
    })
}

/// Host side of a TCP connection.
pub(crate) async fn accept_tcp(sock: TcpStream, tls: tokio_rustls::TlsAcceptor) -> Result<Established, SessionError> {
    let _ = sock.set_nodelay(true);
    let remote = canonical(sock.peer_addr()?);
    let stream = timeout(HANDSHAKE_TIMEOUT, tls.accept(sock)).await.map_err(|_| SessionError::Timeout("tls"))??;
    let fingerprint = peer_fingerprint(stream.get_ref().1.peer_certificates())
        .ok_or_else(|| SessionError::Violation("peer sent no certificate".into()))?;
    let exporter = stream
        .get_ref()
        .1
        .export_keying_material([0u8; 32], EXPORTER_LABEL, None)
        .map_err(|e| SessionError::Violation(format!("tls exporter: {e}")))?;
    let (r, w) = tokio::io::split(stream);
    Ok(Established {
        kind: TransportKind::Tcp,
        reader: Box::new(r),
        writer: Box::new(w),
        fingerprint,
        exporter,
        remote,
        quic: None,
    })
}

/// Viewer side of a TCP connection.
pub(crate) async fn connect_tcp(
    core: &Core,
    addr: SocketAddr,
    expected: Option<String>,
) -> Result<Established, SessionError> {
    let sock = timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| SessionError::Timeout("connect"))??;
    sock.set_nodelay(true)?;
    let tls_cfg = annex_crypto::client_config(core.identity(), expected)?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_cfg));
    let name = ServerName::try_from(annex_crypto::SERVER_NAME).expect("valid server name");
    let tls =
        timeout(HANDSHAKE_TIMEOUT, connector.connect(name, sock)).await.map_err(|_| SessionError::Timeout("tls"))??;
    let fingerprint = peer_fingerprint(tls.get_ref().1.peer_certificates())
        .ok_or_else(|| SessionError::Violation("host sent no certificate".into()))?;
    let exporter = tls
        .get_ref()
        .1
        .export_keying_material([0u8; 32], EXPORTER_LABEL, None)
        .map_err(|e| SessionError::Violation(format!("tls exporter: {e}")))?;
    let (r, w) = tokio::io::split(tls);
    Ok(Established {
        kind: TransportKind::Tcp,
        reader: Box::new(r),
        writer: Box::new(w),
        fingerprint,
        exporter,
        remote: addr,
        quic: None,
    })
}

/// An IPv4 viewer on the host's dual-stack socket shows up as `::ffff:a.b.c.d`; this gives back
/// `a.b.c.d`, so logs and the link check see the address the viewer used.
pub(crate) fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// True for addresses that only exist on a directly attached link (e.g. a USB-C/Thunderbolt or
/// Ethernet cable between the two computers with no router): IPv4 169.254/16, IPv6 fe80::/10.
pub fn is_link_local(addr: &SocketAddr) -> bool {
    match addr {
        SocketAddr::V4(a) => a.ip().is_link_local(),
        SocketAddr::V6(a) => (a.ip().segments()[0] & 0xffc0) == 0xfe80,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_local_detection() {
        assert!(is_link_local(&"169.254.10.2:1".parse().unwrap()));
        assert!(is_link_local(&"[fe80::1%4]:1".parse().unwrap()));
        assert!(!is_link_local(&"192.168.1.2:1".parse().unwrap()));
        assert!(!is_link_local(&"[2001:db8::1]:1".parse().unwrap()));
        // A cable's IPv4 address arriving on the dual-stack listener.
        assert!(is_link_local(&canonical("[::ffff:169.254.10.2]:1".parse().unwrap())));
        assert_eq!(canonical("[::ffff:192.168.1.2]:7".parse().unwrap()).to_string(), "192.168.1.2:7");
    }
}
