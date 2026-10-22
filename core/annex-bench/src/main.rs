//! Loopback latency benchmark: a host and a viewer in one process talk over real TLS on
//! 127.0.0.1. Synthetic access units (keyframe/delta sizes like 1080p60 H.264) are stamped at
//! "capture" and timed on arrival using the clock-sync path, so the number measures the
//! transport + session pipeline (no GPU, display or codec needed; runs in CI).
//!
//! ```text
//! annex-bench [--frames 600] [--fps 60] [--key-bytes 150000] [--delta-bytes 30000]
//!             [--keyframe-interval 120] [--out bench-results] [--max-p95-ms N]
//! ```
//! Writes `<out>/loopback.json` (summary) and `<out>/loopback.csv` (per frame).

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use annex_session::stats::summarize;
use annex_session::*;
use serde::Serialize;

#[derive(Serialize)]
struct Summary {
    benchmark: &'static str,
    version: &'static str,
    os: &'static str,
    arch: &'static str,
    frames_sent: usize,
    frames_dropped: usize,
    frames_received: usize,
    fps_target: u32,
    key_bytes: usize,
    delta_bytes: usize,
    latency: Option<LatencyStats>,
}

fn arg<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> T {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(|v| v.parse().unwrap_or_else(|_| panic!("bad value for {name}")))
        .unwrap_or(default)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let frames: usize = arg(&args, "--frames", 600);
    let fps: u32 = arg(&args, "--fps", 60);
    let key_bytes: usize = arg(&args, "--key-bytes", 150_000);
    let delta_bytes: usize = arg(&args, "--delta-bytes", 30_000);
    let key_every: usize = arg(&args, "--keyframe-interval", 120);
    let out_dir = PathBuf::from(arg(&args, "--out", "bench-results".to_string()));
    let max_p95: f64 = arg(&args, "--max-p95-ms", f64::INFINITY);

    let host_core = Core::ephemeral("bench-host", TrustPolicy::AcceptAllInsecure).unwrap();
    let viewer_core = Core::ephemeral("bench-viewer", TrustPolicy::AcceptAllInsecure).unwrap();
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let host_events: EventSink = Arc::new(move |e| {
        if matches!(e, Event::SessionStarted { .. }) {
            let _ = started_tx.send(());
        }
    });
    let host = start_host(
        host_core,
        HostConfig { port: 0, stream: StreamConfig { codec: Codec::Raw, width: 1920, height: 1080, fps } },
        host_events,
    )
    .await
    .expect("start host");

    // (frame_id, capture_local_us, arrive_us)
    let rows = Arc::new(Mutex::new(Vec::<(u32, i64, i64)>::with_capacity(frames)));
    let frame_sink: FrameSink = {
        let rows = rows.clone();
        Arc::new(move |f: FrameOut<'_>| {
            let now = clock::now_us();
            if let Some(cap) = f.capture_ts_local_us {
                rows.lock().unwrap().push((f.frame_id, cap, now));
            }
        })
    };
    let viewer = connect_viewer(
        viewer_core,
        ViewerConfig {
            address: format!("127.0.0.1:{}", host.port()),
            display: DisplayInfo {
                width_px: 1920,
                height_px: 1080,
                width_mm: 0,
                height_mm: 0,
                refresh_hz: 60,
                scale_percent: 100,
            },
            codecs: vec![Codec::Raw],
            expected_fingerprint: None,
            latency_csv: None,
        },
        Arc::new(|_| {}),
        frame_sink,
    );
    tokio::time::timeout(Duration::from_secs(10), started_rx.recv()).await.expect("session start timed out");
    // Let clock sync converge (pings every 250 ms).
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let key = vec![0xA5u8; key_bytes];
    let delta = vec![0x5Au8; delta_bytes];
    let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / fps as f64));
    let (mut sent, mut dropped) = (0, 0);
    for i in 0..frames {
        tick.tick().await;
        let is_key = i % key_every == 0;
        match host.submit_frame(if is_key { &key } else { &delta }, clock::now_us(), is_key) {
            SubmitResult::Sent => sent += 1,
            _ => dropped += 1,
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    viewer.disconnect().await;
    host.stop().await;

    let rows = rows.lock().unwrap().clone();
    let lat: Vec<i64> = rows.iter().map(|(_, c, a)| a - c).collect();
    let stats = match (rows.first(), rows.last()) {
        (Some(f), Some(l)) => summarize(&lat, f.2, l.2),
        _ => None,
    };
    let summary = Summary {
        benchmark: "loopback-tls-tcp",
        version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        frames_sent: sent,
        frames_dropped: dropped,
        frames_received: rows.len(),
        fps_target: fps,
        key_bytes,
        delta_bytes,
        latency: stats.clone(),
    };
    std::fs::create_dir_all(&out_dir).expect("create output dir");
    let json = serde_json::to_string_pretty(&summary).unwrap();
    std::fs::write(out_dir.join("loopback.json"), &json).expect("write json");
    let mut csv = std::fs::File::create(out_dir.join("loopback.csv")).expect("write csv");
    writeln!(csv, "frame_id,capture_us,display_us,latency_us").unwrap();
    for (id, c, a) in &rows {
        writeln!(csv, "{id},{c},{a},{}", a - c).unwrap();
    }
    println!("{json}");

    let Some(s) = stats else {
        eprintln!("FAIL: no frames received");
        std::process::exit(1);
    };
    if rows.len() * 10 < sent * 9 {
        eprintln!("FAIL: received {} of {} frames", rows.len(), sent);
        std::process::exit(1);
    }
    if s.p95_ms > max_p95 {
        eprintln!("FAIL: p95 {:.2} ms exceeds {:.2} ms", s.p95_ms, max_p95);
        std::process::exit(1);
    }
}
