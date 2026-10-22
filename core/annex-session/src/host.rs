//! Host side: listens for viewers, authorizes them and streams encoded frames.
//! M1 serves one viewer at a time; extra viewers are rejected with `busy`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use annex_crypto::peer_fingerprint;
use annex_proto::{Control, Mode, Role, StreamConfig, VideoFlags, VideoHeader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::clock::now_us;
use crate::conn::{Incoming, Reader, Writer, check_hello, hello};
use crate::events::{Event, EventSink, PeerRole};
use crate::{Core, SessionError};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Long enough for the user on the viewer side to answer its own trust prompt.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(130);
/// Frames queued between the encoder and the socket. Small on purpose: on a slow link we
/// would rather drop frames and resync with a keyframe than build up latency.
const FRAME_QUEUE: usize = 4;

#[derive(Debug, Clone)]
pub struct HostConfig {
    /// TCP port to listen on (0 = pick a free port).
    pub port: u16,
    /// Initial stream description; update with [`HostHandle::set_stream`].
    pub stream: StreamConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitResult {
    Sent,
    /// No viewer connected; the frame was discarded.
    NoSession,
    /// The link is congested, or we are waiting for a keyframe after a drop.
    Dropped,
}

struct OutFrame {
    header: VideoHeader,
    data: Vec<u8>,
}

struct Shared {
    core: Arc<Core>,
    events: EventSink,
    busy: AtomicBool,
    active: Mutex<Option<mpsc::Sender<OutFrame>>>,
    stream: watch::Sender<StreamConfig>,
    next_frame_id: AtomicU32,
    need_keyframe: AtomicBool,
}

impl Shared {
    fn request_keyframe(&self) {
        // Only notify once per outstanding request.
        if !self.need_keyframe.swap(true, Ordering::AcqRel) {
            (self.events)(Event::KeyframeRequested);
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

    pub fn set_stream(&self, stream: StreamConfig) {
        self.shared.stream.send_replace(stream);
    }

    pub fn has_session(&self) -> bool {
        self.shared.active.lock().unwrap().is_some()
    }

    /// Queues one encoded access unit. Safe to call from any thread; never blocks.
    pub fn submit_frame(&self, data: &[u8], capture_ts_us: i64, keyframe: bool) -> SubmitResult {
        let guard = self.shared.active.lock().unwrap();
        let Some(tx) = guard.as_ref() else { return SubmitResult::NoSession };
        if keyframe {
            self.shared.need_keyframe.store(false, Ordering::Release);
        } else if self.shared.need_keyframe.load(Ordering::Acquire) {
            // Deltas after a drop would decode as garbage; wait for the keyframe.
            return SubmitResult::Dropped;
        }
        let frame = OutFrame {
            header: VideoHeader {
                frame_id: self.shared.next_frame_id.fetch_add(1, Ordering::Relaxed),
                capture_ts_us,
                flags: VideoFlags(if keyframe { VideoFlags::KEYFRAME } else { 0 }),
            },
            data: data.to_vec(),
        };
        match tx.try_send(frame) {
            Ok(()) => SubmitResult::Sent,
            Err(mpsc::error::TrySendError::Full(_)) => {
                drop(guard);
                self.shared.request_keyframe();
                SubmitResult::Dropped
            }
            Err(mpsc::error::TrySendError::Closed(_)) => SubmitResult::NoSession,
        }
    }

    /// Stops listening, ends any session and waits for tasks to finish.
    pub async fn stop(&self) {
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(t) = task {
            let _ = t.await;
        }
    }
}

/// Binds the listener and starts accepting viewers. Must be called inside a tokio runtime.
pub async fn start_host(core: Arc<Core>, cfg: HostConfig, events: EventSink) -> Result<HostHandle, SessionError> {
    let listener = TcpListener::bind(("0.0.0.0", cfg.port)).await?;
    let port = listener.local_addr()?.port();
    let tls = tokio_rustls::TlsAcceptor::from(Arc::new(annex_crypto::server_config(core.identity())?));
    let (shutdown, shutdown_rx) = watch::channel(false);
    let shared = Arc::new(Shared {
        core,
        events: events.clone(),
        busy: AtomicBool::new(false),
        active: Mutex::new(None),
        stream: watch::Sender::new(cfg.stream),
        next_frame_id: AtomicU32::new(0),
        need_keyframe: AtomicBool::new(true),
    });
    events(Event::Listening { port });
    let task = tokio::spawn(accept_loop(listener, tls, shared.clone(), shutdown_rx));
    Ok(HostHandle { shared, port, shutdown, task: Mutex::new(Some(task)) })
}

async fn accept_loop(
    listener: TcpListener,
    tls: tokio_rustls::TlsAcceptor,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut sessions = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            r = listener.accept() => match r {
                Ok((sock, addr)) => {
                    tracing::info!(%addr, "incoming connection");
                    sessions.spawn(handle_conn(sock, tls.clone(), shared.clone(), shutdown.clone()));
                }
                Err(e) => tracing::warn!("accept failed: {e}"),
            },
            _ = shutdown.changed() => break,
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
        }
    }
    while sessions.join_next().await.is_some() {}
}

async fn handle_conn(
    sock: TcpStream,
    tls: tokio_rustls::TlsAcceptor,
    shared: Arc<Shared>,
    shutdown: watch::Receiver<bool>,
) {
    let _ = sock.set_nodelay(true);
    let stream = match timeout(HANDSHAKE_TIMEOUT, tls.accept(sock)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return tracing::info!("tls handshake failed: {e}"),
        Err(_) => return tracing::info!("tls handshake timed out"),
    };
    let Some(fp) = peer_fingerprint(stream.get_ref().1.peer_certificates()) else {
        return tracing::warn!("peer sent no certificate");
    };
    let (r, w) = tokio::io::split(stream);
    let mut reader = Reader::new(r);
    let mut writer = Writer::new(w);

    // Claim the single session slot before prompting anyone.
    if shared.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        let _ = writer.send_control(&Control::SessionReject { reason: "busy".into() }).await;
        writer.shutdown().await;
        return;
    }
    let mut started = false;
    let result = run_session(&mut reader, &mut writer, &fp, &shared, shutdown, &mut started).await;
    *shared.active.lock().unwrap() = None;
    shared.busy.store(false, Ordering::Release);
    let reason = match result {
        Ok(reason) => reason,
        Err(SessionError::NotTrusted) => {
            let _ = writer.send_control(&Control::Bye { reason: "not_trusted".into() }).await;
            "not_trusted".into()
        }
        Err(e) => e.to_string(),
    };
    writer.shutdown().await;
    if started {
        (shared.events)(Event::SessionEnded { reason });
    } else {
        tracing::info!("connection closed before session start: {reason}");
    }
}

async fn run_session<R, W>(
    reader: &mut Reader<R>,
    writer: &mut Writer<W>,
    fp: &str,
    shared: &Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
    started: &mut bool,
) -> Result<String, SessionError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin,
{
    writer.send_control(&hello(&shared.core, vec![Role::Host])).await?;
    let peer = check_hello(timeout(HANDSHAKE_TIMEOUT, reader.next_control()).await??, fp)?;
    shared.core.authorize(fp, &peer.device_name, PeerRole::Viewer, &shared.events).await?;

    let req = match timeout(REQUEST_TIMEOUT, reader.next_control()).await?? {
        Control::SessionRequest(r) => r,
        Control::Bye { reason } => return Err(SessionError::Rejected(reason)),
        _ => return Err(SessionError::Violation("expected session_request".into())),
    };
    let stream = shared.stream.borrow().clone();
    let reject = if req.mode != Mode::Mirror {
        Some("extend_not_supported")
    } else if !req.codecs.contains(&stream.codec) {
        Some("no_common_codec")
    } else {
        None
    };
    if let Some(reason) = reject {
        writer.send_control(&Control::SessionReject { reason: reason.into() }).await?;
        return Ok(reason.into());
    }
    writer.send_control(&Control::SessionAccept { stream: stream.clone() }).await?;

    let (frame_tx, mut frame_rx) = mpsc::channel::<OutFrame>(FRAME_QUEUE);
    shared.need_keyframe.store(false, Ordering::Release);
    *shared.active.lock().unwrap() = Some(frame_tx);
    *started = true;
    (shared.events)(Event::SessionStarted {
        peer_name: peer.device_name.clone(),
        fingerprint: fp.to_string(),
        viewer_display: Some(req.display.clone()),
        stream,
    });
    shared.request_keyframe();

    // The reader runs independently so control messages are handled while we write video.
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<Result<Control, SessionError>>(32);
    let mut stream_rx = shared.stream.subscribe();
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
    let main = async {
        loop {
            tokio::select! {
                f = frame_rx.recv() => match f {
                    Some(f) => writer.send_video(f.header, &f.data).await?,
                    None => return Ok("stopped".to_string()),
                },
                c = ctl_rx.recv() => match c {
                    Some(Ok(Control::KeyframeRequest)) => shared.request_keyframe(),
                    Some(Ok(Control::ClockPing { seq, t_viewer_us })) => {
                        writer.send_control(&Control::ClockPong { seq, t_viewer_us, t_host_us: now_us() }).await?;
                    }
                    Some(Ok(Control::Bye { reason })) => return Ok(reason),
                    Some(Ok(other)) => tracing::debug!(?other, "ignoring control message"),
                    Some(Err(e)) => return Err(e),
                    None => return Err(SessionError::Closed),
                },
                Ok(()) = stream_rx.changed() => {
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
