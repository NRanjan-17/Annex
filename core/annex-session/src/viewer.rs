//! Viewer side: connects to a host, authorizes it, requests a mirror session and delivers
//! frames plus latency statistics.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use annex_proto::{Codec, Control, DisplayInfo, InputEvent, Mode, Quality, Role, SessionRequest};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::clock::{ClockSync, now_us};
use crate::conn::{Incoming, Reader, Writer, check_hello, hello};
use crate::events::LinkKind;
use crate::events::{Event, EventSink, FrameOut, FrameSink};
use crate::pairing::{PairContext, pair_as_viewer};
use crate::stats::LatencyWindow;
use crate::transport::{Established, connect_quic, connect_tcp, is_link_local};
use crate::{Core, SessionError};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(130);
const STATS_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct ViewerConfig {
    /// `host`, `host:port`, `ip` or `ip:port`; several comma-separated candidates are raced
    /// (direct-cable addresses first, QUIC before TCP).
    pub address: String,
    pub display: DisplayInfo,
    pub codecs: Vec<Codec>,
    /// If set (e.g. from discovery for an already-trusted host), the TLS handshake fails
    /// unless the host presents exactly this certificate.
    pub expected_fingerprint: Option<String>,
    /// Mirror the host's screen, or extend it with a new virtual display.
    pub mode: Mode,
    /// Ask to control the host with this computer's mouse and keyboard. The host decides.
    pub input: bool,
    /// Bitrate ceiling the host should respect.
    pub quality: Quality,
    /// Optional per-frame latency log: `frame_id,capture_local_us,present_us,latency_us`.
    pub latency_csv: Option<PathBuf>,
}

enum Cmd {
    RequestKeyframe,
    Presented {
        frame_id: u32,
        at_us: i64,
    },
    Input(InputEvent),
    /// Direct-link messages (request / offer) for the host.
    Control(Control),
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

    /// Forwards one input event to the host. Dropped unless the host currently allows input.
    pub fn send_input(&self, event: InputEvent) {
        let _ = self.cmd.send(Cmd::Input(event));
    }

    /// Asks the host to set up a direct Wi-Fi link (answered by `direct_link_offer` or
    /// `direct_link_unavailable`).
    pub fn request_direct_link(&self) {
        let _ = self.cmd.send(Cmd::Control(Control::DirectLinkRequest));
    }

    /// Tells the host to join this hotspot (this computer runs it) for a direct link.
    pub fn offer_direct_link(&self, ssid: &str, passphrase: &str) {
        let _ =
            self.cmd.send(Cmd::Control(Control::DirectLinkOffer { ssid: ssid.into(), passphrase: passphrase.into() }));
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
    let est = connect_best(&core, &cfg).await?;
    let Established { kind, reader, writer, fingerprint: fp, exporter, remote, quic } = est;
    if fp == core.fingerprint() {
        return Err(SessionError::Violation("host uses our own identity".into()));
    }
    let link = if is_link_local(&remote) { LinkKind::Direct } else { LinkKind::Network };
    tracing::info!(%remote, ?kind, ?link, "connected");
    let mut reader = Reader::new(reader);
    let mut writer = Writer::new(writer);
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
            input: cfg.input,
            quality: cfg.quality,
        }))
        .await?;
    let (stream, input) = match timeout(ACCEPT_TIMEOUT, reader.next_control()).await?? {
        Control::SessionAccept { stream, input } => (stream, input),
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
        transport: kind,
        link,
        input,
    });
    // Flipped by the host's `input_allowed` messages.
    let input_allowed = Arc::new(std::sync::atomic::AtomicBool::new(input));
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

    // Hands a received frame to the app and remembers it for latency accounting.
    let deliver = {
        let latency = latency.clone();
        move |frame_id: u32, capture_ts_us: i64, keyframe: bool, data: &[u8]| {
            let local = latency.lock().unwrap().clock.to_local(capture_ts_us);
            if let Some(ts) = local {
                let mut l = latency.lock().unwrap();
                if l.pending.len() >= 256 {
                    l.pending.pop_front();
                }
                l.pending.push_back((frame_id, ts));
            }
            frames(FrameOut { frame_id, capture_ts_local_us: local, keyframe, data });
        }
    };

    // Counters for receiver reports (adaptive bitrate on the host).
    #[derive(Default)]
    struct Counters {
        frames: std::sync::atomic::AtomicU32,
        lost: std::sync::atomic::AtomicU32,
        shards: std::sync::atomic::AtomicU32,
        recovered: std::sync::atomic::AtomicU32,
    }
    let counters = Arc::new(Counters::default());
    let deliver = {
        let counters = counters.clone();
        let inner = deliver;
        move |frame_id: u32, capture_ts_us: i64, keyframe: bool, data: &[u8]| {
            counters.frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            inner(frame_id, capture_ts_us, keyframe, data)
        }
    };

    // QUIC: video arrives as FEC-protected datagrams; a lost frame triggers a keyframe request.
    let (lost_tx, mut lost_rx) = mpsc::unbounded_channel::<()>();
    let datagrams = {
        let deliver = deliver.clone();
        let quic = quic.clone();
        let counters = counters.clone();
        async move {
            let Some(conn) = quic else { return std::future::pending::<()>().await };
            let mut reassembler = annex_proto::fragment::Reassembler::default();
            while let Ok(d) = conn.read_datagram().await {
                counters.shards.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let recovered_before = reassembler.shards_recovered;
                match reassembler.push(&d) {
                    Ok(out) => {
                        let rebuilt = (reassembler.shards_recovered - recovered_before) as u32;
                        counters.recovered.fetch_add(rebuilt, std::sync::atomic::Ordering::Relaxed);
                        if !out.lost.is_empty() {
                            counters.lost.fetch_add(out.lost.len() as u32, std::sync::atomic::Ordering::Relaxed);
                            let _ = lost_tx.send(());
                        }
                        for f in out.frames {
                            deliver(f.frame_id, f.capture_ts_us, f.flags.keyframe(), &f.data);
                        }
                    }
                    Err(e) => tracing::debug!("bad video datagram: {e}"),
                }
            }
            std::future::pending::<()>().await
        }
    };

    let reader_fut = {
        let latency = latency.clone();
        let events = events.clone();
        let input_allowed = input_allowed.clone();
        let deliver = deliver.clone();
        async move {
            loop {
                match reader.next().await? {
                    Incoming::Video(f) => {
                        deliver(f.header.frame_id, f.header.capture_ts_us, f.header.flags.keyframe(), &f.data);
                    }
                    Incoming::Control(Control::ClockPong { t_viewer_us, t_host_us, .. }) => {
                        latency.lock().unwrap().clock.add_sample(t_viewer_us, t_host_us, now_us());
                    }
                    Incoming::Control(Control::StreamConfig(stream)) => events(Event::StreamConfig { stream }),
                    Incoming::Control(Control::DirectLinkOffer { ssid, passphrase }) => {
                        events(Event::DirectLinkOffer { session_id: 0, ssid, passphrase });
                    }
                    Incoming::Control(Control::DirectLinkUnavailable { reason }) => {
                        events(Event::DirectLinkUnavailable { session_id: 0, reason });
                    }
                    Incoming::Control(Control::InputAllowed { allowed }) => {
                        input_allowed.store(allowed, std::sync::atomic::Ordering::Release);
                        events(Event::InputAllowed { session_id: 0, allowed });
                    }
                    Incoming::Control(Control::Bye { reason }) => return Ok::<String, SessionError>(reason),
                    Incoming::Control(other) => tracing::debug!(?other, "ignoring control message"),
                }
            }
        }
    };

    let main = async {
        let mut seq = 0u32;
        let mut last_keyframe_request = std::time::Instant::now() - Duration::from_secs(1);
        // Ping quickly at first so the clock offset converges, then settle down.
        let mut ping = tokio::time::interval(Duration::from_millis(250));
        let mut stats = tokio::time::interval(STATS_INTERVAL);
        stats.tick().await;
        let mut report = tokio::time::interval(Duration::from_secs(1));
        report.tick().await;
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
                _ = report.tick() => {
                    use std::sync::atomic::Ordering::Relaxed;
                    let rtt_us = match &quic {
                        Some(c) => c.rtt().as_micros().min(u32::MAX as u128) as u32,
                        None => latency.lock().unwrap().clock.rtt_us().unwrap_or(0).clamp(0, u32::MAX as i64) as u32,
                    };
                    let r = annex_proto::ReceiverReport {
                        frames: counters.frames.swap(0, Relaxed),
                        lost: counters.lost.swap(0, Relaxed),
                        shards: counters.shards.swap(0, Relaxed),
                        recovered: counters.recovered.swap(0, Relaxed),
                        rtt_us,
                    };
                    writer.send_control(&Control::ReceiverReport(r)).await?;
                }
                Some(()) = lost_rx.recv() => {
                    // At most a few requests per second; the host answers with one IDR anyway.
                    if last_keyframe_request.elapsed() > Duration::from_millis(250) {
                        last_keyframe_request = std::time::Instant::now();
                        writer.send_control(&Control::KeyframeRequest).await?;
                    }
                }
                c = cmd_rx.recv() => match c {
                    Some(Cmd::RequestKeyframe) => writer.send_control(&Control::KeyframeRequest).await?,
                    Some(Cmd::Control(c)) => writer.send_control(&c).await?,
                    Some(Cmd::Input(e)) => {
                        // Never send keystrokes to a host that hasn't asked for them.
                        if input_allowed.load(std::sync::atomic::Ordering::Acquire) {
                            writer.send_control(&Control::Input(e)).await?;
                        }
                    }
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
                        if let Some(conn) = &quic {
                            // Give the bye time to arrive before closing the QUIC connection.
                            let _ = timeout(Duration::from_millis(300), conn.closed()).await;
                            conn.close(0u32.into(), b"bye");
                        }
                        return Ok::<String, SessionError>("user".into());
                    }
                },
            }
        }
    };

    tokio::select! {
        r = main => r,
        r = reader_fut => r,
        _ = datagrams => unreachable!("datagram reader never completes"),
    }
}

/// Resolves every candidate address and races connections: direct-cable (link-local) addresses
/// first, QUIC first, falling back to TCP if no QUIC handshake succeeds.
async fn connect_best(core: &Arc<Core>, cfg: &ViewerConfig) -> Result<Established, SessionError> {
    let mut addrs = Vec::new();
    for part in cfg.address.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match resolve(part).await {
            Ok(a) => addrs.push(a),
            Err(e) => tracing::info!("{part}: {e}"),
        }
    }
    if addrs.is_empty() {
        return Err(SessionError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "address did not resolve")));
    }
    addrs.sort_by_key(|a| !is_link_local(a));
    addrs.dedup();
    let mut last_err = None;
    for quic in [true, false] {
        let mut tries = tokio::task::JoinSet::new();
        for (i, addr) in addrs.iter().copied().enumerate() {
            let (core, expected) = (core.clone(), cfg.expected_fingerprint.clone());
            tries.spawn(async move {
                // Give earlier (preferred) candidates a small head start.
                tokio::time::sleep(Duration::from_millis(150 * i as u64)).await;
                if quic { connect_quic(&core, addr, expected).await } else { connect_tcp(&core, addr, expected).await }
            });
        }
        let deadline = tokio::time::sleep(if quic { Duration::from_secs(3) } else { Duration::from_secs(8) });
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                r = tries.join_next() => match r {
                    Some(Ok(Ok(est))) => {
                        tries.abort_all();
                        return Ok(est);
                    }
                    Some(Ok(Err(e))) => {
                        tracing::info!(quic, "connect attempt failed: {e}");
                        last_err = Some(e);
                    }
                    Some(Err(_)) => {}
                    None => break,
                },
                _ = &mut deadline => {
                    tracing::info!(quic, "connect attempts timed out");
                    tries.abort_all();
                    break;
                }
            }
        }
        if quic {
            tracing::warn!("QUIC did not connect; trying TCP (video over TCP stalls when packets are lost)");
        }
    }
    Err(last_err.unwrap_or(SessionError::Timeout("connect")))
}
