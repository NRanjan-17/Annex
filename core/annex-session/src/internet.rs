//! Sharing over the internet, end to end: get a code from the rendezvous server, wait for a viewer
//! to type it, open the router towards the viewer, and give it a relay address if it can't
//! connect directly. The apps and the CLI drive it the same way.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::events::{Event, EventSink};
use crate::host::HostHandle;
use crate::punch::TurnServer;
use crate::{SessionError, rendezvous};

/// Cloudflare's public STUN server: what the apps use unless told otherwise.
pub const DEFAULT_STUN: &str = "stun.cloudflare.com:3478";

/// A relay the user set up, as typed in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnRelay {
    /// `turn:host:port` (UDP).
    pub url: String,
    pub username: String,
    pub password: String,
}

/// Where the rendezvous and STUN servers are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternetServers {
    /// `wss://…` (or `ws://` for a server on this computer).
    pub rendezvous: String,
    /// `host:port`, e.g. [`DEFAULT_STUN`].
    pub stun: String,
}

/// A STUN server's address, IPv4 first (what home routers map).
pub async fn resolve_stun(stun: &str) -> Result<SocketAddr, SessionError> {
    let mut found: Vec<SocketAddr> = tokio::net::lookup_host(stun).await?.collect();
    found.sort_by_key(|a| !a.is_ipv4());
    found
        .into_iter()
        .next()
        .ok_or_else(|| SessionError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "STUN server not found")))
}

/// Shares `host` over the internet with one code, reporting through `events`
/// ([`Event::InternetCode`], then [`Event::InternetJoined`] and maybe [`Event::InternetRelay`],
/// and always [`Event::InternetCodeEnded`] last). Returns once the code is used up or expired;
/// abort the task to stop early.
pub async fn share_code(
    host: Arc<HostHandle>,
    name: String,
    servers: InternetServers,
    own_relay: Option<OwnRelay>,
    events: EventSink,
) {
    let reason = match share(&host, &name, &servers, own_relay, &events).await {
        Ok(reason) => reason.to_string(),
        Err(SessionError::Rejected(r)) if r == "code_expired" => "expired".into(),
        Err(e) => e.to_string(),
    };
    tracing::info!(reason, "internet code ended");
    events(Event::InternetCodeEnded { reason });
}

async fn share(
    host: &Arc<HostHandle>,
    name: &str,
    servers: &InternetServers,
    own_relay: Option<OwnRelay>,
    events: &EventSink,
) -> Result<&'static str, SessionError> {
    let own_relay = match own_relay {
        Some(r) => match rendezvous::turn_url_addr(&r.url).await {
            Some(addr) => Some(TurnServer { addr, username: r.username, password: r.password }),
            None => {
                return Err(SessionError::Violation(format!("can't use the relay {} (needs turn:host:port)", r.url)));
            }
        },
        None => None,
    };
    let addrs = host.internet_candidates(resolve_stun(&servers.stun).await?).await?;
    let device = host.fingerprint();
    let waiting = rendezvous::host(&servers.rendezvous, name, &addrs, own_relay.is_some(), Some(&device)).await?;
    host.set_punch_code(&waiting.code());
    tracing::info!(addrs = ?addrs, own_relay = own_relay.is_some(), "code ready; waiting for a viewer");
    events(Event::InternetCode { code: waiting.code(), expires_in: waiting.expires_in().as_secs() as u32 });
    let mut matched = waiting.wait().await?;
    let peer = matched.peer.clone();
    tracing::info!(name = %peer.name, addrs = ?peer.addrs, "a viewer joined by code");
    events(Event::InternetJoined { peer_name: peer.name.clone() });
    host.punch(peer.addrs.clone());

    // The viewer asks for the relay only if the direct connection fails.
    let Some(login) = matched.relay_request().await? else {
        tracing::info!("code used: the viewer didn't ask for the relay (it connected directly)");
        return Ok("used");
    };
    tracing::info!(our_relay = login.is_some(), own_relay = own_relay.is_some(), "the viewer asked for the relay");
    host.log_why_not_direct(&peer.addrs);
    let relay = match (own_relay, login) {
        (Some(own), _) => Some((own, false)),
        (None, Some(login)) => login.server().await.ok().map(|s| (s, true)),
        (None, None) => {
            tracing::info!("no relay to offer: the server gave no login (relay off, or today's allowance is used up)");
            None
        }
    };
    let relayed = match &relay {
        Some((relay, limited)) => {
            let ips = peer.addrs.iter().map(|a| a.ip()).collect();
            match host.relay(relay.clone(), ips, *limited).await {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::warn!("relay: {e}");
                    None
                }
            }
        }
        None => None,
    };
    tracing::info!(relayed = ?relayed, "answered the relay request");
    if relayed.is_some() {
        tracing::info!("switching to the relay: the session passes through it, still end to end encrypted (TLS 1.3)");
    }
    events(Event::InternetRelay { relayed: relayed.is_some(), limited: relay.is_some_and(|(_, l)| l) });
    matched.answer_relay(relayed).await?;
    Ok("used")
}
