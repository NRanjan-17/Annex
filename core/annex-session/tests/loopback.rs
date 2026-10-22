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
    DisplayInfo { width_px: 1920, height_px: 1080, width_mm: 527, height_mm: 296, refresh_hz: 60, scale_percent: 100 }
}

fn viewer_cfg(port: u16) -> ViewerConfig {
    ViewerConfig {
        address: format!("127.0.0.1:{port}"),
        display: display(),
        codecs: vec![Codec::H264],
        expected_fingerprint: None,
        latency_csv: None,
    }
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
    tokio::time::timeout(Duration::from_secs(10), async {
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
    let host = start_host(host_core.clone(), HostConfig { port: 0, stream: stream() }, hev).await.unwrap();

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
    wait_for(&mut hrx, |e| matches!(e, Event::KeyframeRequested)).await;
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
    wait_for(&mut hrx, |e| matches!(e, Event::KeyframeRequested)).await;

    {
        let r = received.lock().unwrap();
        assert!(r[0].1, "first delivered frame must be a keyframe");
        assert_eq!(r[0].2, b"frame0");
        assert!(r.windows(2).all(|w| w[1].0 > w[0].0), "frame ids must increase");
    }

    viewer.disconnect().await;
    let Event::SessionEnded { reason } = wait_for(&mut hrx, |e| matches!(e, Event::SessionEnded { .. })).await else {
        unreachable!()
    };
    assert_eq!(reason, "user");
    assert_eq!(host.submit_frame(b"x", 0, true), SubmitResult::NoSession);
    presenter.abort();

    // Both sides remembered each other.
    assert!(host_core.is_trusted(viewer_core.fingerprint()));
    assert!(viewer_core.is_trusted(host_core.fingerprint()));
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn prompt_rejection_ends_session_and_is_not_remembered() {
    let host_core = Core::ephemeral("Host", TrustPolicy::Prompt).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::AcceptAllInsecure).unwrap();
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core.clone(), HostConfig { port: 0, stream: stream() }, hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core.clone(), viewer_cfg(host.port()), vev, Arc::new(|_| {}));

    let Event::TrustRequest { request_id, peer_name, peer_role, fingerprint, fingerprint_display } =
        wait_for(&mut hrx, |e| matches!(e, Event::TrustRequest { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(peer_name, "Viewer");
    assert_eq!(peer_role, PeerRole::Viewer);
    assert_eq!(fingerprint, viewer_core.fingerprint());
    assert_eq!(fingerprint_display.len(), 24);
    assert!(host_core.respond_trust(request_id, false));
    assert!(!host_core.respond_trust(request_id, false), "request ids are single-use");

    let Event::SessionEnded { reason } = wait_for(&mut vrx, |e| matches!(e, Event::SessionEnded { .. })).await else {
        unreachable!()
    };
    assert!(reason.contains("not_trusted"), "{reason}");
    assert!(!host_core.is_trusted(viewer_core.fingerprint()));
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn prompt_acceptance_then_revocation() {
    let host_core = Core::ephemeral("Host", TrustPolicy::Prompt).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::AcceptAllInsecure).unwrap();
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core.clone(), HostConfig { port: 0, stream: stream() }, hev).await.unwrap();

    let (vev, _vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core.clone(), viewer_cfg(host.port()), vev, Arc::new(|_| {}));
    let Event::TrustRequest { request_id, .. } = wait_for(&mut hrx, |e| matches!(e, Event::TrustRequest { .. })).await
    else {
        unreachable!()
    };
    host_core.respond_trust(request_id, true);
    wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    viewer.disconnect().await;
    wait_for(&mut hrx, |e| matches!(e, Event::SessionEnded { .. })).await;

    // Trusted now: reconnect without a prompt.
    let (vev, _vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core.clone(), viewer_cfg(host.port()), vev, Arc::new(|_| {}));
    let e = wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. } | Event::TrustRequest { .. })).await;
    assert!(matches!(e, Event::SessionStarted { .. }), "trusted peer must not be prompted");
    viewer.disconnect().await;
    wait_for(&mut hrx, |e| matches!(e, Event::SessionEnded { .. })).await;

    // Revoke: the next connection prompts again.
    assert!(host_core.untrust(viewer_core.fingerprint()).unwrap());
    let (vev, _vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core.clone(), viewer_cfg(host.port()), vev, Arc::new(|_| {}));
    let Event::TrustRequest { request_id, .. } = wait_for(&mut hrx, |e| matches!(e, Event::TrustRequest { .. })).await
    else {
        unreachable!()
    };
    host_core.respond_trust(request_id, false);
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pinned_fingerprint_mismatch_fails_handshake() {
    let host_core = Core::ephemeral("Host", TrustPolicy::AcceptAllInsecure).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::AcceptAllInsecure).unwrap();
    let (hev, _hrx) = channel_sink();
    let host = start_host(host_core, HostConfig { port: 0, stream: stream() }, hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let mut cfg = viewer_cfg(host.port());
    cfg.expected_fingerprint = Some("00".repeat(32));
    let viewer = connect_viewer(viewer_core, cfg, vev, Arc::new(|_| {}));
    let Event::SessionEnded { reason } = wait_for(&mut vrx, |e| matches!(e, Event::SessionEnded { .. })).await else {
        unreachable!()
    };
    assert!(reason.to_lowercase().contains("certificate"), "{reason}");
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reject_unknown_policy_refuses_without_prompt() {
    let host_core = Core::ephemeral("Host", TrustPolicy::RejectUnknown).unwrap();
    let viewer_core = Core::ephemeral("Viewer", TrustPolicy::AcceptAllInsecure).unwrap();
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core, HostConfig { port: 0, stream: stream() }, hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let viewer = connect_viewer(viewer_core, viewer_cfg(host.port()), vev, Arc::new(|_| {}));
    let Event::SessionEnded { reason } = wait_for(&mut vrx, |e| matches!(e, Event::SessionEnded { .. })).await else {
        unreachable!()
    };
    assert!(reason.contains("not_trusted"), "{reason}");
    assert!(hrx.try_recv().map(|e| !matches!(e, Event::TrustRequest { .. })).unwrap_or(true));
    viewer.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn second_viewer_is_rejected_as_busy() {
    let host_core = Core::ephemeral("Host", TrustPolicy::AcceptAllInsecure).unwrap();
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core, HostConfig { port: 0, stream: stream() }, hev).await.unwrap();
    let (vev, _v1rx) = channel_sink();
    let v1 = connect_viewer(
        Core::ephemeral("V1", TrustPolicy::AcceptAllInsecure).unwrap(),
        viewer_cfg(host.port()),
        vev,
        Arc::new(|_| {}),
    );
    wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    let (vev, mut v2rx) = channel_sink();
    let v2 = connect_viewer(
        Core::ephemeral("V2", TrustPolicy::AcceptAllInsecure).unwrap(),
        viewer_cfg(host.port()),
        vev,
        Arc::new(|_| {}),
    );
    let Event::SessionEnded { reason } = wait_for(&mut v2rx, |e| matches!(e, Event::SessionEnded { .. })).await else {
        unreachable!()
    };
    assert!(reason.contains("busy"), "{reason}");
    v2.disconnect().await;
    v1.disconnect().await;
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn host_stop_ends_viewer_session() {
    let host_core = Core::ephemeral("Host", TrustPolicy::AcceptAllInsecure).unwrap();
    let (hev, mut hrx) = channel_sink();
    let host = start_host(host_core, HostConfig { port: 0, stream: stream() }, hev).await.unwrap();
    let (vev, mut vrx) = channel_sink();
    let viewer = connect_viewer(
        Core::ephemeral("V", TrustPolicy::AcceptAllInsecure).unwrap(),
        viewer_cfg(host.port()),
        vev,
        Arc::new(|_| {}),
    );
    wait_for(&mut hrx, |e| matches!(e, Event::SessionStarted { .. })).await;
    host.stop().await;
    let Event::SessionEnded { reason } = wait_for(&mut vrx, |e| matches!(e, Event::SessionEnded { .. })).await else {
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
    let host = start_host(host_core, HostConfig { port: 0, stream: stream() }, hev).await.unwrap();
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
    let Event::SessionEnded { reason } = wait_for(&mut vrx, |e| matches!(e, Event::SessionEnded { .. })).await else {
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
