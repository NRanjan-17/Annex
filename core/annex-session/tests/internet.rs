//! Internet connections through a simulated internet: each computer sits behind its own home
//! router (NAT), and a STUN server tells each one its public address. Nothing touches the real
//! network, so the routers behave exactly as configured, every run.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use annex_session::*;
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use tokio::sync::mpsc;

const STUN: &str = "198.51.100.1:3478";

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

async fn next_matching(rx: &mut mpsc::UnboundedReceiver<Event>, pred: impl Fn(&Event) -> bool) -> Option<Event> {
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

/// Runs what the rendezvous server will arrange: each side learns its public address from STUN,
/// they swap candidates, the host punches, the viewer connects. Returns the host's and the
/// viewer's events once the session has started or the viewer gave up.
async fn connect_across(host_nat: Nat, viewer_nat: Nat) -> (Option<Event>, Option<Event>, usize) {
    let net = Arc::new(Net::default());
    stun_server(&net);
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
    host.punch(viewer_candidates);

    let frames = Arc::new(Mutex::new(0usize));
    let counted = frames.clone();
    let (vev, mut vrx) = sink();
    let viewer = connect_viewer(
        core("Viewer"),
        ViewerConfig {
            address: host_candidates.iter().map(ToString::to_string).collect::<Vec<_>>().join(","),
            display: display(),
            codecs: vec![Codec::H264],
            expected_fingerprint: None,
            mode: Mode::Mirror,
            input: false,
            quality: Quality::Best,
            latency_csv: None,
            internet: Some(viewer_sock),
        },
        vev,
        Arc::new(move |_| *counted.lock().unwrap() += 1),
    );
    let viewer_event =
        next_matching(&mut vrx, |e| matches!(e, Event::SessionStarted { .. } | Event::SessionEnded { .. })).await;
    let host_event = match viewer_event {
        Some(Event::SessionStarted { .. }) => {
            let started = next_matching(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
            for i in 0..20 {
                host.submit_frame(&[7u8; 4000], clock::now_us(), i == 0);
                tokio::time::sleep(Duration::from_millis(16)).await;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            started
        }
        _ => None,
    };
    let delivered = *frames.lock().unwrap();
    viewer.disconnect().await;
    host.stop().await;
    (host_event, viewer_event, delivered)
}

#[tokio::test(flavor = "multi_thread")]
async fn two_home_routers_connect_directly() {
    let (host, viewer, frames) = connect_across(Nat::PortRestricted, Nat::PortRestricted).await;
    let Some(Event::SessionStarted { link, transport, .. }) = viewer else { panic!("viewer: {viewer:?}") };
    assert_eq!((link, transport), (LinkKind::Internet, TransportKind::Quic));
    let Some(Event::SessionStarted { link, .. }) = host else { panic!("host: {host:?}") };
    assert_eq!(link, LinkKind::Internet, "the host knows the session came over the internet");
    assert!(frames >= 15, "only {frames} of 20 frames arrived");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_symmetric_router_needs_the_relay() {
    let (_, viewer, _) = connect_across(Nat::Symmetric, Nat::PortRestricted).await;
    assert!(matches!(viewer, Some(Event::SessionEnded { .. })), "expected no direct connection, got {viewer:?}");
}
