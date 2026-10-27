//! `annex`: headless host / viewer / discovery tool for development and testing.
//!
//! ```text
//! annex identity                      print this device's name and fingerprint
//! annex peers [remove <fingerprint>]  list or revoke trusted devices
//! annex discover [--seconds N]        list hosts on the LAN
//! annex host [--port P] [--replay file.h264 | --synthetic] [--fps N] [--extend] [--no-advertise] [--internet]
//! annex viewer <address | --code K7P-4QX> [--extend] [--dump file.h264] [--expect <fingerprint>] [--seconds N]
//! ```
//! Internet connections (`--internet`, `--code`) need `--rendezvous wss://…` (or `ANNEX_RENDEZVOUS`)
//! and use `--stun host:port` (default stun.cloudflare.com:3478). If a direct connection isn't
//! possible, the host gets an address on a relay: its own with `--turn turn:host:port --turn-user U
//! --turn-pass P`, otherwise ours through the rendezvous server (time- and quality-limited).
//! `host --extend` also accepts Extend sessions and sends them the same replay at its own size (no
//! virtual display), to test a viewer's Extend path.
//! Common options: `--data-dir DIR` (default: a separate "Annex CLI" data folder), `--name NAME`,
//! `--trust-all` (skip PIN pairing and accept anyone; for automated tests only).

mod h264;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use annex_session::*;

struct Opts {
    args: Vec<String>,
}

impl Opts {
    fn flag(&mut self, name: &str) -> bool {
        if let Some(i) = self.args.iter().position(|a| a == name) {
            self.args.remove(i);
            true
        } else {
            false
        }
    }

    fn value(&mut self, name: &str) -> Option<String> {
        let i = self.args.iter().position(|a| a == name)?;
        if i + 1 >= self.args.len() {
            die(&format!("{name} needs a value"));
        }
        self.args.remove(i);
        Some(self.args.remove(i))
    }

    fn num<T: std::str::FromStr>(&mut self, name: &str, default: T) -> T {
        self.value(name).map(|v| v.parse().unwrap_or_else(|_| die(&format!("bad value for {name}")))).unwrap_or(default)
    }

    fn positional(&mut self) -> Option<String> {
        if self.args.first().is_some_and(|a| !a.starts_with("--")) { Some(self.args.remove(0)) } else { None }
    }
}

fn die(msg: &str) -> ! {
    eprintln!("annex: {msg}");
    std::process::exit(2)
}

/// Never the apps' own folder: the CLI keeps its key in a file, so sharing the folder would
/// replace the app's certificate and break its TLS (KeyMismatch).
fn default_data_dir() -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| ".".into());
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/Annex CLI")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(home).join("Annex CLI")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("annex-cli")
    }
}

fn default_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().trim_end_matches(".local").to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Annex CLI".into())
}

fn open_core(o: &mut Opts) -> Arc<Core> {
    let dir = o.value("--data-dir").map(PathBuf::from).unwrap_or_else(default_data_dir);
    let name = o.value("--name").unwrap_or_else(default_name);
    let policy = if o.flag("--trust-all") { TrustPolicy::AcceptAllInsecure } else { TrustPolicy::Pair };
    // A dev tool: keep its key in a 0600 file rather than prompting for Keychain access.
    Core::open_with(&dir, &name, policy, KeyStorage::File)
        .unwrap_or_else(|e| die(&format!("cannot open data dir {}: {e}", dir.display())))
}

/// Prints events; shows the pairing PIN (host) or asks for it on stdin (viewer).
fn event_printer(core: Arc<Core>) -> EventSink {
    Arc::new(move |e: Event| match e {
        Event::PairingPin { pin, peer_name, fingerprint_display, .. } => eprintln!(
            "\n\"{peer_name}\" ({fingerprint_display}) wants to pair.\nPairing PIN: {pin}\n\
             Type this PIN on the other device. (Ctrl+C to refuse.)"
        ),
        Event::PairingPinRequired { request_id, peer_name, fingerprint_display } => {
            let core = core.clone();
            std::thread::spawn(move || {
                eprint!(
                    "\nPairing with \"{peer_name}\" ({fingerprint_display}).\nEnter the PIN it shows (empty to cancel): "
                );
                let _ = std::io::stderr().flush();
                let mut line = String::new();
                let _ = std::io::stdin().lock().read_line(&mut line);
                let pin = line.trim();
                if pin.is_empty() {
                    core.cancel_request(request_id);
                } else {
                    core.submit_pin(request_id, pin);
                }
            });
        }
        Event::LatencyStats { stats, clock_rtt_ms } => eprintln!(
            "latency: {} frames, {:.1} fps, p50 {:.1} ms, p95 {:.1} ms, p99 {:.1} ms, max {:.1} ms (clock rtt {:.2} ms)",
            stats.frames,
            stats.fps,
            stats.p50_ms,
            stats.p95_ms,
            stats.p99_ms,
            stats.max_ms,
            clock_rtt_ms.unwrap_or(f64::NAN)
        ),
        other => eprintln!("{}", serde_json::to_string(&other).unwrap_or_default()),
    })
}

#[tokio::main]
async fn main() {
    let mut o = Opts { args: std::env::args().skip(1).collect() };
    let mut cmd = o.positional().unwrap_or_else(|| "help".into());
    // `annex host --help` shows help instead of starting a host.
    if o.flag("--help") || o.flag("-h") {
        cmd = "help".into();
    }
    match cmd.as_str() {
        "identity" => {
            let core = open_core(&mut o);
            println!("name:        {}", core.device_name());
            println!("fingerprint: {}", core.fingerprint());
            println!("display:     {}", display(core.fingerprint()));
        }
        "peers" => {
            let core = open_core(&mut o);
            if o.positional().as_deref() == Some("remove") {
                let fp = o.positional().unwrap_or_else(|| die("peers remove <fingerprint>"));
                match core.untrust(&fp) {
                    Ok(true) => println!("revoked {fp}"),
                    Ok(false) => die("not a trusted peer"),
                    Err(e) => die(&e.to_string()),
                }
            } else {
                for p in core.trusted_peers() {
                    println!("{}  {}  {}", p.fingerprint, display(&p.fingerprint), p.name);
                }
            }
        }
        "discover" => discover(&mut o).await,
        "host" => host(&mut o).await,
        "viewer" => viewer(&mut o).await,
        _ => {
            eprintln!(
                "{}",
                include_str!("main.rs")
                    .lines()
                    .take(12)
                    .skip(2)
                    .map(|l| l.trim_start_matches("//!").trim_start_matches(' '))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            std::process::exit(if cmd == "help" { 0 } else { 2 });
        }
    }
}

fn display(fp: &str) -> String {
    fp.as_bytes().chunks(4).take(5).map(|c| String::from_utf8_lossy(c).to_uppercase()).collect::<Vec<_>>().join(" ")
}

async fn discover(o: &mut Opts) {
    let core = open_core(o);
    let secs: u64 = o.num("--seconds", 5);
    let d = annex_discovery::Discovery::new(core.fingerprint()).unwrap_or_else(|e| die(&e.to_string()));
    let _b = d
        .browse(|e| println!("{}", serde_json::to_string(&e).unwrap_or_default()))
        .unwrap_or_else(|e| die(&e.to_string()));
    tokio::time::sleep(Duration::from_secs(secs)).await;
}

async fn host(o: &mut Opts) {
    let core = open_core(o);
    let port: u16 = o.num("--port", annex_proto_port());
    let fps: u32 = o.num("--fps", 60);
    let replay = o.value("--replay").map(PathBuf::from);
    let synthetic = o.flag("--synthetic");
    let extend = o.flag("--extend");
    let advertise = !o.flag("--no-advertise");
    let internet = o.flag("--internet").then(|| internet_opts(o));
    let turn = o.value("--turn").map(|url| {
        let user = o.value("--turn-user").unwrap_or_default();
        let pass = o.value("--turn-pass").unwrap_or_default();
        (url, user, pass)
    });
    let (aus, codec, w, h) = match &replay {
        Some(p) => {
            let data = std::fs::read(p).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())));
            let aus = h264::split_access_units(&data);
            if aus.is_empty() {
                die("no H.264 access units in replay file");
            }
            let w: u32 = o.num("--width", 1920);
            let h: u32 = o.num("--height", 1080);
            (aus, Codec::H264, w, h)
        }
        None if synthetic => (Vec::new(), Codec::Raw, 1920, 1080),
        None => die("host needs --replay <file.h264> or --synthetic (live capture is in the desktop apps)"),
    };
    let events = event_printer(core.clone());
    let handle = start_host(
        core.clone(),
        HostConfig { port, stream: StreamConfig { codec, width: w, height: h, fps }, extend, udp: None },
        events,
    )
    .await
    .unwrap_or_else(|e| die(&e.to_string()));
    let _ad = if advertise {
        let d = annex_discovery::Discovery::new(core.fingerprint()).unwrap_or_else(|e| die(&e.to_string()));
        let ad = d.advertise_host(&core.device_name(), handle.port()).unwrap_or_else(|e| die(&e.to_string()));
        Some((d, ad))
    } else {
        None
    };
    eprintln!(
        "hosting as \"{}\" on port {} — fingerprint {}",
        core.device_name(),
        handle.port(),
        display(core.fingerprint())
    );

    let handle = Arc::new(handle);
    if let Some((server, stun)) = internet {
        tokio::spawn(share_over_internet(handle.clone(), core.device_name(), server, stun, turn));
    }

    let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / fps as f64));
    let mut i = 0usize;
    let mut synthetic_buf = vec![0u8; 30_000];
    // Extend sessions start at the viewer's size; the replay has its own.
    let mut sized = std::collections::HashSet::new();
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = tokio::signal::ctrl_c() => break,
        }
        if !handle.has_session() {
            i = 0;
            continue;
        }
        let ts = clock::now_us();
        let (frame, key): (&[u8], bool) = if aus.is_empty() {
            synthetic_buf[..8].copy_from_slice(&(i as u64).to_be_bytes());
            (&synthetic_buf, i.is_multiple_of(120))
        } else {
            let au = &aus[i % aus.len()];
            (au, h264::is_keyframe(au))
        };
        handle.submit_frame(frame, ts, key);
        let sessions = handle.sessions();
        sized.retain(|id| sessions.iter().any(|(s, _)| s == id));
        for (id, _) in sessions.into_iter().filter(|(_, mode)| *mode == Mode::Extend) {
            if sized.insert(id) {
                handle.set_session_stream(id, StreamConfig { codec, width: w, height: h, fps });
            }
            handle.submit_session_frame(id, frame, ts, key);
        }
        i += 1;
    }
    handle.stop().await;
}

fn annex_proto_port() -> u16 {
    47800
}

/// `--rendezvous` (or `ANNEX_RENDEZVOUS`) and `--stun`.
fn internet_opts(o: &mut Opts) -> (String, String) {
    let server = o
        .value("--rendezvous")
        .or_else(|| std::env::var("ANNEX_RENDEZVOUS").ok())
        .unwrap_or_else(|| die("internet connections need --rendezvous wss://… (or ANNEX_RENDEZVOUS)"));
    (server, o.value("--stun").unwrap_or_else(|| DEFAULT_STUN.into()))
}

/// The STUN server's IPv4 address: what a home router's mapping is about.
/// Shares over the internet, one code after another, printing what happens.
async fn share_over_internet(
    handle: Arc<HostHandle>,
    name: String,
    server: String,
    stun: String,
    turn: Option<(String, String, String)>,
) {
    let servers = InternetServers { rendezvous: server, stun };
    let own_relay = turn.map(|(url, username, password)| OwnRelay { url, username, password });
    let events: EventSink = Arc::new(|e: Event| match e {
        Event::InternetCode { code, .. } => eprintln!("internet code: {code}"),
        Event::InternetJoined { peer_name } => eprintln!("\"{peer_name}\" is connecting"),
        Event::InternetRelay { relayed: true, limited } => {
            eprintln!("relaying ({})", if limited { "our relay, limited" } else { "your relay" })
        }
        Event::InternetRelay { relayed: false, .. } => {
            eprintln!("no relay available (ours is off or used up for today)")
        }
        Event::InternetCodeEnded { reason } => eprintln!("code ended: {reason}"),
        _ => {}
    });
    loop {
        share_code(handle.clone(), name.clone(), servers.clone(), own_relay.clone(), events.clone()).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn viewer(o: &mut Opts) {
    let core = open_core(o);
    let code = o.value("--code");
    let join = code.map(|code| {
        let (rendezvous, stun) = internet_opts(o);
        JoinCode { code, servers: InternetServers { rendezvous, stun } }
    });
    let address = match &join {
        Some(_) => String::new(),
        None => o.positional().unwrap_or_else(|| die("viewer <address> or viewer --code <code>")),
    };
    let dump = o.value("--dump").map(PathBuf::from);
    let expected = o.value("--expect");
    let secs: u64 = o.num("--seconds", 0);
    let latency_csv = o.value("--latency-csv").map(PathBuf::from);
    let mode = if o.flag("--extend") { Mode::Extend } else { Mode::Mirror };
    let file = dump.map(|p| {
        Arc::new(Mutex::new(std::fs::File::create(&p).unwrap_or_else(|e| die(&format!("{}: {e}", p.display())))))
    });
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let printer = event_printer(core.clone());
    let events: EventSink = Arc::new(move |e: Event| {
        let end = matches!(e, Event::SessionEnded { .. });
        printer(e);
        if end {
            let _ = done_tx.send(());
        }
    });
    let (fid_tx, mut fid_rx) = tokio::sync::mpsc::unbounded_channel::<u32>();
    let frames: FrameSink = Arc::new(move |f: FrameOut<'_>| {
        if let Some(file) = &file {
            let _ = file.lock().unwrap().write_all(f.data);
        }
        let _ = fid_tx.send(f.frame_id);
    });
    let cfg = ViewerConfig {
        address,
        display: DisplayInfo {
            width_px: 1920,
            height_px: 1080,
            width_mm: 0,
            height_mm: 0,
            refresh_hz: 60,
            scale_percent: 100,
            refresh_rates: vec![],
        },
        codecs: vec![Codec::H264, Codec::Raw],
        expected_fingerprint: expected,
        mode,
        input: false,
        quality: annex_session::Quality::Best,
        latency_csv,
        internet: None,
        join,
    };
    let v = Arc::new(connect_viewer(core, cfg, events, frames));
    let presenter = {
        let v = v.clone();
        tokio::spawn(async move {
            // The CLI doesn't render; count arrival as presentation.
            while let Some(id) = fid_rx.recv().await {
                v.frame_presented(id);
            }
        })
    };
    let timer = async {
        if secs == 0 { std::future::pending::<()>().await } else { tokio::time::sleep(Duration::from_secs(secs)).await }
    };
    tokio::select! {
        _ = done_rx.recv() => {}
        _ = tokio::signal::ctrl_c() => v.disconnect().await,
        _ = timer => v.disconnect().await,
    }
    presenter.abort();
}
