//! Viewer side: connects to a host, authorizes it, requests a mirror session and delivers
//! frames plus latency statistics.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use annex_crypto::peer_fingerprint;
use annex_proto::{Codec, Control, DisplayInfo, Mode, Role, SessionRequest};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::clock::{ClockSync, now_us};
use crate::conn::{Incoming, Reader, Writer, check_hello, hello};
use crate::events::{Event, EventSink, FrameOut, FrameSink};
use crate::pairing::{PairContext, pair_as_viewer};
use crate::stats::LatencyWindow;
use crate::{Core, SessionError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(130);
const STATS_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct ViewerConfig {
    /// `host`, `host:port`, `ip` or `ip:port`.
    pub address: String,
    pub display: DisplayInfo,
    pub codecs: Vec<Codec>,
    /// If set (e.g. from discovery for an already-trusted host), the TLS handshake fails
    /// unless the host presents exactly this certificate.
    pub expected_fingerprint: Option<String>,
    /// Mirror the host's screen, or extend it with a new virtual display.
    pub mode: Mode,
    /// Optional per-frame latency log: `frame_id,capture_local_us,present_us,latency_us`.
    pub latency_csv: Option<PathBuf>,
}

enum Cmd {
    RequestKeyframe,
    Presented { frame_id: u32, at_us: i64 },
    Disconnect,
}

pub struct ViewerHandle {
    cmd: mpsc::UnboundedSender<Cmd>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl ViewerHandle {
    pub fn request_keyframe(&self) {
        let _ = self.cmd.send(Cmd::RequestKeyframe);
    }

    /// Reports that `frame_id` reached the screen now; feeds the latency statistics.
    pub fn frame_presented(&self, frame_id: u32) {
        let _ = self.cmd.send(Cmd::Presented { frame_id, at_us: now_us() });
    }

    /// Sends `Bye`, closes the connection and waits for the session task to finish.
    pub async fn disconnect(&self) {
        let _ = self.cmd.send(Cmd::Disconnect);
        let task = self.task.lock().unwrap().take();
        if let Some(t) = task {
            let _ = t.await;
        }
    }
}

/// Starts connecting in the background. Must be called inside a tokio runtime.
/// Progress is reported through `events`; the last event is always `SessionEnded`.
pub fn connect_viewer(core: Arc<Core>, cfg: ViewerConfig, events: EventSink, frames: FrameSink) -> ViewerHandle {
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        // Peer-sent reasons stay bare codes (`pairing_failed`, `busy`, ...) for the apps.
        let reason = match run(core, cfg, events.clone(), frames, cmd_rx).await {
            Ok(r) | Err(SessionError::Rejected(r)) => r,
            Err(e) => e.to_string(),
        };
        events(Event::SessionEnded { session_id: 0, reason });
    });
    ViewerHandle { cmd, task: Mutex::new(Some(task)) }
}

async fn resolve(address: &str) -> Result<std::net::SocketAddr, SessionError> {
    if let Ok(a) = address.parse::<std::net::SocketAddr>() {
        return Ok(a);
    }
    if let Ok(ip) = address.parse::<std::net::IpAddr>() {
        return Ok((ip, annex_proto::DEFAULT_PORT).into());
    }
    let with_port =
        if address.contains(':') { address.to_string() } else { format!("{address}:{}", annex_proto::DEFAULT_PORT) };
    tokio::net::lookup_host(with_port)
        .await?
        .next()
        .ok_or_else(|| SessionError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "address did not resolve")))
}

#[derive(Default)]
struct Latency {
    clock: ClockSync,
    /// Recently delivered frames awaiting a `Presented` report: (frame_id, capture_local_us).
    pending: VecDeque<(u32, i64)>,
    window: LatencyWindow,
}

async fn run(
    core: Arc<Core>,
    cfg: ViewerConfig,
    events: EventSink,
    frames: FrameSink,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
) -> Result<String, SessionError> {
    events(Event::Connecting { address: cfg.address.clone() });
    let addr = resolve(&cfg.address).await?;
    let sock =
        timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await.map_err(|_| SessionError::Timeout("connect"))??;
    sock.set_nodelay(true)?;
    let tls_cfg = annex_crypto::client_config(core.identity(), cfg.expected_fingerprint.clone())?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_cfg));
    let name = ServerName::try_from(annex_crypto::SERVER_NAME).expect("valid server name");
    let tls =
        timeout(HANDSHAKE_TIMEOUT, connector.connect(name, sock)).await.map_err(|_| SessionError::Timeout("tls"))??;
    let fp = peer_fingerprint(tls.get_ref().1.peer_certificates())
        .ok_or_else(|| SessionError::Violation("host sent no certificate".into()))?;
    let exporter = tls
        .get_ref()
        .1
        .export_keying_material([0u8; 32], annex_crypto::pairing::EXPORTER_LABEL, None)
        .map_err(|e| SessionError::Violation(format!("tls exporter: {e}")))?;
    if fp == core.fingerprint() {
        return Err(SessionError::Violation("host uses our own identity".into()));
    }

    let (r, w) = tokio::io::split(tls);
    let mut reader = Reader::new(r);
    let mut writer = Writer::new(w);
    let knows = core.knows(&fp);
    writer.send_control(&hello(&core, vec![Role::Viewer], knows, &["h264"])).await?;
    let peer = match timeout(HANDSHAKE_TIMEOUT, reader.next_control()).await?? {
        Control::SessionReject { reason } => return Err(SessionError::Rejected(reason)),
        other => check_hello(other, &fp)?,
    };
    if knows && peer.knows_peer {
        core.touch(&fp);
    } else {
        let cx = PairContext {
            core: &core,
            events: &events,
            peer_fp: &fp,
            peer_name: &peer.device_name,
            exporter: &exporter,
        };
        let paired = pair_as_viewer(cx, &mut reader, &mut writer).await;
        if paired.is_err() {
            writer.shutdown().await;
        }
        paired?;
    }
    writer
        .send_control(&Control::SessionRequest(SessionRequest {
            mode: cfg.mode,
            display: cfg.display.clone(),
            codecs: cfg.codecs.clone(),
            input: false,
        }))
        .await?;
    let stream = match timeout(ACCEPT_TIMEOUT, reader.next_control()).await?? {
        Control::SessionAccept { stream } => stream,
        Control::SessionReject { reason } | Control::Bye { reason } => return Err(SessionError::Rejected(reason)),
        _ => return Err(SessionError::Violation("expected session_accept".into())),
    };
    events(Event::SessionStarted {
        session_id: 0,
        mode: cfg.mode,
        peer_name: peer.device_name.clone(),
        fingerprint: fp.clone(),
        viewer_display: None,
        stream: stream.clone(),
    });
    events(Event::StreamConfig { stream });

    let latency = Arc::new(Mutex::new(Latency::default()));
    let mut csv = match &cfg.latency_csv {
        Some(p) => {
            let mut f = std::fs::File::create(p)?;
            writeln!(f, "frame_id,capture_local_us,present_us,latency_us")?;
            Some(f)
        }
        None => None,
    };

    let reader_fut = {
        let latency = latency.clone();
        let events = events.clone();
        async move {
            loop {
                match reader.next().await? {
                    Incoming::Video(f) => {
                        let local = latency.lock().unwrap().clock.to_local(f.header.capture_ts_us);
                        if let Some(ts) = local {
                            let mut l = latency.lock().unwrap();
                            if l.pending.len() >= 256 {
                                l.pending.pop_front();
                            }
                            l.pending.push_back((f.header.frame_id, ts));
                        }
                        frames(FrameOut {
                            frame_id: f.header.frame_id,
                            capture_ts_local_us: local,
                            keyframe: f.header.flags.keyframe(),
                            data: &f.data,
                        });
                    }
                    Incoming::Control(Control::ClockPong { t_viewer_us, t_host_us, .. }) => {
                        latency.lock().unwrap().clock.add_sample(t_viewer_us, t_host_us, now_us());
                    }
                    Incoming::Control(Control::StreamConfig(stream)) => events(Event::StreamConfig { stream }),
                    Incoming::Control(Control::Bye { reason }) => return Ok::<String, SessionError>(reason),
                    Incoming::Control(other) => tracing::debug!(?other, "ignoring control message"),
                }
            }
        }
    };

    let main = async {
        let mut seq = 0u32;
        // Ping quickly at first so the clock offset converges, then settle down.
        let mut ping = tokio::time::interval(Duration::from_millis(250));
        let mut stats = tokio::time::interval(STATS_INTERVAL);
        stats.tick().await;
        loop {
            tokio::select! {
                _ = ping.tick() => {
                    seq = seq.wrapping_add(1);
                    if seq == 20 {
                        ping = tokio::time::interval(Duration::from_secs(1));
                        ping.tick().await;
                    }
                    writer.send_control(&Control::ClockPing { seq, t_viewer_us: now_us() }).await?;
                }
                _ = stats.tick() => {
                    let mut l = latency.lock().unwrap();
                    let rtt = l.clock.rtt_us().map(|r| r as f64 / 1000.0);
                    if let Some(s) = l.window.take() {
                        drop(l);
                        events(Event::LatencyStats { stats: s, clock_rtt_ms: rtt });
                    }
                }
                c = cmd_rx.recv() => match c {
                    Some(Cmd::RequestKeyframe) => writer.send_control(&Control::KeyframeRequest).await?,
                    Some(Cmd::Presented { frame_id, at_us }) => {
                        let mut l = latency.lock().unwrap();
                        if let Some(pos) = l.pending.iter().position(|(id, _)| *id == frame_id) {
                            let (_, cap) = l.pending.remove(pos).unwrap();
                            l.window.push(at_us - cap, at_us);
                            if let Some(f) = csv.as_mut() {
                                let _ = writeln!(f, "{frame_id},{cap},{at_us},{}", at_us - cap);
                            }
                        }
                    }
                    Some(Cmd::Disconnect) | None => {
                        let _ = writer.send_control(&Control::Bye { reason: "user".into() }).await;
                        writer.shutdown().await;
                        return Ok::<String, SessionError>("user".into());
                    }
                },
            }
        }
    };

    tokio::select! {
        r = main => r,
        r = reader_fut => r,
    }
}
