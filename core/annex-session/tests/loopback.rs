//! End-to-end session tests over real TLS on 127.0.0.1.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use annex_session::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

fn stream() -> StreamConfig {
    StreamConfig { codec: Codec::H264, width: 1920, height: 1080, fps: 60 }
}

fn display() -> DisplayInfo {
    DisplayInfo {
        width_px: 1920,
        height_px: 1080,
        width_mm: 527,
        height_mm: 296,
        refresh_hz: 60,
        scale_percent: 100,
        refresh_rates: vec![],
    }
}

fn viewer_cfg(port: u16) -> ViewerConfig {
    ViewerConfig {
        address: format!("127.0.0.1:{port}"),
        display: display(),
        codecs: vec![Codec::H264],
        expected_fingerprint: None,
        mode: Mode::Mirror,
        input: false,
        quality: annex_session::Quality::Best,
        latency_csv: None,
        internet: None,
        join: None,
    }
}

fn host_cfg() -> HostConfig {
    HostConfig { port: 0, stream: stream(), extend: true, udp: None }
}

fn channel_sink() -> (EventSink, mpsc::UnboundedReceiver<Event>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Arc::new(move |e| {
            let _ = tx.send(e);
        }),
        rx,
    )
}

async fn wait_for(rx: &mut mpsc::UnboundedReceiver<Event>, pred: impl Fn(&Event) -> bool) -> Event {
    // Generous: CI runners are slow and run every test at once.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let e = rx.recv().await.expect("event channel closed");
            if pred(&e) {
                return e;
            }
        }
    })
    .await
    .expect("timed out waiting for event")
}

#[tokio::test(flavor = "multi_thread")]
async fn mirror_session_delivers_frames_and_stats() {
    let host_core = Core::ephemeral("Host", TrustPolicy::AcceptAllInsecure).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::AcceptAllInsecure).unwrap();
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core.clone(), host_cfg(), hev).await.unwrap();

    let received = Arc::new(Mutex::new(Vec::<(u32, bool, Vec<u8>)>::new()));
    let (vev, mut vrx) = channel_sink();
    let (fid_tx, mut fid_rx) = mpsc::unbounded_channel();
    let frames: FrameSink = {
        let received = received.clone();
        Arc::new(move |f: FrameOut<'_>| {
            received.lock().unwrap().push((f.frame_id, f.keyframe, f.data.to_vec()));
            let _ = fid_tx.send(f.frame_id);
        })
    };
    let viewer = Arc::new(connect_viewer(viewer_core.clone(), viewer_cfg(host.port()), vev, frames));
    let presenter = {
        let viewer = viewer.clone();
        tokio::spawn(async move {
            while let Some(id) = fid_rx.recv().await {
                viewer.frame_presented(id);
            }
        })
    };

    let started = wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    let Event::SessionStarted { peer_name, viewer_display, .. } = started else { unreachable!() };
    assert_eq!(peer_name, "Viewer");
    assert_eq!(viewer_display, Some(display()));
    wait_for(&mut hrx, |e| matches!(e, Event::KeyframeRequested { .. })).await;
    let Event::StreamConfig { stream: s } = wait_for(&mut vrx, |e| matches!(e, Event::StreamConfig { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(s, stream());

    // A delta before the first keyframe is dropped; the keyframe and following deltas flow.
    assert_eq!(host.submit_frame(b"delta-too-early", 0, false), SubmitResult::Dropped);
    let mut sent = 0;
    for i in 0..150u32 {
        let r = host.submit_frame(format!("frame{i}").as_bytes(), clock::now_us(), i % 60 == 0);
        if r == SubmitResult::Sent {
            sent += 1;
        }
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    assert!(sent > 100, "only {sent} frames sent");
    let stats = wait_for(&mut vrx, |e| matches!(e, Event::LatencyStats { .. })).await;
    let Event::LatencyStats { stats, clock_rtt_ms } = stats else { unreachable!() };
    assert!(stats.frames > 0);
    assert!(stats.p50_ms < 100.0, "loopback latency too high: {stats:?}");
    assert!(clock_rtt_ms.is_some());

    // Mid-session stream change reaches the viewer.
    host.set_stream(StreamConfig { width: 1280, height: 720, ..stream() });
    let Event::StreamConfig { stream: s } = wait_for(&mut vrx, |e| matches!(e, Event::StreamConfig { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(s.width, 1280);

    // Viewer asks for a keyframe.
    viewer.request_keyframe();
    wait_for(&mut hrx, |e| matches!(e, Event::KeyframeRequested { .. })).await;

    {
        let r = received.lock().unwrap();
        assert!(r[0].1, "first delivered frame must be a keyframe");
        assert_eq!(r[0].2, b"frame0");
        assert!(r.windows(2).all(|w| w[1].0 > w[0].0), "frame ids must increase");
    }

    viewer.disconnect().await;
    let Event::SessionEnded { reason, .. } = wait_for(&mut hrx, |e| matches!(e, Event::SessionEnded { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(reason, "user");
    assert_eq!(host.submit_frame(b"x", 0, true), SubmitResult::NoSession);
    presenter.abort();

    // The insecure test policy skips pairing and stores nothing.
    assert!(!host_core.is_trusted(viewer_core.fingerprint()));
    host.stop().await;
}

/// Runs one pairing attempt; the viewer types `typed` (`"RIGHT"` = the host's PIN, `None` = the
/// host cancels). Returns (host outcome, viewer outcome): "started" or the session-end reason.
async fn pair_attempt(host_core: &Arc<Core>, viewer_core: &Arc<Core>, typed: Option<&str>) -> (String, String) {
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core.clone(), host_cfg(), hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core.clone(), viewer_cfg(host.port()), vev, Arc::new(|_| {}));

    let Event::PairingPin { request_id: host_req, pin, peer_name, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::PairingPin { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(peer_name, "Viewer");
    assert_eq!(pin.len(), 7, "PIN is shown as `123 456`: {pin}");
    let Event::PairingPinRequired { request_id, peer_name, fingerprint_display } =
        wait_for(&mut vrx, |e| matches!(e, Event::PairingPinRequired { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(peer_name, "Host");
    assert_eq!(fingerprint_display.len(), 24);
    match typed {
        Some("RIGHT") => assert!(viewer_core.submit_pin(request_id, &pin)),
        Some(other) => assert!(viewer_core.submit_pin(request_id, other)),
        None => assert!(host_core.cancel_request(host_req)),
    }
    let Event::PairingFinished { success, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::PairingFinished { .. })).await
    else {
        unreachable!()
    };
    let outcome = |e: &Event| matches!(e, Event::SessionStarted { .. } | Event::SessionEnded { .. });
    let h = if success {
        wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
        "started".to_string()
    } else {
        "not_paired".to_string()
    };
    let v = match wait_for(&mut vrx, outcome).await {
        Event::SessionEnded { reason, .. } => reason,
        _ => "started".to_string(),
    };
    viewer.disconnect().await;
    host.stop().await;
    (h, v)
}

#[tokio::test(flavor = "multi_thread")]
async fn pin_pairing_succeeds_and_is_remembered() {
    let host_core = Core::ephemeral("Host", TrustPolicy::Pair).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::Pair).unwrap();
    assert_eq!(pair_attempt(&host_core, &viewer_core, Some("RIGHT")).await, ("started".into(), "started".into()));
    assert!(host_core.is_trusted(viewer_core.fingerprint()));
    assert!(viewer_core.is_trusted(host_core.fingerprint()));

    // Second connection: no pairing.
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core.clone(), host_cfg(), hev).await.unwrap();
    let (vev, _vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core.clone(), viewer_cfg(host.port()), vev, Arc::new(|_| {}));
    let e = wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. } | Event::PairingPin { .. })).await;
    assert!(matches!(e, Event::SessionStarted { .. }), "trusted peers must not pair again");
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_host_writes_the_pin_to_a_file_and_lets_paired_devices_control_it() {
    let dir = tempfile::tempdir().unwrap();
    let host_core = Core::open_with(dir.path(), "Host", TrustPolicy::Pair, KeyStorage::File).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::Pair).unwrap();
    let pin_file = host_core.pin_file().unwrap();

    // Not headless: no file.
    assert_eq!(pair_attempt(&host_core, &viewer_core, Some("000000")).await.0, "not_paired");
    assert!(!pin_file.exists());

    host_core.set_headless(true);
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core.clone(), host_cfg(), hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core.clone(), viewer_cfg(host.port()), vev, Arc::new(|_| {}));
    let Event::PairingPinRequired { request_id, .. } =
        wait_for(&mut vrx, |e| matches!(e, Event::PairingPinRequired { .. })).await
    else {
        unreachable!()
    };
    // Typed from the file, as someone logged in over SSH would.
    let text = std::fs::read_to_string(&pin_file).unwrap();
    assert!(text.contains("Viewer"), "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&pin_file).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(viewer_core.submit_pin(request_id, text.lines().next().unwrap()));
    wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    assert!(!pin_file.exists(), "the PIN goes once pairing ends");

    // Paired devices may control a headless computer without being allowed one by one.
    let fp = viewer_core.fingerprint();
    assert!(!host_core.trusted_peers().iter().any(|p| p.allow_input));
    assert!(host_core.allows_input(fp));
    assert!(!host_core.allows_input("not-paired"));
    host_core.set_headless(false);
    assert!(!host_core.allows_input(fp));
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_pin_fails_and_nothing_is_trusted() {
    let host_core = Core::ephemeral("Host", TrustPolicy::Pair).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::Pair).unwrap();
    let (h, v) = pair_attempt(&host_core, &viewer_core, Some("000 000")).await;
    // (Probability the random PIN is 000000 is 1e-6.)
    assert_eq!(h, "not_paired");
    assert_eq!(v, "pairing_failed");
    assert!(!host_core.is_trusted(viewer_core.fingerprint()));
    assert!(!viewer_core.is_trusted(host_core.fingerprint()));
}

#[tokio::test(flavor = "multi_thread")]
async fn host_can_cancel_pairing() {
    let host_core = Core::ephemeral("Host", TrustPolicy::Pair).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::Pair).unwrap();
    let (h, v) = pair_attempt(&host_core, &viewer_core, None).await;
    assert_eq!((h.as_str(), v.as_str()), ("not_paired", "pairing_cancelled"));
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_failures_lock_pairing() {
    let host_core = Core::ephemeral("Host", TrustPolicy::Pair).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::Pair).unwrap();
    for _ in 0..MAX_PAIRING_FAILURES {
        assert_eq!(pair_attempt(&host_core, &viewer_core, Some("000000")).await.1, "pairing_failed");
    }
    // Locked: the host refuses before showing a PIN.
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core.clone(), host_cfg(), hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core.clone(), viewer_cfg(host.port()), vev, Arc::new(|_| {}));
    let Event::SessionEnded { reason, .. } = wait_for(&mut vrx, |e| matches!(e, Event::SessionEnded { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(reason, "pairing_locked");
    assert!(hrx.try_recv().map(|e| !matches!(e, Event::PairingPin { .. })).unwrap_or(true));
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_peer_must_pair_again() {
    let host_core = Core::ephemeral("Host", TrustPolicy::Pair).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::Pair).unwrap();
    assert_eq!(pair_attempt(&host_core, &viewer_core, Some("RIGHT")).await.0, "started");
    assert!(host_core.untrust(viewer_core.fingerprint()).unwrap());
    // The viewer still trusts the host, but the host no longer trusts the viewer: pairing again.
    assert_eq!(pair_attempt(&host_core, &viewer_core, Some("RIGHT")).await.0, "started");
}

#[tokio::test(flavor = "multi_thread")]
async fn pinned_fingerprint_mismatch_fails_handshake() {
    let host_core = Core::ephemeral("Host", TrustPolicy::AcceptAllInsecure).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::AcceptAllInsecure).unwrap();
    let (hev, _hrx) = channel_sink();
    let host = start_host(host_core, host_cfg(), hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let mut cfg = viewer_cfg(host.port());
    cfg.expected_fingerprint = Some("00".repeat(32));
    let viewer = connect_viewer(viewer_core, cfg, vev, Arc::new(|_| {}));
    let Event::SessionEnded { reason, .. } = wait_for(&mut vrx, |e| matches!(e, Event::SessionEnded { .. })).await
    else {
        unreachable!()
    };
    assert!(reason.to_lowercase().contains("certificate"), "{reason}");
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reject_unknown_policy_refuses_without_pairing() {
    let host_core = Core::ephemeral("Host", TrustPolicy::RejectUnknown).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::AcceptAllInsecure).unwrap();
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core, host_cfg(), hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core, viewer_cfg(host.port()), vev, Arc::new(|_| {}));
    let Event::SessionEnded { reason, .. } = wait_for(&mut vrx, |e| matches!(e, Event::SessionEnded { .. })).await
    else {
        unreachable!()
    };
    assert!(reason.contains("not_trusted"), "{reason}");
    assert!(hrx.try_recv().map(|e| !matches!(e, Event::PairingPin { .. })).unwrap_or(true));
    viewer.disconnect().await;
    host.stop().await;
}

fn viewer_core(name: &str) -> Arc<Core> {
    Core::ephemeral(name, TrustPolicy::AcceptAllInsecure).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn extra_viewer_beyond_limit_is_rejected_as_busy() {
    let (hev, mut hrx) = channel_sink();
    let host = start_host(viewer_core("Host"), host_cfg(), hev).await.unwrap();
    let mut viewers = Vec::new();
    for i in 0..MAX_SESSIONS {
        let (vev, _rx) = channel_sink();
        viewers.push(connect_viewer(viewer_core(&format!("V{i}")), viewer_cfg(host.port()), vev, Arc::new(|_| {})));
        wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    }
    assert_eq!(host.sessions().len(), MAX_SESSIONS);
    let (vev, mut extra_rx) = channel_sink();
    let extra = connect_viewer(viewer_core("Extra"), viewer_cfg(host.port()), vev, Arc::new(|_| {}));
    let Event::SessionEnded { reason, .. } = wait_for(&mut extra_rx, |e| matches!(e, Event::SessionEnded { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(reason, "busy");
    extra.disconnect().await;
    for v in viewers {
        v.disconnect().await;
    }
    host.stop().await;
}

fn frame_counter() -> (FrameSink, Arc<Mutex<Vec<Vec<u8>>>>) {
    let got: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let g = got.clone();
    (Arc::new(move |f: FrameOut<'_>| g.lock().unwrap().push(f.data.to_vec())), got)
}

async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_mirror_viewers_receive_the_same_frames() {
    let (hev, mut hrx) = channel_sink();
    let host = start_host(viewer_core("Host"), host_cfg(), hev).await.unwrap();
    let (s1, got1) = frame_counter();
    let (s2, got2) = frame_counter();
    let (e1, _r1) = channel_sink();
    let (e2, _r2) = channel_sink();
    let v1 = connect_viewer(viewer_core("A"), viewer_cfg(host.port()), e1, s1);
    wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    let v2 = connect_viewer(viewer_core("B"), viewer_cfg(host.port()), e2, s2);
    wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    assert_eq!(host.submit_frame(b"key", clock::now_us(), true), SubmitResult::Sent);
    assert_eq!(host.submit_frame(b"delta", clock::now_us(), false), SubmitResult::Sent);
    eventually(|| got1.lock().unwrap().len() == 2 && got2.lock().unwrap().len() == 2).await;
    v1.disconnect().await;
    v2.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn extend_session_gets_its_own_stream() {
    let (hev, mut hrx) = channel_sink();
    let host = start_host(viewer_core("Host"), host_cfg(), hev).await.unwrap();
    // One mirror viewer and one extend viewer.
    let (ms, mirror_got) = frame_counter();
    let (me, _mr) = channel_sink();
    let mirror = connect_viewer(viewer_core("Mirror"), viewer_cfg(host.port()), me, ms);
    wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;

    let (xs, extend_got) = frame_counter();
    let (xe, mut xrx) = channel_sink();
    let mut cfg = viewer_cfg(host.port());
    cfg.mode = Mode::Extend;
    cfg.display = DisplayInfo {
        width_px: 2880,
        height_px: 1801,
        width_mm: 300,
        height_mm: 190,
        refresh_hz: 120,
        scale_percent: 200,
        refresh_rates: vec![],
    };
    let extend = connect_viewer(viewer_core("Extend"), cfg, xe, xs);
    let Event::SessionStarted { session_id, mode, stream, viewer_display, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(mode, Mode::Extend);
    assert_eq!((stream.width, stream.height, stream.fps), (2880, 1800, 120), "native size (even) and refresh rate");
    assert_eq!(viewer_display.unwrap().scale_percent, 200);
    let Event::StreamConfig { stream: vs } = wait_for(&mut xrx, |e| matches!(e, Event::StreamConfig { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(vs, stream);

    // Per-session frames go only to the extend viewer; mirror frames only to the mirror viewer.
    assert_eq!(host.submit_session_frame(session_id, b"ext-key", clock::now_us(), true), SubmitResult::Sent);
    assert_eq!(host.submit_frame(b"mirror-key", clock::now_us(), true), SubmitResult::Sent);
    eventually(|| extend_got.lock().unwrap().len() == 1 && mirror_got.lock().unwrap().len() == 1).await;
    assert_eq!(extend_got.lock().unwrap()[0], b"ext-key");
    assert_eq!(mirror_got.lock().unwrap()[0], b"mirror-key");

    // The user picks another resolution for the virtual display.
    let new = StreamConfig { width: 1920, height: 1200, ..stream };
    assert!(host.set_session_stream(session_id, new.clone()));
    let Event::StreamConfig { stream: vs } = wait_for(&mut xrx, |e| matches!(e, Event::StreamConfig { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(vs, new);

    // The host ends the extend session (e.g. virtual display removed).
    host.end_session(session_id);
    let Event::SessionEnded { reason, .. } = wait_for(&mut xrx, |e| matches!(e, Event::SessionEnded { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(reason, "host_ended_session");
    let Event::SessionEnded { session_id: ended, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::SessionEnded { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(ended, session_id);
    assert_eq!(host.sessions().len(), 1);
    extend.disconnect().await;
    mirror.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn extend_is_refused_by_mirror_only_hosts() {
    let (hev, _hrx) = channel_sink();
    let host = start_host(viewer_core("Host"), HostConfig { extend: false, ..host_cfg() }, hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let mut cfg = viewer_cfg(host.port());
    cfg.mode = Mode::Extend;
    let v = connect_viewer(viewer_core("V"), cfg, vev, Arc::new(|_| {}));
    let Event::SessionEnded { reason, .. } = wait_for(&mut vrx, |e| matches!(e, Event::SessionEnded { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(reason, "extend_not_supported");
    v.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn host_stop_ends_viewer_session() {
    let host_core = Core::ephemeral("Host", TrustPolicy::AcceptAllInsecure).unwrap();
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core, host_cfg(), hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let viewer = connect_viewer(
        Core::ephemeral("V", TrustPolicy::AcceptAllInsecure).unwrap(),
        viewer_cfg(host.port()),
        vev,
        Arc::new(|_| {}),
    );
    wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    host.stop().await;
    let Event::SessionEnded { reason, .. } = wait_for(&mut vrx, |e| matches!(e, Event::SessionEnded { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(reason, "host_stopped");
    viewer.disconnect().await;
}

/// A man-in-the-middle TCP proxy that misbehaves on the host→viewer direction after the
/// handshake: either flips one byte inside a TLS record, or replays a whole TLS record.
#[derive(Clone, Copy)]
enum Attack {
    FlipByte,
    ReplayRecord,
}

async fn spawn_mitm(target_port: u16, attack: Attack) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (client, _) = listener.accept().await.unwrap();
        let server = tokio::net::TcpStream::connect(("127.0.0.1", target_port)).await.unwrap();
        let (mut cr, mut cw) = client.into_split();
        let (mut sr, mut sw) = server.into_split();
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut cr, &mut sw).await;
        });
        // Parse TLS records from the server; tamper with the 12th application-data record.
        let mut n_app = 0;
        loop {
            let mut h = [0u8; 5];
            if sr.read_exact(&mut h).await.is_err() {
                break;
            }
            let len = u16::from_be_bytes([h[3], h[4]]) as usize;
            let mut body = vec![0u8; len];
            if sr.read_exact(&mut body).await.is_err() {
                break;
            }
            let mut out = h.to_vec();
            if h[0] == 23 {
                n_app += 1;
            }
            let target = h[0] == 23 && n_app == 12;
            match attack {
                Attack::FlipByte if target => {
                    body[len / 2] ^= 0x01;
                    out.extend_from_slice(&body);
                }
                Attack::ReplayRecord if target => {
                    out.extend_from_slice(&body);
                    out.extend_from_slice(&h);
                    out.extend_from_slice(&body);
                }
                _ => out.extend_from_slice(&body),
            }
            if cw.write_all(&out).await.is_err() {
                break;
            }
        }
    });
    port
}

async fn run_attack(attack: Attack) -> String {
    let host_core = Core::ephemeral("Host", TrustPolicy::AcceptAllInsecure).unwrap();
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core, host_cfg(), hev).await.unwrap();
    let proxy = spawn_mitm(host.port(), attack).await;
    let (vev, mut vrx) = channel_sink();
    let viewer = connect_viewer(
        Core::ephemeral("V", TrustPolicy::AcceptAllInsecure).unwrap(),
        viewer_cfg(proxy),
        vev,
        Arc::new(|_| {}),
    );
    wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    let feeder = tokio::spawn(async move {
        for i in 0..200u32 {
            host.submit_frame(&[0u8; 2000], clock::now_us(), i == 0);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        host
    });
    let Event::SessionEnded { reason, .. } = wait_for(&mut vrx, |e| matches!(e, Event::SessionEnded { .. })).await
    else {
        unreachable!()
    };
    viewer.disconnect().await;
    feeder.await.unwrap().stop().await;
    reason
}

#[tokio::test(flavor = "multi_thread")]
async fn tampered_record_is_rejected() {
    let reason = run_attack(Attack::FlipByte).await;
    assert!(reason.to_lowercase().contains("decrypt"), "{reason}");
}

#[tokio::test(flavor = "multi_thread")]
async fn replayed_record_is_rejected() {
    let reason = run_attack(Attack::ReplayRecord).await;
    assert!(reason.to_lowercase().contains("decrypt"), "{reason}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_use_quic_with_datagram_video() {
    let (hev, mut hrx) = channel_sink();
    let host = start_host(viewer_core("Host"), host_cfg(), hev).await.unwrap();
    let (fs, got) = frame_counter();
    let (vev, mut vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core("V"), viewer_cfg(host.port()), vev, fs);
    let Event::SessionStarted { transport, link, .. } =
        wait_for(&mut vrx, |e| matches!(e, Event::SessionStarted { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(transport, TransportKind::Quic);
    assert_eq!(link, LinkKind::Network, "127.0.0.1 is not a direct (link-local) link");
    let Event::SessionStarted { transport, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(transport, TransportKind::Quic);
    // A 200 KB keyframe spans ~170 datagrams.
    let big = vec![7u8; 200_000];
    assert_eq!(host.submit_frame(&big, clock::now_us(), true), SubmitResult::Sent);
    eventually(|| got.lock().unwrap().len() == 1).await;
    assert_eq!(got.lock().unwrap()[0], big);
    viewer.disconnect().await;
    host.stop().await;
}

/// Windows reports an ICMP "port unreachable" from an earlier send as a ConnectionReset error on
/// the next receive (the viewer races several attempts). That is not fatal for a relay.
fn relay_error_is_fatal(e: &std::io::Error) -> bool {
    !matches!(e.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionRefused)
}

/// UDP relay that drops every `drop_every`-th host→viewer packet once the handshake is done.
/// A loopback UDP socket with 4 MB buffers. Each frame arrives as a burst of ~26 datagrams, and
/// Windows' default receive buffer can overflow under that, adding loss the test didn't ask for.
fn roomy_udp() -> tokio::net::UdpSocket {
    let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::UDP)).unwrap();
    s.set_recv_buffer_size(4 << 20).unwrap();
    s.set_send_buffer_size(4 << 20).unwrap();
    s.bind(&std::net::SocketAddr::from(([127, 0, 0, 1], 0)).into()).unwrap();
    s.set_nonblocking(true).unwrap();
    tokio::net::UdpSocket::from_std(s.into()).unwrap()
}

async fn lossy_udp_proxy(target: std::net::SocketAddr, drop_every: usize) -> u16 {
    let front = Arc::new(roomy_udp());
    let back = Arc::new(roomy_udp());
    back.connect(target).await.unwrap();
    let port = front.local_addr().unwrap().port();
    let client: Arc<Mutex<Option<std::net::SocketAddr>>> = Arc::default();
    {
        let (front, back, client) = (front.clone(), back.clone(), client.clone());
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            loop {
                match front.recv_from(&mut buf).await {
                    Ok((n, from)) => {
                        *client.lock().unwrap() = Some(from);
                        let _ = back.send(&buf[..n]).await;
                    }
                    Err(e) if !relay_error_is_fatal(&e) => continue,
                    Err(_) => break,
                }
            }
        });
    }
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        let mut count = 0usize;
        loop {
            let n = match back.recv(&mut buf).await {
                Ok(n) => n,
                Err(e) if !relay_error_is_fatal(&e) => continue,
                Err(_) => break,
            };
            count += 1;
            if count > 50 && count.is_multiple_of(drop_every) {
                continue;
            }
            let to = *client.lock().unwrap();
            if let Some(to) = to {
                let _ = front.send_to(&buf[..n], to).await;
            }
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn fec_carries_frames_through_packet_loss() {
    let (hev, mut hrx) = channel_sink();
    let host = start_host(viewer_core("Host"), host_cfg(), hev).await.unwrap();
    let proxy = lossy_udp_proxy(format!("127.0.0.1:{}", host.port()).parse().unwrap(), 20).await;
    let (fs, got) = frame_counter();
    let (vev, _vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core("V"), viewer_cfg(proxy), vev, fs);
    let Event::SessionStarted { transport, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(transport, TransportKind::Quic);
    let frame = vec![3u8; 30_000];
    let mut sent = 0;
    for i in 0..100 {
        // Keyframe every 10 frames so a lost frame doesn't stall delivery for long.
        if host.submit_frame(&frame, clock::now_us(), i % 10 == 0) == SubmitResult::Sent {
            sent += 1;
        }
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let delivered = got.lock().unwrap().len();
    // Pacing may skip a few frames on a busy machine; the test is about what loss does to the rest.
    assert!(sent >= 60, "only {sent} frames were sent");
    // Without FEC, 5 % loss over ~26 datagrams per frame would deliver about a quarter of them. Half
    // keeps a wide margin over that while leaving room for a busy CI machine (once 67 % on Windows).
    assert!(delivered * 100 >= sent * 50, "only {delivered} of {sent} frames survived 5 % loss");
    assert!(got.lock().unwrap().iter().all(|f| *f == frame));
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn heavy_loss_triggers_a_lower_bitrate_hint() {
    let (hev, mut hrx) = channel_sink();
    let host = start_host(viewer_core("Host"), host_cfg(), hev).await.unwrap();
    // Every 6th packet lost (~17 %): beyond what 15 % FEC rebuilds for long.
    let proxy = lossy_udp_proxy(format!("127.0.0.1:{}", host.port()).parse().unwrap(), 6).await;
    let (vev, _vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core("V"), viewer_cfg(proxy), vev, Arc::new(|_| {}));
    let Event::SessionStarted { session_id, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await
    else {
        unreachable!()
    };
    let feeder = tokio::spawn(async move {
        for i in 0..200 {
            host.submit_frame(&[1u8; 30_000], clock::now_us(), i % 30 == 0);
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
        host
    });
    let Event::BitrateHint { session_id: hinted, scale_percent } =
        wait_for(&mut hrx, |e| matches!(e, Event::BitrateHint { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(hinted, session_id);
    assert!(scale_percent < 100, "loss must lower the bitrate, got {scale_percent} %");
    viewer.disconnect().await;
    feeder.await.unwrap().stop().await;
}

/// Pairs, then connects with `input` requested. Returns the running pair plus event streams.
async fn input_session(
    allow: bool,
) -> (HostHandle, ViewerHandle, mpsc::UnboundedReceiver<Event>, mpsc::UnboundedReceiver<Event>, Arc<Core>, Arc<Core>) {
    let host_core = Core::ephemeral("Host", TrustPolicy::Pair).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::Pair).unwrap();
    assert_eq!(pair_attempt(&host_core, &viewer_core, Some("RIGHT")).await.0, "started");
    assert!(!host_core.allows_input(viewer_core.fingerprint()), "input is off by default");
    if allow {
        assert!(host_core.set_allow_input(viewer_core.fingerprint(), true).unwrap());
    }
    let (hev, hrx) = channel_sink();
    let host = start_host(host_core.clone(), host_cfg(), hev).await.unwrap();
    let (vev, vrx) = channel_sink();
    let viewer = connect_viewer(
        viewer_core.clone(),
        ViewerConfig { input: true, ..viewer_cfg(host.port()) },
        vev,
        Arc::new(|_| {}),
    );
    (host, viewer, hrx, vrx, host_core, viewer_core)
}

fn started_input(e: &Event) -> bool {
    match e {
        Event::SessionStarted { input, .. } => *input,
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn input_is_forwarded_only_while_allowed() {
    let (host, viewer, mut hrx, mut vrx, host_core, viewer_core) = input_session(true).await;
    assert!(started_input(&wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await));
    assert!(started_input(&wait_for(&mut vrx, |e| matches!(e, Event::SessionStarted { .. })).await));

    let key = InputEvent::Key { usage: 0x04, down: true };
    viewer.send_input(key);
    let Event::Input { event, .. } = wait_for(&mut hrx, |e| matches!(e, Event::Input { .. })).await else {
        unreachable!()
    };
    assert_eq!(event, key);

    // Revoking mid-session: the viewer is told (with the next receiver report) and stops sending.
    host_core.set_allow_input(viewer_core.fingerprint(), false).unwrap();
    let e = wait_for(&mut vrx, |e| matches!(e, Event::InputAllowed { .. })).await;
    assert!(matches!(e, Event::InputAllowed { allowed: false, .. }));
    viewer.send_input(InputEvent::MouseMove { x: 1, y: 2 });
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(e) = hrx.try_recv() {
        assert!(!matches!(e, Event::Input { .. }), "input after revocation: {e:?}");
    }
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn input_needs_the_host_to_allow_the_device() {
    let (host, viewer, mut hrx, mut vrx, _, _) = input_session(false).await;
    assert!(!started_input(&wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await));
    assert!(!started_input(&wait_for(&mut vrx, |e| matches!(e, Event::SessionStarted { .. })).await));
    // The host is asked whether to allow it.
    let Event::InputRequested { fingerprint, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::InputRequested { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(fingerprint.len(), 64);
    viewer.send_input(InputEvent::Key { usage: 0x04, down: true });
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(e) = hrx.try_recv() {
        assert!(!matches!(e, Event::Input { .. }), "input without permission: {e:?}");
    }
    viewer.disconnect().await;
    host.stop().await;
}

/// Direct Wi-Fi negotiation works from either side: whoever can run a hotspot offers it, and
/// either side can ask or decline.
#[tokio::test(flavor = "multi_thread")]
async fn direct_link_messages_flow_both_ways() {
    let (hev, mut hrx) = channel_sink();
    let host = start_host(viewer_core("Host"), host_cfg(), hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core("V"), viewer_cfg(host.port()), vev, Arc::new(|_| {}));
    let Event::SessionStarted { session_id: id, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await
    else {
        unreachable!()
    };
    wait_for(&mut vrx, |e| matches!(e, Event::SessionStarted { .. })).await;

    // Viewer asks; host declines.
    viewer.request_direct_link();
    wait_for(&mut hrx, |e| matches!(e, Event::DirectLinkRequested { session_id } if *session_id == id)).await;
    assert!(host.refuse_direct_link(id, "no_hotspot"));
    wait_for(&mut vrx, |e| matches!(e, Event::DirectLinkUnavailable { reason, .. } if reason == "no_hotspot")).await;

    // Host asks; viewer declines, then offers its hotspot.
    assert!(host.request_direct_link(id));
    wait_for(&mut vrx, |e| matches!(e, Event::DirectLinkRequested { .. })).await;
    viewer.refuse_direct_link("busy");
    wait_for(&mut hrx, |e| matches!(e, Event::DirectLinkUnavailable { reason, .. } if reason == "busy")).await;
    viewer.offer_direct_link("Annex-ab12", "0123456789abcdef");
    wait_for(&mut hrx, |e| matches!(e, Event::DirectLinkOffer { ssid, .. } if ssid == "Annex-ab12")).await;

    // Host offers its own.
    assert!(host.offer_direct_link(id, "Annex-cd34", "fedcba9876543210"));
    wait_for(&mut vrx, |e| matches!(e, Event::DirectLinkOffer { ssid, .. } if ssid == "Annex-cd34")).await;

    viewer.disconnect().await;
    host.stop().await;
}

/// The host listens on IPv6 as well (discovery advertises `fe80::…` on a cable, and viewers try
/// it first), over both QUIC and the TCP fallback, and an IPv4 viewer still reads as IPv4.
#[tokio::test(flavor = "multi_thread")]
async fn host_accepts_ipv6_and_ipv4_viewers() {
    let (hev, mut hrx) = channel_sink();
    let host = start_host(viewer_core("Host"), host_cfg(), hev).await.unwrap();
    for address in [format!("[::1]:{}", host.port()), format!("127.0.0.1:{}", host.port())] {
        let (vev, mut vrx) = channel_sink();
        let viewer =
            connect_viewer(viewer_core("Viewer"), ViewerConfig { address, ..viewer_cfg(0) }, vev, Arc::new(|_| {}));
        let Event::SessionStarted { transport, .. } =
            wait_for(&mut vrx, |e| matches!(e, Event::SessionStarted { .. })).await
        else {
            unreachable!()
        };
        assert_eq!(transport, annex_session::TransportKind::Quic);
        wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
        viewer.disconnect().await;
        wait_for(&mut hrx, |e| matches!(e, Event::SessionEnded { .. })).await;
    }
    host.stop().await;
}

// ---- Clipboard and files -------------------------------------------------------------------

fn noise(len: usize) -> Vec<u8> {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

fn ended(e: &Event) -> Option<(bool, Option<String>, Option<String>)> {
    match e {
        Event::TransferEnded { ok, path, error, .. } => Some((*ok, path.clone(), error.clone())),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_and_clipboard_go_both_ways_intact() {
    let (host, viewer, mut hrx, mut vrx, host_core, viewer_core) = input_session(true).await;
    let (host_dl, viewer_dl, src) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    host_core.set_downloads(Some(host_dl.path().to_path_buf()));
    viewer_core.set_downloads(Some(viewer_dl.path().to_path_buf()));
    let Event::SessionStarted { session_id, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await
    else {
        unreachable!()
    };
    wait_for(&mut vrx, |e| matches!(e, Event::SessionStarted { .. })).await;

    // Viewer → host: 3 MB, byte for byte, under its own name, nothing else left behind.
    let data = noise(3 << 20);
    let file = src.path().join("report.bin");
    std::fs::write(&file, &data).unwrap();
    viewer.send_file(&file).unwrap();
    let (ok, path, error) = ended(&wait_for(&mut hrx, |e| ended(e).is_some()).await).unwrap();
    assert!(ok, "{error:?}");
    assert_eq!(std::fs::read(path.unwrap()).unwrap(), data);
    assert_eq!(files_in(host_dl.path()), ["report.bin"]);
    assert!(ended(&wait_for(&mut vrx, |e| ended(e).is_some()).await).unwrap().0, "the sender sees it arrive");

    // Host → viewer, and a name that's taken gets a number.
    std::fs::write(viewer_dl.path().join("report.bin"), b"old").unwrap();
    host.send_file(session_id, &file).unwrap();
    let (ok, path, _) =
        ended(&wait_for(&mut vrx, |e| matches!(e, Event::TransferEnded { incoming: true, .. })).await).unwrap();
    assert!(ok);
    assert!(path.unwrap().ends_with("report (2).bin"));

    // Clipboard, both ways. The apps call from their own threads, outside the runtime.
    let sent = std::thread::scope(|s| {
        s.spawn(|| viewer.send_clipboard("text/plain;charset=utf-8", "héllo".as_bytes().to_vec())).join().unwrap()
    });
    assert!(sent);
    let e = wait_for(&mut hrx, |e| matches!(e, Event::ClipboardReceived { .. })).await;
    assert!(
        matches!(e, Event::ClipboardReceived { ref mime, ref data, .. } if mime == "text/plain;charset=utf-8" && data == "héllo".as_bytes())
    );
    let png = noise(200_000);
    assert!(host.send_clipboard(session_id, "image/png", png.clone()));
    let e = wait_for(&mut vrx, |e| matches!(e, Event::ClipboardReceived { .. })).await;
    assert!(matches!(e, Event::ClipboardReceived { ref data, .. } if *data == png));
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_without_control_cant_send_files_or_clipboard() {
    let (host, viewer, mut hrx, mut vrx, host_core, _) = input_session(false).await;
    let (host_dl, src) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    host_core.set_downloads(Some(host_dl.path().to_path_buf()));
    wait_for(&mut vrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    let file = src.path().join("x.bin");
    std::fs::write(&file, noise(100_000)).unwrap();
    viewer.send_file(&file).unwrap();
    viewer.send_clipboard("text/plain;charset=utf-8", b"secret".to_vec());
    let (ok, _, error) = ended(&wait_for(&mut vrx, |e| ended(e).is_some()).await).unwrap();
    assert!(!ok);
    assert!(error.unwrap().contains("doesn't take files"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(e) = hrx.try_recv() {
        assert!(!matches!(e, Event::ClipboardReceived { .. } | Event::TransferStarted { .. }), "{e:?}");
    }
    assert!(files_in(host_dl.path()).is_empty());
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_file_leaves_nothing_behind() {
    let (host, viewer, mut hrx, mut vrx, host_core, _) = input_session(true).await;
    let (host_dl, src) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    host_core.set_downloads(Some(host_dl.path().to_path_buf()));
    let Event::SessionStarted { session_id, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await
    else {
        unreachable!()
    };
    wait_for(&mut vrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    let file = src.path().join("big.bin");
    // Big enough to still be arriving when it's cancelled; one noise block repeated, since making
    // 400 MB of noise byte by byte takes seconds in a debug build.
    std::fs::write(&file, noise(1 << 20).repeat(400)).unwrap();
    viewer.send_file(&file).unwrap();
    let Event::TransferProgress { id, .. } =
        wait_for(&mut hrx, |e| matches!(e, Event::TransferProgress { incoming: true, .. })).await
    else {
        unreachable!()
    };
    host.cancel_transfer(session_id, true, id);
    let (ok, _, _) = ended(&wait_for(&mut hrx, |e| ended(e).is_some()).await).unwrap();
    assert!(!ok);
    let (ok, _, error) = ended(&wait_for(&mut vrx, |e| ended(e).is_some()).await).unwrap();
    assert!(!ok, "the sender learns it was cancelled");
    assert!(error.unwrap().contains("Cancelled"));
    assert!(files_in(host_dl.path()).is_empty(), "no partial file: {:?}", files_in(host_dl.path()));
    viewer.disconnect().await;
    host.stop().await;
}
