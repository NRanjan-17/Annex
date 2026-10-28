//! The UDP socket under QUIC, with what internet connections need on top: it asks a STUN server
//! which public address the router gave it, sends punch packets that open the router towards a
//! peer, keeps STUN traffic away from QUIC, and can reach peers through a relay (TURN) when a
//! direct path isn't possible. Everything else QUIC sends and receives passes through unchanged,
//! so a LAN session behaves exactly as with a plain socket.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use tokio::sync::oneshot;

use crate::stun::{self, Message};

/// How long to wait for a STUN server's answer before trying once more.
const STUN_TRY: Duration = Duration::from_millis(700);
/// How long punch packets keep going: enough for the viewer's QUIC handshake to retry through.
pub const PUNCH_FOR: Duration = Duration::from_secs(6);
const PUNCH_EVERY: Duration = Duration::from_millis(200);
/// TURN permissions last 5 minutes (RFC 8656 §9); renew them, and the allocation, before then.
static RELAY_REFRESH_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(240_000);

/// Tests: refresh the relay this often instead (to see refreshes within a short test).
#[doc(hidden)]
pub fn set_relay_refresh(every: Duration) {
    RELAY_REFRESH_MS.store(every.as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
}

/// A relay (TURN server) and the login for it.
#[derive(Clone, PartialEq, Eq)]
pub struct TurnServer {
    pub addr: SocketAddr,
    pub username: String,
    pub password: String,
}

/// Without the password, so it can't reach the log.
impl std::fmt::Debug for TurnServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnServer").field("addr", &self.addr).field("username", &self.username).finish_non_exhaustive()
    }
}

/// The login for a relay, once its realm and nonce are known.
#[derive(Clone)]
struct Login {
    username: String,
    realm: String,
    nonce: Vec<u8>,
    key: [u8; 16],
}

/// The relay this socket has an allocation on.
struct Relay {
    server: SocketAddr,
    /// This socket's address on the relay.
    relayed: SocketAddr,
    /// Our relay, with its usage limits (sessions on it use less bandwidth), not the user's own.
    limited: bool,
    login: Login,
    /// IPs allowed to send through the relay (as `ip:0`), renewed with the allocation.
    allowed: Vec<SocketAddr>,
    /// Peers whose packets arrived through the relay; packets to them go back the same way.
    peers: HashSet<SocketAddr>,
}

/// What this computer's router does with ports, as two STUN answers show it. Only logged: it says
/// which side keeps a direct connection out, and never changes how Annex connects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Router {
    /// No address translation: the public address is this computer's own (IPv6).
    None,
    /// One public port for everyone: another computer can connect to the address a code carries.
    SamePort,
    /// A new public port for each address it talks to (a "hard" NAT, often an ISP sharing one
    /// address among many homes): another computer can't connect to it directly.
    NewPortEach,
    /// The second STUN answer didn't come.
    Unknown,
}

pub struct PunchSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    /// STUN/TURN answers awaited, by transaction id.
    waiting: Mutex<HashMap<stun::TransactionId, oneshot::Sender<Vec<u8>>>>,
    /// Addresses this socket punched towards: a session from one of them came over the internet.
    punched: Mutex<HashSet<SocketAddr>>,
    relay: Mutex<Option<Relay>>,
    /// Packets unwrapped from the relay, waiting for QUIC to read them: (from, data).
    relayed_in: Mutex<VecDeque<(SocketAddr, Vec<u8>)>>,
    /// Where punches from punched peers' IPs came from (for the log).
    heard: Mutex<HashSet<SocketAddr>>,
    /// This router's ports, once checked (for the log).
    router: Mutex<Option<Router>>,
}

impl fmt::Debug for PunchSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PunchSocket").field("inner", &self.inner).finish_non_exhaustive()
    }
}

impl PunchSocket {
    pub fn new(inner: Arc<dyn AsyncUdpSocket>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            waiting: Mutex::default(),
            punched: Mutex::default(),
            relay: Mutex::default(),
            relayed_in: Mutex::default(),
            heard: Mutex::default(),
            router: Mutex::default(),
        })
    }

    /// A real UDP socket (tokio).
    pub fn from_std(socket: UdpSocket) -> io::Result<Arc<Self>> {
        let runtime = quinn::default_runtime().ok_or_else(|| io::Error::other("no async runtime for QUIC"))?;
        Ok(Self::new(runtime.wrap_udp_socket(socket)?))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    /// Sends one datagram outside QUIC (STUN, punches).
    async fn send_raw(self: &Arc<Self>, to: SocketAddr, data: &[u8]) -> io::Result<()> {
        let to = self.for_socket(to)?;
        let mut poller = self.inner.clone().create_io_poller();
        let t = Transmit { destination: to, ecn: None, contents: data, segment_size: None, src_ip: None };
        std::future::poll_fn(|cx| {
            loop {
                match self.inner.try_send(&t) {
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => match poller.as_mut().poll_writable(cx) {
                        Poll::Ready(Ok(())) => continue,
                        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        Poll::Pending => return Poll::Pending,
                    },
                    r => return Poll::Ready(r),
                }
            }
        })
        .await
    }

    /// An IPv4 address must be written `::ffff:a.b.c.d` on a dual-stack socket, and an IPv6 one
    /// can't be reached from an IPv4 socket at all.
    fn for_socket(&self, to: SocketAddr) -> io::Result<SocketAddr> {
        match (self.local_addr()?.ip(), to.ip()) {
            (IpAddr::V6(_), IpAddr::V4(v4)) => Ok(SocketAddr::new(v4.to_ipv6_mapped().into(), to.port())),
            (IpAddr::V4(_), IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
                Some(v4) => Ok(SocketAddr::new(v4.into(), to.port())),
                None => Err(io::Error::new(io::ErrorKind::Unsupported, "IPv6 address on an IPv4 socket")),
            },
            _ => Ok(to),
        }
    }

    /// Sends a STUN/TURN request (made by `build` for a transaction id) and returns the answer,
    /// trying three times with the same id (so a server that already answered repeats itself
    /// rather than acting twice).
    async fn transact(
        self: &Arc<Self>,
        to: SocketAddr,
        build: impl Fn(stun::TransactionId) -> Vec<u8>,
    ) -> io::Result<Vec<u8>> {
        let id = rand_id();
        let request = build(id);
        for _ in 0..3 {
            let (tx, rx) = oneshot::channel();
            self.waiting.lock().unwrap().insert(id, tx);
            let sent = self.send_raw(to, &request).await;
            let answer = match sent {
                Ok(()) => tokio::time::timeout(STUN_TRY, rx).await.ok().and_then(Result::ok),
                Err(e) => {
                    self.waiting.lock().unwrap().remove(&id);
                    return Err(e);
                }
            };
            self.waiting.lock().unwrap().remove(&id);
            if let Some(a) = answer {
                return Ok(a);
            }
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, format!("{to} didn't answer")))
    }

    /// The public address and port the router gave this socket, as a STUN server sees it.
    pub async fn public_address(self: &Arc<Self>, stun_server: SocketAddr) -> io::Result<SocketAddr> {
        let answer = self.transact(stun_server, |id| stun::binding_request(&id)).await?;
        let a = stun::mapped_address(&answer).ok_or_else(|| io::Error::other("bad STUN answer"))?;
        let public = SocketAddr::new(a.ip().to_canonical(), a.port());
        tracing::debug!(%stun_server, %public, "public address from STUN");
        Ok(public)
    }

    /// Checks what this computer's router does with ports, for the log: asks `stun_server` once
    /// more on another port (53, or 3478 if that was 53) and compares the answer with `public`,
    /// the first one. A router that gives each address its own port answers differently.
    pub async fn check_router(self: &Arc<Self>, public: SocketAddr, stun_server: SocketAddr) -> Router {
        let router = if public.is_ipv6() {
            Router::None
        } else {
            let other = SocketAddr::new(stun_server.ip(), if stun_server.port() == 53 { 3478 } else { 53 });
            match self.public_address(other).await {
                Ok(second) if second == public => Router::SamePort,
                Ok(second) => {
                    tracing::debug!(%public, %second, "two STUN ports saw different public addresses");
                    Router::NewPortEach
                }
                Err(e) => {
                    tracing::debug!("second STUN check: {e}");
                    Router::Unknown
                }
            }
        };
        let what = match router {
            Router::None => "no address translation (IPv6): others can connect directly",
            Router::SamePort => "one public port for everyone (easy NAT): others can connect directly",
            Router::NewPortEach => "a new public port for each address (hard NAT, often a shared ISP address)",
            Router::Unknown => "couldn't tell how this router handles ports (no second STUN answer)",
        };
        let port_kept = self.local_addr().is_ok_and(|l| l.port() == public.port());
        tracing::info!(%public, port_kept, "network check: {what}");
        *self.router.lock().unwrap() = Some(router);
        router
    }

    /// Logs why a direct connection with `peers` didn't happen, as far as this side can tell: how
    /// this router handles ports ([`check_router`](Self::check_router)) and what arrived from the
    /// peers' IPs while punching.
    pub fn log_why_not_direct(&self, peers: &[SocketAddr]) {
        let router = *self.router.lock().unwrap();
        let given: HashSet<SocketAddr> = peers.iter().map(|&p| canonical(p)).collect();
        let ips: HashSet<IpAddr> = given.iter().map(|p| p.ip()).collect();
        let heard: Vec<SocketAddr> =
            self.heard.lock().unwrap().iter().filter(|h| ips.contains(&h.ip())).copied().collect();
        let theirs_moves = heard.iter().any(|h| !given.contains(h));
        let ours_moves = router == Some(Router::NewPortEach);
        let verdict = match (ours_moves, theirs_moves, heard.is_empty()) {
            (true, true, _) => "both networks block it (hard NAT on both sides)",
            (false, true, _) => "the other computer's network blocks it (hard NAT)",
            (true, false, _) => "this computer's network blocks it (hard NAT)",
            (false, false, true) => "nothing from the other computer got through (its network, or a firewall)",
            (false, false, false) => "its packets got through, but the connection didn't (a firewall?)",
        };
        tracing::info!(this_router = ?router, heard_from = ?heard, given = ?given, "why not direct: {verdict}");
    }

    /// A signed TURN request: `kind` with its own attributes (`allowed` for a permission), then
    /// the login and MESSAGE-INTEGRITY. A stale nonce (438) is replaced and the request sent once
    /// more; `login.nonce` keeps the new one.
    async fn signed(
        self: &Arc<Self>,
        server: SocketAddr,
        login: &mut Login,
        kind: u16,
        allowed: &[SocketAddr],
    ) -> io::Result<Vec<u8>> {
        for _ in 0..2 {
            let l = login.clone();
            let answer = self
                .transact(server, |id| {
                    let m = Message::new(kind, id);
                    let m = match kind {
                        stun::ALLOCATE_REQUEST => m.attr(stun::ATTR_REQUESTED_TRANSPORT, [17, 0, 0, 0]),
                        stun::REFRESH_REQUEST => m.attr(stun::ATTR_LIFETIME, 600u32.to_be_bytes()),
                        _ => allowed.iter().fold(m, |m, p| m.peer(stun::ATTR_XOR_PEER_ADDRESS, *p)),
                    };
                    m.attr(stun::ATTR_USERNAME, &l.username)
                        .attr(stun::ATTR_REALM, &l.realm)
                        .attr(stun::ATTR_NONCE, &l.nonce)
                        .encode_signed(&l.key)
                })
                .await?;
            match (stun::error_code(&answer), stun::attribute(&answer, stun::ATTR_NONCE)) {
                (Some(438), Some(nonce)) => login.nonce = nonce.to_vec(),
                _ => return Ok(answer),
            }
        }
        Err(io::Error::other("the relay kept rejecting its own nonce"))
    }

    /// Gets an address on `server` that relays to this socket, and lets `peers` (by IP address,
    /// any port) send to it. Keeps the allocation alive until the relay refuses (e.g. the login
    /// expired, which ends a relayed session: that's how our relay's time limit works) or this
    /// socket goes away. Returns the relayed address, for the peer to connect to.
    ///
    /// A relay allows one allocation per socket, so while one is live on the same server it's
    /// shared: the new peers are let in and the same address is returned (it lasts as long as the
    /// first login).
    pub async fn allocate_relay(
        self: &Arc<Self>,
        server: TurnServer,
        peers: Vec<IpAddr>,
        limited: bool,
    ) -> io::Result<SocketAddr> {
        let srv = canonical(server.addr);
        let peers: Vec<SocketAddr> = peers.into_iter().map(|ip| SocketAddr::new(ip.to_canonical(), 0)).collect();
        tracing::info!(server = %srv, ?peers, limited, "asking the relay for an address");
        let refused = |what: &str, r: &[u8]| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("the relay refused {what} ({})", stun::error_code(r).unwrap_or(0)),
            )
        };

        let live = self.relay.lock().unwrap().as_mut().filter(|r| r.server == srv).map(|r| {
            for p in &peers {
                if !r.allowed.contains(p) {
                    r.allowed.push(*p);
                }
            }
            (r.login.clone(), r.allowed.clone(), r.relayed)
        });
        if let Some((mut login, allowed, relayed)) = live {
            let answer = self.signed(srv, &mut login, stun::CREATE_PERMISSION_REQUEST, &allowed).await?;
            self.keep_nonce(&login);
            return match stun::error_code(&answer) {
                None => Ok(relayed),
                Some(_) => Err(refused("the other computer", &answer)),
            };
        }

        // The first request is unsigned; the 401 that comes back names the realm and a nonce.
        let challenge = self
            .transact(srv, |id| {
                Message::new(stun::ALLOCATE_REQUEST, id).attr(stun::ATTR_REQUESTED_TRANSPORT, [17, 0, 0, 0]).encode()
            })
            .await?;
        let realm =
            String::from_utf8_lossy(stun::attribute(&challenge, stun::ATTR_REALM).unwrap_or_default()).into_owned();
        let mut login = Login {
            key: stun::long_term_key(&server.username, &realm, &server.password),
            username: server.username,
            realm,
            nonce: stun::attribute(&challenge, stun::ATTR_NONCE).unwrap_or_default().to_vec(),
        };
        let answer = self.signed(srv, &mut login, stun::ALLOCATE_REQUEST, &[]).await?;
        if stun::message_type(&answer) != Some(stun::ALLOCATE_SUCCESS) || !stun::integrity_ok(&answer, &login.key) {
            return Err(refused("the login", &answer));
        }
        let relayed = stun::xor_address_attr(&answer, stun::ATTR_XOR_RELAYED_ADDRESS)
            .ok_or_else(|| io::Error::other("the relay gave no address"))?;
        let answer = self.signed(srv, &mut login, stun::CREATE_PERMISSION_REQUEST, &peers).await?;
        if stun::error_code(&answer).is_some() {
            return Err(refused("the other computer", &answer));
        }
        *self.relay.lock().unwrap() =
            Some(Relay { server: srv, relayed, limited, login, allowed: peers, peers: HashSet::new() });

        // Keep it alive: refresh the allocation and the permissions every few minutes.
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let every = RELAY_REFRESH_MS.load(std::sync::atomic::Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(every)).await;
                let Some(me) = me.upgrade() else { return };
                let Some((mut login, allowed)) =
                    me.relay.lock().unwrap().as_ref().map(|r| (r.login.clone(), r.allowed.clone()))
                else {
                    return;
                };
                let mut ok = matches!(me.signed(srv, &mut login, stun::REFRESH_REQUEST, &[]).await,
                    Ok(r) if stun::error_code(&r).is_none());
                if ok {
                    ok = matches!(me.signed(srv, &mut login, stun::CREATE_PERMISSION_REQUEST, &allowed).await,
                        Ok(r) if stun::error_code(&r).is_none());
                }
                me.keep_nonce(&login);
                if !ok {
                    tracing::info!("the relay ended this computer's allocation");
                    *me.relay.lock().unwrap() = None;
                    return;
                }
            }
        });
        Ok(relayed)
    }

    /// Remembers the relay's latest nonce for the next request.
    fn keep_nonce(&self, login: &Login) {
        if let Some(r) = self.relay.lock().unwrap().as_mut() {
            r.login.nonce = login.nonce.clone();
        }
    }

    /// `Some(limited)` if `peer`'s packets come through the relay (`limited`: our relay).
    pub fn relayed(&self, peer: SocketAddr) -> Option<bool> {
        self.relay.lock().unwrap().as_ref().filter(|r| r.peers.contains(&canonical(peer))).map(|r| r.limited)
    }

    /// Opens this computer's router towards `peers` for [`PUNCH_FOR`]: a small STUN indication to
    /// each, several times a second. The peers' own packets can then get in.
    pub fn punch(self: &Arc<Self>, peers: Vec<SocketAddr>) {
        let peers: Vec<SocketAddr> =
            peers.into_iter().map(|a| SocketAddr::new(a.ip().to_canonical(), a.port())).collect();
        tracing::debug!(?peers, "punching through to");
        self.punched.lock().unwrap().extend(peers.iter().copied());
        let me = self.clone();
        tokio::spawn(async move {
            let end = tokio::time::Instant::now() + PUNCH_FOR;
            while tokio::time::Instant::now() < end {
                for &p in &peers {
                    // Unreachable candidates (e.g. IPv6 from an IPv4-only socket) are skipped.
                    let _ = me.send_raw(p, &stun::binding_indication(&rand_id())).await;
                }
                tokio::time::sleep(PUNCH_EVERY).await;
            }
        });
    }

    /// Whether this socket punched towards `peer` (its session came over the internet).
    pub fn was_punched(&self, peer: SocketAddr) -> bool {
        self.punched.lock().unwrap().contains(&canonical(peer))
    }

    /// Takes a received STUN message out of QUIC's way: an awaited answer is handed over (true);
    /// punches and anything else are dropped.
    fn take_stun(&self, data: &[u8]) -> bool {
        if let Some(id) = stun::transaction_id(data)
            && let Some(tx) = self.waiting.lock().unwrap().remove(&id)
        {
            let _ = tx.send(data.to_vec());
            return true;
        }
        false
    }

    /// An unasked-for STUN packet (a punch) from `from`. The first from each address on a punched
    /// peer's IP is logged: from the address that peer gave, or from another port, which means
    /// the peer's router picks a new port for each address.
    fn note_punch(&self, from: SocketAddr) {
        let given: Vec<SocketAddr> =
            self.punched.lock().unwrap().iter().filter(|p| p.ip() == from.ip()).copied().collect();
        if given.is_empty() || !self.heard.lock().unwrap().insert(from) {
            return;
        }
        if given.contains(&from) {
            tracing::info!(%from, "network check: the other computer's punch got through, from the address it gave");
        } else {
            tracing::info!(
                %from,
                ?given,
                "network check: the other computer's packets come from another port than it gave: its router picks a \
                 new port for each address (hard NAT)"
            );
        }
    }

    /// One datagram from the relay server: a peer's packet (queued for QUIC, from the peer's
    /// address) or an answer to one of our requests.
    fn take_from_relay(&self, data: &[u8]) {
        if let Some((peer, payload)) = stun::data_indication(data) {
            let peer = canonical(peer);
            if let Some(r) = self.relay.lock().unwrap().as_mut() {
                r.peers.insert(peer);
            }
            let from = self.for_socket(peer).unwrap_or(peer);
            self.relayed_in.lock().unwrap().push_back((from, payload.to_vec()));
        } else if stun::is_stun(data) {
            let _ = self.take_stun(data);
        }
    }

    /// Hands queued relayed packets to QUIC, one per buffer.
    fn deliver_relayed(&self, bufs: &mut [IoSliceMut<'_>], meta: &mut [RecvMeta]) -> usize {
        let mut queue = self.relayed_in.lock().unwrap();
        let mut n = 0;
        while n < bufs.len().min(meta.len()) {
            let Some((from, data)) = queue.pop_front() else { break };
            if data.len() > bufs[n].len() {
                continue; // can't be a QUIC packet QUIC expects; drop it
            }
            bufs[n][..data.len()].copy_from_slice(&data);
            meta[n] = RecvMeta { addr: from, len: data.len(), stride: data.len(), ecn: None, dst_ip: None };
            n += 1;
        }
        n
    }

    fn relay_server(&self) -> Option<SocketAddr> {
        self.relay.lock().unwrap().as_ref().map(|r| r.server)
    }
}

fn canonical(a: SocketAddr) -> SocketAddr {
    SocketAddr::new(a.ip().to_canonical(), a.port())
}

fn rand_id() -> stun::TransactionId {
    let mut id = [0u8; 12];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut id).expect("system randomness");
    id
}

impl AsyncUdpSocket for PunchSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        self.inner.clone().create_io_poller()
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        let server = match self.relay.lock().unwrap().as_ref() {
            Some(r) if r.peers.contains(&canonical(transmit.destination)) => r.server,
            _ => return self.inner.try_send(transmit),
        };
        // To a relayed peer: each packet goes to the relay in a Send indication.
        let to = self.for_socket(server)?;
        let size = transmit.segment_size.unwrap_or(transmit.contents.len()).max(1);
        for (i, packet) in transmit.contents.chunks(size).enumerate() {
            let wrapped = stun::send_indication(rand_id(), canonical(transmit.destination), packet);
            let t =
                Transmit { destination: to, ecn: transmit.ecn, contents: &wrapped, segment_size: None, src_ip: None };
            match self.inner.try_send(&t) {
                // Nothing sent yet: QUIC waits and tries again. Partway through: the rest count as
                // lost, which QUIC recovers from.
                Err(e) if i == 0 => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            let delivered = self.deliver_relayed(bufs, meta);
            if delivered > 0 {
                return Poll::Ready(Ok(delivered));
            }
            let n = match self.inner.poll_recv(cx, bufs, meta) {
                Poll::Ready(Ok(n)) => n,
                other => return other,
            };
            // Keep QUIC's datagrams, moving them down over any STUN ones taken out. Everything from
            // the relay server is TURN (its relayed packets would look like QUIC by their first
            // byte), possibly several coalesced (GRO); from anyone else, a batch of segments is
            // one flow of equal-sized packets, never STUN.
            let relay = self.relay_server();
            let mut kept = 0;
            for i in 0..n {
                let m = meta[i];
                if relay.is_some_and(|r| canonical(m.addr) == r) {
                    for packet in bufs[i][..m.len].chunks(m.stride.max(1)) {
                        self.take_from_relay(packet);
                    }
                    continue;
                }
                if m.len == m.stride && stun::is_stun(&bufs[i][..m.len]) {
                    if !self.take_stun(&bufs[i][..m.len]) {
                        self.note_punch(canonical(m.addr));
                    }
                    continue;
                }
                if kept != i {
                    let (to, from) = bufs.split_at_mut(i);
                    to[kept][..m.len].copy_from_slice(&from[0][..m.len]);
                    meta[kept] = m;
                }
                kept += 1;
            }
            if kept > 0 {
                return Poll::Ready(Ok(kept));
            }
            // Only STUN/TURN arrived: deliver anything unwrapped, else look again (the inner socket
            // registers the waker if nothing's left).
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}

/// This computer's own addresses with `port`, besides the public one STUN finds: its main IPv6
/// address when that's global (two IPv6 homes then need no punching at all), and its main
/// home-network IPv4 address, for a viewer on the same network (most routers won't loop a
/// connection to their own public address back inside). The server passes that one on only to a
/// computer connecting from the same public address. Found by asking the system which address it
/// would use towards the internet; nothing is sent.
pub fn own_addresses(port: u16) -> Vec<SocketAddr> {
    let towards = |bind: &str, to: &str| {
        let s = UdpSocket::bind(bind).ok()?;
        s.connect(to).ok()?;
        s.local_addr().ok().map(|a| a.ip())
    };
    let mut out = Vec::new();
    if let Some(IpAddr::V6(v6)) = towards("[::]:0", "[2001:db8::1]:9")
        && is_global_v6(&v6)
    {
        out.push(SocketAddr::new(IpAddr::V6(v6), port));
    }
    if let Some(IpAddr::V4(v4)) = towards("0.0.0.0:0", "192.0.2.1:9")
        && v4.is_private()
    {
        out.push(SocketAddr::new(IpAddr::V4(v4), port));
    }
    out
}

/// Global unicast (2000::/3), minus the documentation range 2001:db8::/32. Excludes link-local,
/// unique-local and mapped IPv4.
fn is_global_v6(ip: &std::net::Ipv6Addr) -> bool {
    let s = ip.segments();
    s[0] & 0xe000 == 0x2000 && !(s[0] == 0x2001 && s[1] == 0x0db8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_global_ipv6_counts_as_public() {
        for (ip, global) in [
            ("2a02:8070:1::5", true),
            ("2001:4860::8888", true),
            ("2001:db8::1", false),
            ("fe80::1", false),
            ("fd12:3456::1", false),
            ("::ffff:192.168.1.5", false),
            ("::1", false),
        ] {
            assert_eq!(is_global_v6(&ip.parse().unwrap()), global, "{ip}");
        }
    }
}
