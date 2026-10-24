//! The UDP socket under QUIC, with what internet connections need on top: it asks a STUN server
//! which public address the router gave it, sends punch packets that open the router towards a
//! peer, and keeps STUN traffic away from QUIC. Everything QUIC sends and receives passes through
//! unchanged, so a LAN session behaves exactly as with a plain socket.

use std::collections::{HashMap, HashSet};
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

use crate::stun;

/// How long to wait for a STUN server's answer before trying once more.
const STUN_TRY: Duration = Duration::from_millis(700);
/// How long punch packets keep going: enough for the viewer's QUIC handshake to retry through.
pub const PUNCH_FOR: Duration = Duration::from_secs(6);
const PUNCH_EVERY: Duration = Duration::from_millis(200);

pub struct PunchSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    /// STUN answers awaited, by transaction id.
    waiting: Mutex<HashMap<stun::TransactionId, oneshot::Sender<SocketAddr>>>,
    /// Addresses this socket punched towards: a session from one of them came over the internet.
    punched: Mutex<HashSet<SocketAddr>>,
}

impl fmt::Debug for PunchSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PunchSocket").field("inner", &self.inner).finish_non_exhaustive()
    }
}

impl PunchSocket {
    pub fn new(inner: Arc<dyn AsyncUdpSocket>) -> Arc<Self> {
        Arc::new(Self { inner, waiting: Mutex::default(), punched: Mutex::default() })
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

    /// The public address and port the router gave this socket, as a STUN server sees it.
    pub async fn public_address(self: &Arc<Self>, stun_server: SocketAddr) -> io::Result<SocketAddr> {
        for _ in 0..3 {
            let id: stun::TransactionId = rand_id();
            let (tx, rx) = oneshot::channel();
            self.waiting.lock().unwrap().insert(id, tx);
            let sent = self.send_raw(stun_server, &stun::binding_request(&id)).await;
            let answer = match sent {
                Ok(()) => tokio::time::timeout(STUN_TRY, rx).await.ok().and_then(Result::ok),
                Err(e) => {
                    self.waiting.lock().unwrap().remove(&id);
                    return Err(e);
                }
            };
            self.waiting.lock().unwrap().remove(&id);
            if let Some(a) = answer {
                return Ok(SocketAddr::new(a.ip().to_canonical(), a.port()));
            }
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, "the STUN server didn't answer"))
    }

    /// Opens this computer's router towards `peers` for [`PUNCH_FOR`]: a small STUN indication to
    /// each, several times a second. The peers' own packets can then get in.
    pub fn punch(self: &Arc<Self>, peers: Vec<SocketAddr>) {
        let peers: Vec<SocketAddr> =
            peers.into_iter().map(|a| SocketAddr::new(a.ip().to_canonical(), a.port())).collect();
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
        self.punched.lock().unwrap().contains(&SocketAddr::new(peer.ip().to_canonical(), peer.port()))
    }

    /// Takes a received STUN message out of QUIC's way: an awaited answer is handed over; punches
    /// and anything else are dropped.
    fn take_stun(&self, data: &[u8]) {
        if let (Some(id), Some(addr)) = (stun::transaction_id(data), stun::mapped_address(data))
            && let Some(tx) = self.waiting.lock().unwrap().remove(&id)
        {
            let _ = tx.send(addr);
        }
    }
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
        self.inner.try_send(transmit)
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            let n = match self.inner.poll_recv(cx, bufs, meta) {
                Poll::Ready(Ok(n)) => n,
                other => return other,
            };
            // Keep QUIC's datagrams, moving them down over any STUN ones taken out. A batch of
            // several segments (GRO) is one flow of equal-sized packets, never STUN.
            let mut kept = 0;
            for i in 0..n {
                let m = meta[i];
                if m.len == m.stride && stun::is_stun(&bufs[i][..m.len]) {
                    self.take_stun(&bufs[i][..m.len]);
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
            // Only STUN arrived: look again (the inner socket registers the waker if nothing's left).
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

/// This computer's own public address with `port`, if it has one: its main IPv6 address when
/// that's global (two IPv6 homes then need no punching at all). Private, link-local and LAN
/// addresses are left out: computers on the same network find each other through Nearby, and
/// they'd tell the other side about this home network for nothing. Found by asking the system
/// which address it would use towards the internet; nothing is sent.
pub fn own_addresses(port: u16) -> Vec<SocketAddr> {
    let s = UdpSocket::bind("[::]:0").ok();
    let ip = s.and_then(|s| s.connect("[2001:db8::1]:9").ok().and(s.local_addr().ok())).map(|a| a.ip());
    match ip {
        Some(IpAddr::V6(v6)) if is_global_v6(&v6) => vec![SocketAddr::new(IpAddr::V6(v6), port)],
        _ => Vec::new(),
    }
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
