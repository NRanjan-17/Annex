//! Host side: listens for viewers, authorizes them and streams encoded frames.
//!
//! Up to [`MAX_SESSIONS`] viewers at once. **Mirror** sessions all receive the same frames
//! ([`HostHandle::submit_frame`]); each **Extend** session gets its own virtual display and
//! frames ([`HostHandle::submit_session_frame`]). Extra viewers are rejected with `busy`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use annex_proto::{Codec, Control, DisplayInfo, Mode, Role, StreamConfig, VideoFlags, VideoHeader};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::clock::now_us;
use crate::conn::{Incoming, Reader, Writer, check_hello, hello};
use crate::events::{Event, EventSink};
use crate::pairing::{PairContext, pair_as_host};
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
    pub fn refuse_direct_link(&self, session_id: SessionId, reason: &str) -> bool {
        self.send_to(session_id, Control::DirectLinkUnavailable { reason: reason.into() })
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

/// Initial stream for an Extend session: the viewer's native resolution (even), ≤ 60 fps.
pub fn extend_stream_for(display: &DisplayInfo) -> StreamConfig {
    StreamConfig {
        codec: Codec::H264,
        width: display.width_px.max(2) & !1,
        height: display.height_px.max(2) & !1,
        fps: if display.refresh_hz == 0 { 60 } else { display.refresh_hz.clamp(24, 60) },
    }
}

/// Binds the listener and starts accepting viewers. Must be called inside a tokio runtime.
pub async fn start_host(core: Arc<Core>, cfg: HostConfig, events: EventSink) -> Result<HostHandle, SessionError> {
    let listener = TcpListener::bind(("0.0.0.0", cfg.port)).await?;
    let port = listener.local_addr()?.port();
    let tls = tokio_rustls::TlsAcceptor::from(Arc::new(annex_crypto::server_config(core.identity())?));
    // QUIC on the same port number (UDP). If UDP can't be bound, viewers fall back to TCP.
    let quic = match quinn::Endpoint::server(quic_server_config(&core)?, (std::net::Ipv4Addr::UNSPECIFIED, port).into())
    {
        Ok(e) => Some(e),
        Err(e) => {
            tracing::warn!("QUIC unavailable, TCP only: {e}");
            None
        }
    };
    let (shutdown, shutdown_rx) = watch::channel(false);
    let shared = Arc::new(Shared {
        core,
        events: events.clone(),
        extend: cfg.extend,
        mirror_stream: Mutex::new(cfg.stream),
        sessions: Mutex::new(HashMap::new()),
        claimed: AtomicUsize::new(0),
        next_id: AtomicU64::new(1),
    });
    events(Event::Listening { port });
    let task = tokio::spawn(accept_loop(listener, tls, quic, shared.clone(), shutdown_rx));
    Ok(HostHandle { shared, port, shutdown, task: Mutex::new(Some(task)) })
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
                    tracing::info!(%addr, "incoming TCP connection");
                    let (tls, shared, shutdown) = (tls.clone(), shared.clone(), shutdown.clone());
                    sessions.spawn(async move {
                        match accept_tcp(sock, tls).await {
                            Ok(est) => handle_conn(est, shared, shutdown).await,
                            Err(e) => tracing::info!("TCP handshake failed: {e}"),
                        }
                    });
                }
                Err(e) => tracing::warn!("accept failed: {e}"),
            },
            Some(incoming) = quic_accept(&quic) => {
                tracing::info!(addr = %incoming.remote_address(), "incoming QUIC connection");
                let (shared, shutdown) = (shared.clone(), shutdown.clone());
                sessions.spawn(async move {
                    match accept_quic(incoming).await {
                        Ok(est) => handle_conn(est, shared, shutdown).await,
                        Err(e) => tracing::info!("QUIC handshake failed: {e}"),
                    }
                });
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
    let link = Link { transport: kind, direct: is_link_local(&remote) };
    let result =
        run_session(&mut reader, &mut writer, &fp, &exporter, &shared, shutdown, id, link, &video, &mut started).await;
    shared.sessions.lock().unwrap().remove(&id);
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
    direct: bool,
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
    started: &mut bool,
) -> Result<String, SessionError> {
    if fp == shared.core.fingerprint() {
        return Err(SessionError::Violation("peer uses our own identity".into()));
    }
    let knows = shared.core.knows(fp);
    let caps: &[&str] = if shared.extend { &["h264", "extend"] } else { &["h264"] };
    writer.send_control(&hello(&shared.core, vec![Role::Host], knows, caps)).await?;
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
    });
    shared.sessions.lock().unwrap().insert(id, slot.clone());
    *started = true;
    (shared.events)(Event::SessionStarted {
        session_id: id,
        mode: req.mode,
        peer_name: peer.device_name.clone(),
        fingerprint: fp.to_string(),
        viewer_display: Some(req.display.clone()),
        stream,
        transport: link.transport,
        link: if link.direct { crate::events::LinkKind::Direct } else { crate::events::LinkKind::Network },
        input,
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
    stream_rx.mark_unchanged();
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
    let mut rate = crate::rate::RateController::new(req.quality);
    if rate.scale_percent() != 100 {
        // The viewer chose a lower quality: start the encoder there rather than at 100 %.
        (shared.events)(Event::BitrateHint { session_id: id, scale_percent: rate.scale_percent() });
    }
    let main = async {
        loop {
            tokio::select! {
                f = frame_rx.recv() => match f {
                    Some(f) => video.send(writer, f.header, &f.data).await?,
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
