//! Internet connections through a simulated internet: each computer sits behind its own home
//! router (NAT), a STUN server tells each one its public address, and a TURN server relays when
//! the routers won't allow a direct path. Nothing touches the real network, so the routers behave
//! exactly as configured, every run.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use annex_session::*;
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use tokio::sync::mpsc;

const STUN: &str = "198.51.100.1:3478";
const TURN: &str = "198.51.100.2:3478";
const TURN_USER: &str = "annex";

/// The test relay's password: made up per run, so no password sits in the source.
fn turn_pass() -> &'static str {
    static PASS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PASS.get_or_init(|| {
        let mut bytes = [0u8; 12];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes).unwrap();
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    })
}

/// How a router maps and filters, the two kinds that matter for punching.
#[derive(Clone, Copy)]
enum Nat {
    /// Most home routers: one public port per inside socket, whoever it talks to; replies only
    /// from an address and port it has sent to.
    PortRestricted,
    /// Some carrier and office networks: a new public port for every destination. The port the
    /// STUN server saw is useless to anyone else, so punching can't work (that needs a relay).
    Symmetric,
}

struct Router {
    kind: Nat,
    public_ip: std::net::IpAddr,
    next_port: u16,
    /// (inside socket, destination or None for endpoint-independent) → public port.
    out: HashMap<(SocketAddr, Option<SocketAddr>), u16>,
    /// public port → inside socket, and the far ends it has sent to from that port.
    inbound: HashMap<u16, (SocketAddr, HashSet<SocketAddr>)>,
}

#[derive(Default)]
struct NetState {
    /// Sockets reachable at their own address (public hosts, and inside sockets by private address).
    sockets: HashMap<SocketAddr, Weak<SimSocket>>,
    routers: Vec<Router>,
    /// Which router each inside socket is behind.
    behind: HashMap<SocketAddr, usize>,
}

#[derive(Default)]
struct Net(Mutex<NetState>);

impl Net {
    fn router(self: &Arc<Self>, kind: Nat, public_ip: &str) -> usize {
        let mut s = self.0.lock().unwrap();
        s.routers.push(Router {
            kind,
            public_ip: public_ip.parse().unwrap(),
            next_port: 40000,
            out: HashMap::new(),
            inbound: HashMap::new(),
        });
        s.routers.len() - 1
    }

    fn socket(self: &Arc<Self>, addr: &str, router: Option<usize>) -> Arc<SimSocket> {
        let addr: SocketAddr = addr.parse().unwrap();
        let sock = Arc::new(SimSocket { net: Arc::downgrade(self), addr, inbox: Mutex::default() });
        let mut s = self.0.lock().unwrap();
        s.sockets.insert(addr, Arc::downgrade(&sock));
        if let Some(r) = router {
            s.behind.insert(addr, r);
        }
        sock
    }

    fn send(&self, from: SocketAddr, to: SocketAddr, data: &[u8]) {
        let mut s = self.0.lock().unwrap();
        // Out through the sender's router: the packet leaves from a public address and port.
        let src = match s.behind.get(&from).copied() {
            Some(r) => {
                let router = &mut s.routers[r];
                let key = (from, matches!(router.kind, Nat::Symmetric).then_some(to));
                let port = match router.out.get(&key) {
                    Some(&p) => p,
                    None => {
                        router.next_port += 1;
                        let p = router.next_port;
                        router.out.insert(key, p);
                        p
                    }
                };
                router.inbound.entry(port).or_insert_with(|| (from, HashSet::new())).1.insert(to);
                SocketAddr::new(router.public_ip, port)
            }
            None => from,
        };
        // In through the receiver's router, if `to` is one of its public addresses.
        let dest = match s.routers.iter().find(|r| r.public_ip == to.ip()) {
            Some(router) => match router.inbound.get(&to.port()) {
                Some((inside, allowed)) if allowed.contains(&src) => *inside,
                _ => return, // unsolicited: dropped
            },
            None => to,
        };
        if let Some(sock) = s.sockets.get(&dest).and_then(Weak::upgrade) {
            drop(s);
            sock.deliver(src, data);
        }
    }
}

#[derive(Default)]
struct Inbox {
    queue: VecDeque<(SocketAddr, Vec<u8>)>,
    waker: Option<Waker>,
}

struct SimSocket {
    net: Weak<Net>,
    addr: SocketAddr,
    inbox: Mutex<Inbox>,
}

impl std::fmt::Debug for SimSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SimSocket({})", self.addr)
    }
}

impl SimSocket {
    fn deliver(&self, from: SocketAddr, data: &[u8]) {
        let mut inbox = self.inbox.lock().unwrap();
        inbox.queue.push_back((from, data.to_vec()));
        if let Some(w) = inbox.waker.take() {
            w.wake();
        }
    }

    async fn recv(&self) -> (SocketAddr, Vec<u8>) {
        std::future::poll_fn(|cx| {
            let mut inbox = self.inbox.lock().unwrap();
            match inbox.queue.pop_front() {
                Some(p) => Poll::Ready(p),
                None => {
                    inbox.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
        .await
    }
}

#[derive(Debug)]
struct AlwaysWritable;

impl UdpPoller for AlwaysWritable {
    fn poll_writable(self: Pin<&mut Self>, _: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncUdpSocket for SimSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(AlwaysWritable)
    }

    fn try_send(&self, t: &Transmit) -> io::Result<()> {
        if let Some(net) = self.net.upgrade() {
            let size = t.segment_size.unwrap_or(t.contents.len());
            for chunk in t.contents.chunks(size) {
                net.send(self.addr, t.destination, chunk);
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
        let mut inbox = self.inbox.lock().unwrap();
        let Some((from, data)) = inbox.queue.pop_front() else {
            inbox.waker = Some(cx.waker().clone());
            return Poll::Pending;
        };
        bufs[0][..data.len()].copy_from_slice(&data);
        meta[0] = RecvMeta { addr: from, len: data.len(), stride: data.len(), ..Default::default() };
        Poll::Ready(Ok(1))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.addr)
    }
}

/// A STUN server on the simulated internet: answers each Binding request with the address it came from.
fn stun_server(net: &Arc<Net>) {
    let sock = net.socket(STUN, None);
    let net = Arc::downgrade(net);
    tokio::spawn(async move {
        loop {
            let (from, data) = sock.recv().await;
            if let (true, Some(id), Some(net)) =
                (stun::is_binding_request(&data), stun::transaction_id(&data), net.upgrade())
            {
                net.send(sock.addr, from, &stun::binding_success(&id, from));
            }
        }
    });
}

/// A relay socket, and the IPs allowed to send to it.
type Allocation = (Arc<SimSocket>, Arc<Mutex<HashSet<IpAddr>>>);

/// A TURN server (RFC 8656) on the simulated internet: the parts a relay client uses, with a
/// long-term login (realm "annex.test"). Like real servers it changes its nonce now and then
/// (every 3 requests here) and allows one allocation per client address. Returns how many
/// refreshes it granted.
fn turn_server(net: &Arc<Net>) -> Arc<std::sync::atomic::AtomicUsize> {
    let refreshes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = refreshes.clone();
    let sock = net.socket(TURN, None);
    let weak = Arc::downgrade(net);
    let key = stun::long_term_key(TURN_USER, "annex.test", turn_pass());
    tokio::spawn(async move {
        // client address → (its relay socket, IPs allowed to send to it)
        let allocations: Arc<Mutex<HashMap<SocketAddr, Allocation>>> = Arc::default();
        let mut next_port = 50000u16;
        let (mut nonce, mut accepted) = (1u32, 0u32);
        loop {
            let (client, data) = sock.recv().await;
            let Some(net) = weak.upgrade() else { return };
            let (Some(kind), Some(id)) = (stun::message_type(&data), stun::transaction_id(&data)) else { continue };
            let reply = |m: stun::Message| net.send(sock.addr, client, &m.encode_signed(&key));
            if kind == stun::SEND_INDICATION {
                let allocs = allocations.lock().unwrap();
                if let (Some((relay, allowed)), Some(peer), Some(payload)) = (
                    allocs.get(&client),
                    stun::xor_address_attr(&data, stun::ATTR_XOR_PEER_ADDRESS),
                    stun::attribute(&data, stun::ATTR_DATA),
                ) && allowed.lock().unwrap().contains(&peer.ip())
                {
                    net.send(relay.addr, peer, payload);
                }
                continue;
            }
            if !stun::integrity_ok(&data, &key) {
                // No or wrong login: ask for one (wrong logins fail again the same way).
                let m = stun::Message::new(kind | stun::ERROR_CLASS, id)
                    .attr(stun::ATTR_ERROR_CODE, [0, 0, 4, 1])
                    .attr(stun::ATTR_REALM, "annex.test")
                    .attr(stun::ATTR_NONCE, format!("nonce-{nonce}"));
                net.send(sock.addr, client, &m.encode());
                continue;
            }
            if stun::attribute(&data, stun::ATTR_NONCE) != Some(format!("nonce-{nonce}").as_bytes()) {
                let m = stun::Message::new(kind | stun::ERROR_CLASS, id)
                    .attr(stun::ATTR_ERROR_CODE, [0, 0, 4, 38])
                    .attr(stun::ATTR_NONCE, format!("nonce-{nonce}"));
                net.send(sock.addr, client, &m.encode());
                continue;
            }
            accepted += 1;
            if accepted % 3 == 0 {
                nonce += 1;
            }
            match kind {
                stun::ALLOCATE_REQUEST if allocations.lock().unwrap().contains_key(&client) => {
                    reply(stun::Message::new(kind | stun::ERROR_CLASS, id).attr(stun::ATTR_ERROR_CODE, [0, 0, 4, 37]));
                }
                stun::REFRESH_REQUEST => {
                    counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    reply(stun::Message::new(kind | 0x0100, id));
                }
                stun::ALLOCATE_REQUEST => {
                    next_port += 1;
                    let relayed: SocketAddr = format!("198.51.100.2:{next_port}").parse().unwrap();
                    let relay = net.socket(&relayed.to_string(), None);
                    let allowed = Arc::new(Mutex::new(HashSet::new()));
                    allocations.lock().unwrap().insert(client, (relay.clone(), allowed.clone()));
                    // Packets to the relayed address go to the client, from permitted IPs only.
                    let (weak, server) = (weak.clone(), sock.addr);
                    tokio::spawn(async move {
                        loop {
                            let (from, payload) = relay.recv().await;
                            let Some(net) = weak.upgrade() else { return };
                            if allowed.lock().unwrap().contains(&from.ip()) {
                                let m = stun::Message::new(stun::DATA_INDICATION, [9; 12])
                                    .peer(stun::ATTR_XOR_PEER_ADDRESS, from)
                                    .attr(stun::ATTR_DATA, &payload);
                                net.send(server, client, &m.encode());
                            }
                        }
                    });
                    reply(
                        stun::Message::new(stun::ALLOCATE_SUCCESS, id)
                            .peer(stun::ATTR_XOR_RELAYED_ADDRESS, relayed)
                            .attr(stun::ATTR_LIFETIME, 600u32.to_be_bytes()),
                    );
                }
                stun::CREATE_PERMISSION_REQUEST => {
                    if let Some((_, allowed)) = allocations.lock().unwrap().get(&client) {
                        for p in peer_addresses(&data) {
                            allowed.lock().unwrap().insert(p.ip());
                        }
                    }
                    reply(stun::Message::new(kind | 0x0100, id));
                }
                _ => reply(stun::Message::new(kind | 0x0100, id)),
            }
        }
    });
    refreshes
}

/// All XOR-PEER-ADDRESS attributes of a message (a permission request may carry several).
fn peer_addresses(packet: &[u8]) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    let mut at = 20;
    while at + 4 <= packet.len() {
        let kind = u16::from_be_bytes([packet[at], packet[at + 1]]);
        let len = usize::from(u16::from_be_bytes([packet[at + 2], packet[at + 3]]));
        if kind == stun::ATTR_XOR_PEER_ADDRESS {
            // Rebuild a one-attribute message to decode it with the library's XOR rules.
            let mut one = packet[..20].to_vec();
            one[2..4].copy_from_slice(&((4 + len.next_multiple_of(4)) as u16).to_be_bytes());
            one.extend_from_slice(&packet[at..at + 4 + len.next_multiple_of(4)]);
            out.extend(stun::xor_address_attr(&one, stun::ATTR_XOR_PEER_ADDRESS));
        }
        at += 4 + len.next_multiple_of(4);
    }
    out
}

fn core(name: &str) -> Arc<Core> {
    Core::ephemeral(name, TrustPolicy::AcceptAllInsecure).unwrap()
}

fn sink() -> (EventSink, mpsc::UnboundedReceiver<Event>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Arc::new(move |e| {
            let _ = tx.send(e);
        }),
        rx,
    )
}

async fn next_matching(rx: &mut mpsc::UnboundedReceiver<Event>, mut pred: impl FnMut(&Event) -> bool) -> Option<Event> {
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(e) = rx.recv().await {
            if pred(&e) {
                return Some(e);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

fn display() -> DisplayInfo {
    DisplayInfo {
        width_px: 1280,
        height_px: 720,
        width_mm: 0,
        height_mm: 0,
        refresh_hz: 60,
        scale_percent: 100,
        refresh_rates: vec![],
    }
}

/// Which relay the host uses, when it uses one.
#[derive(Clone, Copy, PartialEq)]
enum Relay {
    /// No relay: direct or nothing.
    None,
    /// One the user set up (no limits).
    Own,
    /// Ours, from the rendezvous server (limited to about 6 Mbit/s).
    Ours,
    /// The user's, with the wrong password.
    WrongLogin,
}

struct Outcome {
    host: Option<Event>,
    viewer: Option<Event>,
    frames: usize,
    /// The bitrate scale the host's encoder was told to start at (100 if never told).
    host_scale: u32,
    /// Allocation refreshes the relay granted during the session.
    refreshes: usize,
}

/// Runs what the rendezvous server arranges: each side learns its public address from STUN,
/// they swap candidates, the host punches, the viewer connects (to the host's relay address, if
/// `relay` is set: what happens after a direct attempt fails). Returns the host's and the
/// viewer's events once the session has started or the viewer gave up.
async fn connect_across(host_nat: Nat, viewer_nat: Nat, relay: Relay) -> Outcome {
    // Refresh the relay several times within the session, as hours of real use would.
    set_relay_refresh(Duration::from_millis(150));
    let net = Arc::new(Net::default());
    stun_server(&net);
    let refreshes = turn_server(&net);
    let home = net.router(host_nat, "203.0.113.10");
    let away = net.router(viewer_nat, "203.0.113.20");
    let stun_addr: SocketAddr = STUN.parse().unwrap();

    let (hev, mut hrx) = sink();
    let host_sock = net.socket("10.0.0.2:47800", Some(home));
    let cfg = HostConfig {
        port: 0,
        stream: StreamConfig { codec: Codec::H264, width: 1280, height: 720, fps: 60 },
        extend: false,
        udp: Some(host_sock),
    };
    let host = start_host(core("Host"), cfg, hev).await.unwrap();
    let host_candidates = host.internet_candidates(stun_addr).await.unwrap();
    assert_eq!(host_candidates[0].ip().to_string(), "203.0.113.10", "STUN reports the router's public address");

    let viewer_sock = InternetSocket::with_socket(net.socket("192.168.1.5:50000", Some(away))).unwrap();
    let viewer_candidates = viewer_sock.candidates(stun_addr).await.unwrap();
    host.punch(viewer_candidates.clone());
    let mut address = host_candidates.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
    if relay != Relay::None {
        let login = TurnServer {
            addr: TURN.parse().unwrap(),
            username: TURN_USER.into(),
            password: if relay == Relay::WrongLogin { format!("{}x", turn_pass()) } else { turn_pass().into() },
        };
        let ips = viewer_candidates.iter().map(|a| a.ip()).collect();
        match host.relay(login, ips, relay == Relay::Ours).await {
            Ok(relayed) => {
                assert_eq!(relayed.ip().to_string(), "198.51.100.2", "an address on the relay");
                viewer_sock.through_relay(&[relayed]);
                address = relayed.to_string();
            }
            Err(e) => {
                host.stop().await;
                return Outcome {
                    host: None,
                    viewer: Some(Event::Error { message: e.to_string() }),
                    frames: 0,
                    host_scale: 100,
                    refreshes: 0,
                };
            }
        }
    }

    let frames = Arc::new(Mutex::new(0usize));
    let counted = frames.clone();
    let (vev, mut vrx) = sink();
    let viewer = connect_viewer(
        core("Viewer"),
        ViewerConfig {
            address,
            display: display(),
            codecs: vec![Codec::H264],
            expected_fingerprint: None,
            mode: Mode::Mirror,
            input: false,
            quality: Quality::Best,
            latency_csv: None,
            internet: Some(viewer_sock),
            join: None,
        },
        vev,
        Arc::new(move |_| *counted.lock().unwrap() += 1),
    );
    let viewer_event =
        next_matching(&mut vrx, |e| matches!(e, Event::SessionStarted { .. } | Event::SessionEnded { .. })).await;
    let mut host_scale = 100;
    let host_event = match viewer_event {
        Some(Event::SessionStarted { .. }) => {
            let started = next_matching(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
            // About a second, so the relay is refreshed (with new nonces) while frames flow.
            for i in 0..20 {
                host.submit_frame(&[7u8; 4000], clock::now_us(), i == 0);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            // The encoder's starting rate is set just after the session starts.
            while let Ok(e) = hrx.try_recv() {
                if let Event::BitrateHint { scale_percent, .. } = e {
                    host_scale = scale_percent;
                    break;
                }
            }
            started
        }
        _ => None,
    };
    let delivered = *frames.lock().unwrap();
    viewer.disconnect().await;
    host.stop().await;
    let refreshes = refreshes.load(std::sync::atomic::Ordering::Relaxed);
    Outcome { host: host_event, viewer: viewer_event, frames: delivered, host_scale, refreshes }
}

fn started(e: &Option<Event>) -> (LinkKind, TransportKind) {
    match e {
        Some(Event::SessionStarted { link, transport, .. }) => (*link, *transport),
        other => panic!("expected a session, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_home_routers_connect_directly() {
    let o = connect_across(Nat::PortRestricted, Nat::PortRestricted, Relay::None).await;
    assert_eq!(started(&o.viewer), (LinkKind::Internet, TransportKind::Quic));
    assert_eq!(started(&o.host).0, LinkKind::Internet, "the host knows the session came over the internet");
    assert!(o.frames >= 15, "only {} of 20 frames arrived", o.frames);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_symmetric_router_needs_the_relay() {
    let o = connect_across(Nat::Symmetric, Nat::PortRestricted, Relay::None).await;
    assert!(matches!(o.viewer, Some(Event::SessionEnded { .. })), "expected no direct connection, got {:?}", o.viewer);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_relay_connects_two_symmetric_routers() {
    let o = connect_across(Nat::Symmetric, Nat::Symmetric, Relay::Own).await;
    assert_eq!(started(&o.viewer), (LinkKind::Relayed, TransportKind::Quic));
    assert_eq!(started(&o.host).0, LinkKind::Relayed);
    assert!(o.frames >= 15, "only {} of 20 frames arrived", o.frames);
    assert!(o.host_scale >= 100, "the user's own relay has no bitrate limit ({} %)", o.host_scale);
    assert!(o.refreshes >= 3, "the allocation was refreshed through nonce changes ({} refreshes)", o.refreshes);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_viewer_shares_the_hosts_relay_address() {
    let net = Arc::new(Net::default());
    stun_server(&net);
    turn_server(&net);
    let home = net.router(Nat::Symmetric, "203.0.113.10");
    let cfg = HostConfig {
        port: 0,
        stream: StreamConfig { codec: Codec::H264, width: 1280, height: 720, fps: 60 },
        extend: false,
        udp: Some(net.socket("10.0.0.2:47800", Some(home))),
    };
    let (hev, _hrx) = sink();
    let host = start_host(core("Host"), cfg, hev).await.unwrap();
    let login = || TurnServer { addr: TURN.parse().unwrap(), username: TURN_USER.into(), password: turn_pass().into() };
    let first = host.relay(login(), vec!["203.0.113.20".parse().unwrap()], false).await.unwrap();
    // A relay refuses a second allocation from the same socket (437); this one reuses the first,
    // letting the new viewer in (with a nonce that has changed meanwhile).
    let second = host.relay(login(), vec!["203.0.113.30".parse().unwrap()], false).await.unwrap();
    assert_eq!(first, second);
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn our_relay_keeps_sessions_to_fast_quality() {
    let o = connect_across(Nat::Symmetric, Nat::PortRestricted, Relay::Ours).await;
    assert_eq!(started(&o.host).0, LinkKind::Relayed);
    assert_eq!(o.host_scale, 55, "held to Fast (6 Mbit/s is more than 720p needs)");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_relay_login_is_refused() {
    let o = connect_across(Nat::Symmetric, Nat::Symmetric, Relay::WrongLogin).await;
    let Some(Event::Error { message }) = &o.viewer else { panic!("got {:?}", o.viewer) };
    assert!(message.contains("refused the login"), "{message}");
}
