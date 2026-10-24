//! Client for the rendezvous server (`server/rendezvous`), which introduces two computers for an
//! internet connection. The sharing computer gets a short code and waits; the viewer joins with
//! the code; each learns the other's addresses. The session itself never goes through the server.
//!
//! The connection is a WebSocket, over TLS checked against the system's trusted certificates
//! (`wss://`), or plain for a server on this computer (`ws://`, development only).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::SessionError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Keeps the waiting host's connection open through routers (the server answers pings without
/// waking up, so waiting costs nothing).
const PING_EVERY: Duration = Duration::from_secs(30);

/// The other computer, as the rendezvous server describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub name: String,
    pub addrs: Vec<SocketAddr>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Msg {
    Code { code: String, expires_in: u32 },
    Hello { name: String, addrs: Vec<String> },
    Join { name: String, addrs: Vec<String> },
    Peer { name: String, addrs: Vec<String> },
    Expired,
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

type Socket = WebSocketStream<Box<dyn Io>>;

/// The sharing computer's registration: a code to show, valid until a viewer uses it or it expires.
pub struct HostCode {
    ws: Socket,
    code: String,
    expires_in: Duration,
}

impl HostCode {
    /// The code as people read it: `K7P-4QX`.
    pub fn code(&self) -> String {
        let (a, b) = self.code.split_at(self.code.len() / 2);
        format!("{a}-{b}")
    }

    pub fn expires_in(&self) -> Duration {
        self.expires_in
    }

    /// Waits for a viewer to join. Errors with `Rejected("code_expired")` when the code runs out.
    pub async fn wait(mut self) -> Result<Peer, SessionError> {
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        loop {
            tokio::select! {
                m = self.ws.next() => match m {
                    Some(Ok(Message::Text(t))) => match serde_json::from_str::<Msg>(&t) {
                        Ok(Msg::Peer { name, addrs }) => return Ok(peer(name, addrs)),
                        Ok(Msg::Expired) => return Err(SessionError::Rejected("code_expired".into())),
                        _ => {}
                    },
                    Some(Ok(Message::Close(_))) | None => return Err(SessionError::Closed),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(ws_error(e)),
                },
                _ = ping.tick() => self.ws.send(Message::Ping(Default::default())).await.map_err(ws_error)?,
            }
        }
    }
}

/// Registers this sharing computer: asks for a code, then says where it can be reached.
pub async fn host(server: &str, name: &str, addrs: &[SocketAddr]) -> Result<HostCode, SessionError> {
    let mut ws = open(server, "/v1/host").await?;
    let (code, expires_in) = match next_msg(&mut ws).await? {
        Msg::Code { code, expires_in } => (code, Duration::from_secs(expires_in.into())),
        _ => return Err(SessionError::Violation("rendezvous: expected a code".into())),
    };
    send(&mut ws, &Msg::Hello { name: name.into(), addrs: strings(addrs) }).await?;
    Ok(HostCode { ws, code, expires_in })
}

/// Joins a sharing computer by its code. Errors with `Rejected("unknown_code")` for a wrong,
/// used or expired code, and `Rejected("too_many_attempts")` after too many tries.
pub async fn join(server: &str, code: &str, name: &str, addrs: &[SocketAddr]) -> Result<Peer, SessionError> {
    let code: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let mut ws = open(server, &format!("/v1/join/{code}")).await?;
    send(&mut ws, &Msg::Join { name: name.into(), addrs: strings(addrs) }).await?;
    match next_msg(&mut ws).await? {
        Msg::Peer { name, addrs } => Ok(peer(name, addrs)),
        _ => Err(SessionError::Violation("rendezvous: expected the other computer".into())),
    }
}

fn strings(addrs: &[SocketAddr]) -> Vec<String> {
    addrs.iter().take(8).map(|a| SocketAddr::new(a.ip().to_canonical(), a.port()).to_string()).collect()
}

/// Addresses that don't parse are dropped (the server only relays what the other side sent).
fn peer(name: String, addrs: Vec<String>) -> Peer {
    Peer { name, addrs: addrs.iter().filter_map(|a| a.parse().ok()).collect() }
}

async fn send(ws: &mut Socket, m: &Msg) -> Result<(), SessionError> {
    let text = serde_json::to_string(m).expect("serializable");
    ws.send(Message::text(text)).await.map_err(ws_error)
}

async fn next_msg(ws: &mut Socket) -> Result<Msg, SessionError> {
    loop {
        match timeout(CONNECT_TIMEOUT, ws.next()).await? {
            Some(Ok(Message::Text(t))) => {
                return serde_json::from_str(&t)
                    .map_err(|e| SessionError::Violation(format!("rendezvous: bad message: {e}")));
            }
            Some(Ok(Message::Close(f))) => {
                let expired = f.is_some_and(|f| u16::from(f.code) == 4410);
                return Err(if expired { SessionError::Rejected("code_expired".into()) } else { SessionError::Closed });
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(ws_error(e)),
            None => return Err(SessionError::Closed),
        }
    }
}

fn ws_error(e: tungstenite::Error) -> SessionError {
    match e {
        tungstenite::Error::Http(r) if r.status() == 404 => SessionError::Rejected("unknown_code".into()),
        tungstenite::Error::Http(r) if r.status() == 429 => SessionError::Rejected("too_many_attempts".into()),
        tungstenite::Error::Http(r) => SessionError::Violation(format!("rendezvous answered {}", r.status())),
        tungstenite::Error::Io(e) => SessionError::Io(e),
        e => SessionError::Violation(format!("rendezvous: {e}")),
    }
}

async fn open(server: &str, path: &str) -> Result<Socket, SessionError> {
    let bad = || SessionError::Violation(format!("rendezvous address isn't a ws:// or wss:// address: {server}"));
    let uri: tungstenite::http::Uri = server.trim_end_matches('/').parse().map_err(|_| bad())?;
    let tls = match uri.scheme_str() {
        Some("wss") => true,
        Some("ws") => false,
        _ => return Err(bad()),
    };
    let host = uri.host().ok_or_else(bad)?.trim_start_matches('[').trim_end_matches(']').to_string();
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    let tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port))).await??;
    let _ = tcp.set_nodelay(true);
    let stream: Box<dyn Io> = if tls {
        let name = ServerName::try_from(host).map_err(|_| bad())?;
        let connector = tokio_rustls::TlsConnector::from(tls_config()?);
        Box::new(timeout(CONNECT_TIMEOUT, connector.connect(name, tcp)).await??)
    } else {
        Box::new(tcp)
    };
    let url = format!("{}{path}", server.trim_end_matches('/'));
    let (ws, _) = timeout(CONNECT_TIMEOUT, tokio_tungstenite::client_async(url, stream)).await?.map_err(ws_error)?;
    Ok(ws)
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, SessionError> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(|b| b.with_platform_verifier())
        .map_err(|e| SessionError::Violation(format!("tls: {e}")))?
        .with_no_client_auth();
    Ok(Arc::new(config))
}
