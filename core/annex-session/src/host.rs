//! Host side: listens for viewers, authorizes them and streams encoded frames.
//!
//! Up to [`MAX_SESSIONS`] viewers at once. **Mirror** sessions all receive the same frames
//! ([`HostHandle::submit_frame`]); each **Extend** session gets its own virtual display and
//! frames ([`HostHandle::submit_session_frame`]). Extra viewers are rejected with `busy`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::Instrument as _;

use annex_proto::{Codec, Control, DisplayInfo, Mode, Role, StreamConfig, VideoFlags, VideoHeader};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::clock::now_us;
use crate::conn::{Incoming, Reader, Writer, check_hello, hello};
use crate::events::{Event, EventSink, LinkKind};
use crate::pairing::{PairContext, pair_as_host};
use crate::punch::{PunchSocket, TurnServer};
use crate::transfer::no_transfers;
use crate::transport::{
    BoxRead, BoxWrite, Established, TransportKind, accept_quic, accept_tcp, is_link_local, quic_server_config,
};
use crate::{Core, SessionError};

/// FEC overhead for datagram video (percent of data shards). Adjusted by receiver reports.
const DEFAULT_FEC_PERCENT: u32 = 15;

/// Where a session's video goes.
enum VideoOut {
    /// Video records on the control stream (TCP).
    Stream,
    /// FEC-protected datagrams (QUIC). The FEC level follows receiver reports.
    Datagrams { conn: quinn::Connection, fec_percent: AtomicU32 },
}

impl VideoOut {
    async fn send(&self, writer: &mut Writer<BoxWrite>, header: VideoHeader, data: &[u8]) -> Result<(), SessionError> {
        match self {
            VideoOut::Stream => writer.send_video(header, data).await,
            VideoOut::Datagrams { conn, fec_percent } => {
                let max = conn.max_datagram_size().unwrap_or(1200).min(1400);
                let grams = annex_proto::fragment::fragment(
                    header.frame_id,
                    header.capture_ts_us,
                    header.flags,
                    data,
                    max,
                    fec_percent.load(Ordering::Relaxed),
                )?;
                for g in grams {
                    // A full datagram queue drops the oldest datagrams; FEC and keyframe requests recover.
                    if let Err(e) = conn.send_datagram(g.into()) {
                        return Err(SessionError::Io(std::io::Error::other(e)));
                    }
                }
                Ok(())
            }
        }
    }
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Long enough for the user on the viewer side to type the pairing PIN.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(130);
/// Frames queued between the encoder and the socket. Small on purpose: on a slow link we
/// would rather drop frames and resync with a keyframe than build up latency.
const FRAME_QUEUE: usize = 4;
/// Concurrent viewers per host.
pub const MAX_SESSIONS: usize = 4;

pub type SessionId = u64;

#[derive(Debug, Clone)]
pub struct HostConfig {
    /// TCP port to listen on (0 = pick a free port).
    pub port: u16,
    /// Stream description for mirror sessions; update with [`HostHandle::set_stream`].
    pub stream: StreamConfig,
    /// Whether this host can create virtual displays (accepts `extend` sessions).
    pub extend: bool,
    /// The UDP socket for QUIC instead of binding `port`. Tests use it for a simulated network.
    #[doc(hidden)]
    pub udp: Option<Arc<dyn quinn::AsyncUdpSocket>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitResult {
    Sent,
    /// No (matching) viewer connected; the frame was discarded.
    NoSession,
    /// The link is congested, or we are waiting for a keyframe after a drop.
    Dropped,
}

struct OutFrame {
    header: VideoHeader,
    data: Arc<Vec<u8>>,
}

struct SessionSlot {
    id: SessionId,
    mode: Mode,
    frames: mpsc::Sender<OutFrame>,
    stream: watch::Sender<StreamConfig>,
    need_keyframe: AtomicBool,
    next_frame_id: AtomicU32,
    /// Signalled by [`HostHandle::end_session`].
    ended: tokio::sync::Notify,
    /// Messages the app sends to this viewer (direct-link offers).
    outbox: mpsc::UnboundedSender<Control>,
    /// Clipboard and files, when both sides take them (QUIC sessions only).
    transfers: Option<Arc<crate::transfer::Transfers>>,
}

struct Shared {
    core: Arc<Core>,
    events: EventSink,
    extend: bool,
    mirror_stream: Mutex<StreamConfig>,
    sessions: Mutex<HashMap<SessionId, Arc<SessionSlot>>>,
    /// Connections that have claimed a slot (pairing or streaming).
    claimed: AtomicUsize,
    next_id: AtomicU64,
    /// QUIC's socket, which also reaches across the internet (None if QUIC couldn't start).
    udp: Option<Arc<PunchSocket>>,
}

impl Shared {
    fn request_keyframe(&self, slot: &SessionSlot) {
        // Only notify once per outstanding request.
        if !slot.need_keyframe.swap(true, Ordering::AcqRel) {
            (self.events)(Event::KeyframeRequested { session_id: slot.id });
        }
    }

    fn submit(&self, slot: &SessionSlot, data: &Arc<Vec<u8>>, capture_ts_us: i64, keyframe: bool) -> SubmitResult {
        if keyframe {
            slot.need_keyframe.store(false, Ordering::Release);
        } else if slot.need_keyframe.load(Ordering::Acquire) {
            // Deltas after a drop would decode as garbage; wait for the keyframe.
            return SubmitResult::Dropped;
        }
        let frame = OutFrame {
            header: VideoHeader {
                frame_id: slot.next_frame_id.fetch_add(1, Ordering::Relaxed),
                capture_ts_us,
                flags: VideoFlags(if keyframe { VideoFlags::KEYFRAME } else { 0 }),
            },
            data: data.clone(),
        };
        match slot.frames.try_send(frame) {
            Ok(()) => SubmitResult::Sent,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.request_keyframe(slot);
                SubmitResult::Dropped
            }
            Err(mpsc::error::TrySendError::Closed(_)) => SubmitResult::NoSession,
        }
    }
}

pub struct HostHandle {
    shared: Arc<Shared>,
    port: u16,
    shutdown: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl HostHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Changes the mirror stream (e.g. the captured display's resolution changed). Every mirror
    /// session is told with a `stream_config` message.
    pub fn set_stream(&self, stream: StreamConfig) {
        *self.shared.mirror_stream.lock().unwrap() = stream.clone();
        for s in self.shared.sessions.lock().unwrap().values().filter(|s| s.mode == Mode::Mirror) {
            s.stream.send_replace(stream.clone());
        }
    }

    /// Changes one session's stream (Extend: the user picked another resolution).
    pub fn set_session_stream(&self, session_id: SessionId, stream: StreamConfig) -> bool {
        match self.shared.sessions.lock().unwrap().get(&session_id) {
            Some(s) => {
                s.stream.send_replace(stream);
                true
            }
            None => false,
        }
    }

    /// Where a viewer on the internet can reach this host: the public address and port its router
    /// gave the session socket (asked of `stun_server`), plus its own LAN and IPv6 addresses.
    /// Hand them to the viewer through the rendezvous server.
    pub async fn internet_candidates(&self, stun_server: SocketAddr) -> Result<Vec<SocketAddr>, SessionError> {
        let udp = self.shared.udp.as_ref().ok_or(SessionError::Violation("QUIC isn't available".into()))?;
        let mut out = vec![udp.public_address(stun_server).await?];
        out.extend(crate::punch::own_addresses(udp.local_addr()?.port()));
        out.dedup();
        Ok(out)
    }

    /// A viewer on the internet is about to connect from `viewer`: open this computer's router
    /// towards those addresses for a few seconds.
    pub fn punch(&self, viewer: Vec<SocketAddr>) {
        if let Some(udp) = &self.shared.udp {
            udp.punch(viewer);
        }
    }

    /// The viewer couldn't connect directly: gets an address on `relay` that forwards to this
    /// host, open to the viewer's IP addresses. Hand it to the viewer through the rendezvous
    /// server. `limited`: it's our relay (sessions through it are held to about 6 Mbit/s, and
    /// end when its login expires), not one the user set up.
    pub async fn relay(
        &self,
        relay: TurnServer,
        viewer: Vec<std::net::IpAddr>,
        limited: bool,
    ) -> Result<SocketAddr, SessionError> {
        let udp = self.shared.udp.as_ref().ok_or(SessionError::Violation("QUIC isn't available".into()))?;
        Ok(udp.allocate_relay(relay, viewer, limited).await?)
    }

    /// Whether this host accepts Extend sessions (it can create virtual displays).
    pub fn extend_supported(&self) -> bool {
        self.shared.extend
    }

    pub fn has_session(&self) -> bool {
        !self.shared.sessions.lock().unwrap().is_empty()
    }

    /// Active sessions and their modes.
    pub fn sessions(&self) -> Vec<(SessionId, Mode)> {
        self.shared.sessions.lock().unwrap().values().map(|s| (s.id, s.mode)).collect()
    }

    /// Queues one encoded access unit for every **mirror** session. Never blocks.
    pub fn submit_frame(&self, data: &[u8], capture_ts_us: i64, keyframe: bool) -> SubmitResult {
        let targets: Vec<Arc<SessionSlot>> =
            self.shared.sessions.lock().unwrap().values().filter(|s| s.mode == Mode::Mirror).cloned().collect();
        if targets.is_empty() {
            return SubmitResult::NoSession;
        }
        let data = Arc::new(data.to_vec());
        let results: Vec<SubmitResult> =
            targets.iter().map(|s| self.shared.submit(s, &data, capture_ts_us, keyframe)).collect();
        if results.contains(&SubmitResult::Sent) {
            SubmitResult::Sent
        } else if results.contains(&SubmitResult::Dropped) {
            SubmitResult::Dropped
        } else {
            SubmitResult::NoSession
        }
    }

    /// Queues one encoded access unit for a single session (Extend). Never blocks.
    pub fn submit_session_frame(
        &self,
        session_id: SessionId,
        data: &[u8],
        capture_ts_us: i64,
        keyframe: bool,
    ) -> SubmitResult {
        let slot = self.shared.sessions.lock().unwrap().get(&session_id).cloned();
        match slot {
            Some(s) => self.shared.submit(&s, &Arc::new(data.to_vec()), capture_ts_us, keyframe),
            None => SubmitResult::NoSession,
        }
    }

    /// Tells one viewer to join this hotspot (this computer runs it) for a direct link.
    pub fn offer_direct_link(&self, session_id: SessionId, ssid: &str, passphrase: &str) -> bool {
        self.send_to(session_id, Control::DirectLinkOffer { ssid: ssid.into(), passphrase: passphrase.into() })
    }

    /// Answers a direct-link request that can't be met here.
    /// Asks the viewer to set up a direct Wi-Fi link (a Windows viewer starts its hotspot and
    /// answers with an offer; others answer "unavailable").
    pub fn request_direct_link(&self, session_id: SessionId) -> bool {
        self.send_to(session_id, Control::DirectLinkRequest)
    }

    pub fn refuse_direct_link(&self, session_id: SessionId, reason: &str) -> bool {
        self.send_to(session_id, Control::DirectLinkUnavailable { reason: reason.into() })
    }

    fn transfers(&self, session_id: SessionId) -> Option<Arc<crate::transfer::Transfers>> {
        self.shared.sessions.lock().unwrap().get(&session_id).and_then(|s| s.transfers.clone())
    }

    /// Puts a clipboard item on the viewer's clipboard. False: no such session, the viewer can't
    /// take it (older version, TCP), or it's too big for the link.
    pub fn send_clipboard(&self, session_id: SessionId, mime: &str, data: Vec<u8>) -> bool {
        self.transfers(session_id).is_some_and(|t| t.send_clipboard(mime, data))
    }

    /// Sends a file to the viewer; the returned id is in its transfer events.
    pub fn send_file(&self, session_id: SessionId, path: &std::path::Path) -> Result<u64, String> {
        self.transfers(session_id).ok_or_else(no_transfers)?.send_file(path)
    }

    pub fn cancel_transfer(&self, session_id: SessionId, incoming: bool, id: u64) {
        if let Some(t) = self.transfers(session_id) {
            t.cancel(incoming, id);
        }
    }

    fn send_to(&self, session_id: SessionId, c: Control) -> bool {
        self.shared.sessions.lock().unwrap().get(&session_id).is_some_and(|s| s.outbox.send(c).is_ok())
    }

    /// Ends one session (e.g. its virtual display could not be created or was removed).
    pub fn end_session(&self, session_id: SessionId) {
        if let Some(s) = self.shared.sessions.lock().unwrap().remove(&session_id) {
            // notify_one stores a permit, so this works even if the task isn't waiting yet.
            s.ended.notify_one();
        }
    }

    /// Stops listening, ends all sessions and waits for tasks to finish.
    pub async fn stop(&self) {
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(t) = task {
            let _ = t.await;
        }
    }
}

/// The largest picture H.264 can carry (level 5.1/5.2). Encoders reject bigger frames.
pub const MAX_H264: (u32, u32) = (4096, 2304);

/// Initial stream for an Extend session: the viewer's native resolution (even, scaled down to fit
/// [`MAX_H264`]) and refresh rate (≤ 240 Hz), like a monitor plugged into the host.
pub fn extend_stream_for(display: &DisplayInfo) -> StreamConfig {
    let (w, h) = (display.width_px.max(2), display.height_px.max(2));
    let k = (MAX_H264.0 as f64 / w as f64).min(MAX_H264.1 as f64 / h as f64).min(1.0);
    StreamConfig {
        codec: Codec::H264,
        width: ((w as f64 * k).round() as u32).max(2) & !1,
        height: ((h as f64 * k).round() as u32).max(2) & !1,
        fps: if display.refresh_hz == 0 { 60 } else { display.refresh_hz.clamp(24, 240) },
    }
}

/// Binds the listener and starts accepting viewers. Must be called inside a tokio runtime.
pub async fn start_host(core: Arc<Core>, cfg: HostConfig, events: EventSink) -> Result<HostHandle, SessionError> {
    let listener = bind_tcp(cfg.port).await?;
    let port = listener.local_addr()?.port();
    let tls = tokio_rustls::TlsAcceptor::from(Arc::new(annex_crypto::server_config(core.identity())?));
    // QUIC on the same port number (UDP). If UDP can't be bound, viewers fall back to TCP.
    let server = quic_server_config(&core)?;
    let udp = match cfg.udp {
        Some(s) => Ok(PunchSocket::new(s)),
        None => bind_udp(port, listener.local_addr()?.is_ipv6()).map_err(SessionError::from),
    };
    let quic = match udp.and_then(|u| quic_endpoint(server, u.clone()).map(|e| (e, u))) {
        Ok(q) => Some(q),
        Err(e) => {
            tracing::warn!("QUIC unavailable, TCP only: {e}");
            None
        }
    };
    let (quic, udp) = quic.unzip();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let shared = Arc::new(Shared {
        core,
        events: events.clone(),
        extend: cfg.extend,
        mirror_stream: Mutex::new(cfg.stream),
        sessions: Mutex::new(HashMap::new()),
        claimed: AtomicUsize::new(0),
        next_id: AtomicU64::new(1),
        udp,
    });
    events(Event::Listening { port });
    let task = tokio::spawn(accept_loop(listener, tls, quic, shared.clone(), shutdown_rx));
    Ok(HostHandle { shared, port, shutdown, task: Mutex::new(Some(task)) })
}

/// IPv6 and IPv4 on one socket where the system allows it: discovery advertises IPv6 addresses
/// (`fe80::…` on a cable), and viewers try those first. Falls back to IPv4 only.
async fn bind_tcp(port: u16) -> std::io::Result<TcpListener> {
    let dual = || -> std::io::Result<std::net::TcpListener> {
        let s = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;
        // Set both ways explicitly: Windows defaults to IPv6 only, macOS and Linux don't.
        s.set_only_v6(false)?;
        #[cfg(not(windows))]
        s.set_reuse_address(true)?;
        s.bind(&std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)).into())?;
        s.listen(128)?;
        s.set_nonblocking(true)?;
        Ok(s.into())
    };
    match dual() {
        Ok(l) => TcpListener::from_std(l),
        Err(e) => {
            tracing::info!("IPv6 listener unavailable, IPv4 only: {e}");
            TcpListener::bind(("0.0.0.0", port)).await
        }
    }
}

/// The UDP socket for QUIC, on the same addresses as the TCP listener.
fn bind_udp(port: u16, ipv6: bool) -> std::io::Result<Arc<PunchSocket>> {
    let s = if ipv6 {
        let s = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
        s.set_only_v6(false)?;
        s.bind(&SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)).into())?;
        s
    } else {
        let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
        s.bind(&SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port)).into())?;
        s
    };
    s.set_nonblocking(true)?;
    PunchSocket::from_std(s.into())
}

fn quic_endpoint(config: quinn::ServerConfig, udp: Arc<PunchSocket>) -> Result<quinn::Endpoint, SessionError> {
    let runtime = quinn::default_runtime().ok_or_else(|| std::io::Error::other("no async runtime for QUIC"))?;
    Ok(quinn::Endpoint::new_with_abstract_socket(quinn::EndpointConfig::default(), Some(config), udp, runtime)?)
}

async fn accept_loop(
    listener: TcpListener,
    tls: tokio_rustls::TlsAcceptor,
    quic: Option<quinn::Endpoint>,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut sessions = tokio::task::JoinSet::new();
    let quic_accept = async |q: &Option<quinn::Endpoint>| match q {
        Some(e) => e.accept().await,
        None => std::future::pending().await,
    };
    loop {
        tokio::select! {
            r = listener.accept() => match r {
                Ok((sock, addr)) => {
                    tracing::info!(addr = %crate::transport::canonical(addr), "incoming TCP connection");
                    let (tls, shared, shutdown) = (tls.clone(), shared.clone(), shutdown.clone());
                    sessions.spawn(async move {
                        match accept_tcp(sock, tls).await {
                            Ok(est) => handle_conn(est, shared, shutdown).await,
                            Err(e) => tracing::info!("TCP handshake failed: {e}"),
                        }
                    }.instrument(conn_span(addr)));
                }
                Err(e) => tracing::warn!("accept failed: {e}"),
            },
            Some(incoming) = quic_accept(&quic) => {
                tracing::info!(addr = %crate::transport::canonical(incoming.remote_address()), "incoming QUIC connection");
                let (shared, shutdown) = (shared.clone(), shutdown.clone());
                let span = conn_span(incoming.remote_address());
                sessions.spawn(async move {
                    match accept_quic(incoming).await {
                        Ok(est) => handle_conn(est, shared, shutdown).await,
                        Err(e) => tracing::info!("QUIC handshake failed: {e}"),
                    }
                }.instrument(span));
            }
            _ = shutdown.changed() => break,
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
        }
    }
    while sessions.join_next().await.is_some() {}
    if let Some(q) = quic {
        q.close(0u32.into(), b"host stopped");
        let _ = timeout(Duration::from_millis(500), q.wait_idle()).await;
    }
}

/// Every line about one viewer's connection carries its address, and its session once it has one.
fn conn_span(addr: std::net::SocketAddr) -> tracing::Span {
    tracing::info_span!("viewer", addr = %crate::transport::canonical(addr), session = tracing::field::Empty)
}

async fn handle_conn(est: Established, shared: Arc<Shared>, shutdown: watch::Receiver<bool>) {
    let Established { kind, reader, writer, fingerprint: fp, exporter, remote, quic } = est;
    let mut reader = Reader::new(reader);
    let mut writer = Writer::new(writer);
    let video = match (&quic, kind) {
        (Some(conn), TransportKind::Quic) if conn.max_datagram_size().is_some() => {
            VideoOut::Datagrams { conn: conn.clone(), fec_percent: AtomicU32::new(DEFAULT_FEC_PERCENT) }
        }
        _ => VideoOut::Stream,
    };

    // Claim a slot before asking anyone for a PIN.
    if shared.claimed.fetch_add(1, Ordering::AcqRel) >= MAX_SESSIONS {
        shared.claimed.fetch_sub(1, Ordering::AcqRel);
        let _ = writer.send_control(&Control::SessionReject { reason: "busy".into() }).await;
        writer.shutdown().await;
        if let Some(conn) = quic {
            // Dropping the connection would discard the unsent reject; wait for the viewer to hang up.
            let _ = timeout(Duration::from_millis(500), conn.closed()).await;
        }
        return;
    }
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let mut started = false;
    let link = Link {
        transport: kind,
        limited: shared.udp.as_ref().and_then(|u| u.relayed(remote)).unwrap_or(false),
        kind: if shared.udp.as_ref().is_some_and(|u| u.relayed(remote).is_some()) {
            LinkKind::Relayed
        } else if shared.udp.as_ref().is_some_and(|u| u.was_punched(remote)) {
            LinkKind::Internet
        } else if is_link_local(&remote) {
            LinkKind::Direct
        } else {
            LinkKind::Network
        },
    };
    let result = run_session(
        &mut reader,
        &mut writer,
        &fp,
        &exporter,
        &shared,
        shutdown,
        id,
        link,
        &video,
        quic.as_ref(),
        &mut started,
    )
    .await;
    if let Some(t) = shared.sessions.lock().unwrap().remove(&id).and_then(|s| s.transfers.clone()) {
        t.close();
    }
    shared.claimed.fetch_sub(1, Ordering::AcqRel);
    let reason = match result {
        Ok(reason) => reason,
        Err(SessionError::Rejected(reason)) => reason,
        Err(e) => e.to_string(),
    };
    writer.shutdown().await;
    if let Some(conn) = quic {
        // Let the bye reach the viewer before tearing the connection down.
        let _ = timeout(Duration::from_millis(500), conn.closed()).await;
        conn.close(0u32.into(), b"bye");
    }
    if started {
        (shared.events)(Event::SessionEnded { session_id: id, reason });
    } else {
        tracing::info!("connection closed before session start: {reason}");
    }
}

#[derive(Clone, Copy)]
struct Link {
    transport: TransportKind,
    kind: LinkKind,
    /// Through our relay, which has usage limits: the session is held to about 6 Mbit/s.
    limited: bool,
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    reader: &mut Reader<BoxRead>,
    writer: &mut Writer<BoxWrite>,
    fp: &str,
    exporter: &[u8],
    shared: &Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
    id: SessionId,
    link: Link,
    video: &VideoOut,
    quic: Option<&quinn::Connection>,
    started: &mut bool,
) -> Result<String, SessionError> {
    if fp == shared.core.fingerprint() {
        return Err(SessionError::Violation("peer uses our own identity".into()));
    }
    let knows = shared.core.knows(fp);
    let mut caps = vec!["h264"];
    if shared.extend {
        caps.push("extend");
    }
    if quic.is_some() {
        caps.push(crate::transfer::CAP);
    }
    writer.send_control(&hello(&shared.core, vec![Role::Host], knows, &caps)).await?;
    let peer = check_hello(timeout(HANDSHAKE_TIMEOUT, reader.next_control()).await??, fp)?;
    if knows && peer.knows_peer {
        shared.core.touch(fp);
    } else {
        let cx = PairContext {
            core: &shared.core,
            events: &shared.events,
            peer_fp: fp,
            peer_name: &peer.device_name,
            exporter,
        };
        pair_as_host(cx, reader, writer).await?;
    }

    let req = match timeout(REQUEST_TIMEOUT, reader.next_control()).await?? {
        Control::SessionRequest(r) => r,
        Control::Bye { reason } => return Err(SessionError::Rejected(reason)),
        _ => return Err(SessionError::Violation("expected session_request".into())),
    };
    let stream = match req.mode {
        Mode::Mirror => shared.mirror_stream.lock().unwrap().clone(),
        Mode::Extend => extend_stream_for(&req.display),
    };
    let reject = if req.mode == Mode::Extend && !shared.extend {
        Some("extend_not_supported")
    } else if !req.codecs.contains(&stream.codec) {
        Some("no_common_codec")
    } else {
        None
    };
    tracing::Span::current().record("session", id);
    tracing::info!(id, peer = %peer.device_name, mode = ?req.mode, input = req.input, quality = ?req.quality, "session request");
    if let Some(reason) = reject {
        tracing::warn!(id, reason, "session rejected");
        writer.send_control(&Control::SessionReject { reason: reason.into() }).await?;
        return Ok(reason.into());
    }
    // Input needs the viewer to ask and the user to have allowed this device on the host.
    let mut input = req.input && shared.core.allows_input(fp);
    writer.send_control(&Control::SessionAccept { stream: stream.clone(), input }).await?;

    let (frame_tx, mut frame_rx) = mpsc::channel::<OutFrame>(FRAME_QUEUE);
    let (outbox, mut outbox_rx) = mpsc::unbounded_channel::<Control>();
    let (stream_tx, mut stream_rx) = watch::channel(stream.clone());
    let slot = Arc::new(SessionSlot {
        id,
        mode: req.mode,
        frames: frame_tx,
        stream: stream_tx,
        need_keyframe: AtomicBool::new(false),
        next_frame_id: AtomicU32::new(0),
        ended: tokio::sync::Notify::new(),
        outbox,
        transfers: match quic {
            Some(conn) if peer.caps.iter().any(|c| c == crate::transfer::CAP) => {
                // Only from a viewer that asked for, and still has, control of this computer.
                let (core, fp, asked) = (shared.core.clone(), fp.to_string(), req.input);
                Some(crate::transfer::Transfers::start(
                    conn.clone(),
                    id,
                    shared.events.clone(),
                    shared.core.downloads(),
                    link.limited,
                    Box::new(move || asked && core.allows_input(&fp)),
                ))
            }
            _ => None,
        },
    });
    shared.sessions.lock().unwrap().insert(id, slot.clone());
    if req.mode == Mode::Mirror {
        // A `set_stream` between reading the mirror stream above and the insert didn't reach this
        // slot; one after the insert did, and is picked up by `stream_rx.changed()` below.
        let current = shared.mirror_stream.lock().unwrap().clone();
        if current != stream {
            slot.stream.send_replace(current);
        }
    }
    *started = true;
    let relay_ceiling = crate::rate::relay_ceiling(req.mode, &stream);
    (shared.events)(Event::SessionStarted {
        session_id: id,
        mode: req.mode,
        peer_name: peer.device_name.clone(),
        fingerprint: fp.to_string(),
        viewer_display: Some(req.display.clone()),
        stream,
        transport: link.transport,
        link: link.kind,
        input,
        pointer_deltas: true,
        peer_os: crate::conn::peer_os(&peer),
    });
    shared.request_keyframe(&slot);
    if req.input && !input {
        // Let the user allow it from here instead of finding the per-device switch.
        (shared.events)(Event::InputRequested {
            session_id: id,
            peer_name: peer.device_name.clone(),
            fingerprint: fp.to_string(),
        });
    }

    // The reader runs independently so control messages are handled while we write video.
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<Result<Control, SessionError>>(32);
    let reader_fut = async {
        loop {
            let item = match reader.next().await {
                Ok(Incoming::Control(c)) => Ok(c),
                Ok(Incoming::Video(_)) => Err(SessionError::Violation("viewer sent video".into())),
                Err(e) => Err(e),
            };
            let fatal = item.is_err();
            if ctl_tx.send(item).await.is_err() || fatal {
                break;
            }
        }
        // Stay pending: `main` drains the channel in order and sees the terminal error last,
        // so a `Bye` followed by EOF is reported as the `Bye` reason, not as "closed".
        std::future::pending::<()>().await
    };
    let mut rate = if link.limited {
        let ceiling = relay_ceiling;
        tracing::info!(id, ceiling, "through our relay: bitrate limited");
        crate::rate::RateController::with_ceiling(ceiling)
    } else {
        crate::rate::RateController::new(req.quality)
    };
    if rate.scale_percent() != 100 {
        // The viewer chose a lower quality: start the encoder there rather than at 100 %.
        (shared.events)(Event::BitrateHint { session_id: id, scale_percent: rate.scale_percent() });
    }
    let main = async {
        loop {
            tokio::select! {
                f = frame_rx.recv() => match f {
                    Some(f) => match video.send(writer, f.header, &f.data).await {
                        // Too big to send (more datagram shards than a frame may have): skip it and
                        // resync with a keyframe instead of ending the session.
                        Err(SessionError::Proto(annex_proto::ProtoError::RecordTooLarge(len))) => {
                            tracing::warn!(id, len, frame_id = f.header.frame_id, "frame too large to send; dropped");
                            shared.request_keyframe(&slot);
                        }
                        r => r?,
                    },
                    None => return Ok("stopped".to_string()),
                },
                Some(c) = outbox_rx.recv() => writer.send_control(&c).await?,
                _ = slot.ended.notified() => {
                    let _ = writer.send_control(&Control::Bye { reason: "host_ended_session".into() }).await;
                    return Ok("host_ended_session".to_string());
                },
                c = ctl_rx.recv() => match c {
                    Some(Ok(Control::KeyframeRequest)) => shared.request_keyframe(&slot),
                    Some(Ok(Control::DirectLinkRequest)) => {
                        (shared.events)(Event::DirectLinkRequested { session_id: id });
                    }
                    Some(Ok(Control::DirectLinkOffer { ssid, passphrase })) => {
                        (shared.events)(Event::DirectLinkOffer { session_id: id, ssid, passphrase });
                    }
                    Some(Ok(Control::DirectLinkUnavailable { reason })) => {
                        (shared.events)(Event::DirectLinkUnavailable { session_id: id, reason });
                    }
                    Some(Ok(Control::Input(event))) => {
                        // Re-checked per event so revoking takes effect immediately.
                        if input && shared.core.allows_input(fp) {
                            (shared.events)(Event::Input { session_id: id, event });
                        }
                    }
                    Some(Ok(Control::ReceiverReport(r))) => {
                        // Reports arrive about once a second: a good moment to pick up a changed
                        // input setting.
                        let allowed = req.input && shared.core.allows_input(fp);
                        if allowed != input {
                            input = allowed;
                            writer.send_control(&Control::InputAllowed { allowed }).await?;
                            (shared.events)(Event::InputAllowed { session_id: id, allowed });
                        }
                        let d = rate.on_report(&r);
                        if let VideoOut::Datagrams { fec_percent, .. } = video {
                            fec_percent.store(d.fec_percent, Ordering::Relaxed);
                        }
                        if d.scale_changed {
                            (shared.events)(Event::BitrateHint { session_id: id, scale_percent: d.scale_percent });
                        }
                    }
                    Some(Ok(Control::ClockPing { seq, t_viewer_us })) => {
                        writer.send_control(&Control::ClockPong { seq, t_viewer_us, t_host_us: now_us() }).await?;
                    }
                    Some(Ok(Control::Bye { reason })) => return Ok(reason),
                    Some(Ok(other)) => tracing::debug!(?other, "ignoring control message"),
                    Some(Err(e)) => return Err(e),
                    None => return Err(SessionError::Closed),
                },
                Ok(()) = stream_rx.changed() => {
                    // Mirror sessions resync with a keyframe after a resolution change too.
                    let s = stream_rx.borrow_and_update().clone();
                    writer.send_control(&Control::StreamConfig(s)).await?;
                }
                _ = shutdown.changed() => {
                    let _ = writer.send_control(&Control::Bye { reason: "host_stopped".into() }).await;
                    return Ok("host_stopped".to_string());
                }
            }
        }
    };
    tokio::select! {
        r = main => r,
        _ = reader_fut => unreachable!("reader future never completes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(width_px: u32, height_px: u32) -> DisplayInfo {
        DisplayInfo {
            width_px,
            height_px,
            width_mm: 0,
            height_mm: 0,
            refresh_hz: 60,
            scale_percent: 200,
            refresh_rates: vec![],
        }
    }

    #[test]
    fn extend_stream_fits_h264() {
        let s = extend_stream_for(&display(4480, 2520));
        assert_eq!((s.width, s.height), (4096, 2304), "a 4.5K iMac is scaled down to what H.264 carries");
        let s = extend_stream_for(&display(2880, 1801));
        assert_eq!((s.width, s.height), (2880, 1800), "smaller screens keep their size");
    }
}
