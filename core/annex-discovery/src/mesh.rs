//! Discovery across private networks that don't carry multicast (mesh VPNs that give each
//! computer an address like 100.101.102.103), where mDNS can't see anything.
//!
//! The mesh's own client knows which computers are on it, so the browse side asks it (no
//! scanning), then asks each computer directly over UDP whether Annex is there. Every running
//! Annex answers on [`PRESENCE_PORT`] with what its mDNS record says (name, fingerprint, sharing,
//! port), but only to private, shared-address-space, unique-local and link-local addresses: the
//! same audience mDNS already reaches, now including the rest of the private network. Requests
//! are padded so an answer is never larger than the question.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// UDP port every Annex answers presence questions on, sharing or not.
pub const PRESENCE_PORT: u16 = 47790;
const ASK: &[u8] = b"ANNEX?1\n";
const HERE: &[u8] = b"ANNEX!1\n";
/// Questions are padded to this size; answers must fit in it.
const ASK_LEN: usize = 512;

/// What a presence answer says: the same fields as the mDNS TXT record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Presence {
    pub v: u16,
    pub id: String,
    pub name: String,
    pub sharing: bool,
    pub extend: bool,
    pub port: u16,
}

/// The question, padded to [`ASK_LEN`].
pub fn ask() -> Vec<u8> {
    let mut q = ASK.to_vec();
    q.resize(ASK_LEN, 0);
    q
}

/// The answer to `question` from `from`, if it gets one: only well-formed, full-size questions
/// from private addresses, and only answers that fit in the question.
pub fn answer(question: &[u8], from: IpAddr, me: &Presence) -> Option<Vec<u8>> {
    if question.len() < ASK_LEN || !question.starts_with(ASK) || !is_private_network(from) {
        return None;
    }
    let mut a = HERE.to_vec();
    a.extend(serde_json::to_vec(me).ok()?);
    (a.len() <= question.len()).then_some(a)
}

/// Reads an answer.
pub fn parse_answer(data: &[u8]) -> Option<Presence> {
    let p: Presence = serde_json::from_slice(data.strip_prefix(HERE)?).ok()?;
    let id_ok = p.id.len() == 64 && p.id.bytes().all(|b| b.is_ascii_hexdigit());
    (id_ok && p.port != 0).then(|| Presence {
        id: p.id.to_ascii_lowercase(),
        name: p.name.chars().take(64).collect(),
        ..p
    })
}

/// Private (10/8, 172.16/12, 192.168/16), shared address space (100.64/10, which mesh VPNs use),
/// link-local, loopback; IPv6 unique-local (fc00::/7), link-local and loopback.
pub fn is_private_network(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private() || v4.is_link_local() || v4.is_loopback() || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => v6.is_unique_local() || v6.is_unicast_link_local() || v6.is_loopback(),
    }
}

/// Answers presence questions on a background thread while it lives. `me` is what it says;
/// None (not yet set) answers nothing.
pub struct Responder {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Responder {
    pub fn start(me: Arc<Mutex<Option<Presence>>>, port: u16) -> io::Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))?;
        socket.set_read_timeout(Some(Duration::from_millis(250)))?;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new().name("annex-presence".into()).spawn(move || {
            let mut buf = [0u8; ASK_LEN + 64];
            while !stop2.load(std::sync::atomic::Ordering::Acquire) {
                let Ok((n, from)) = socket.recv_from(&mut buf) else { continue };
                let reply = me.lock().unwrap().as_ref().and_then(|me| answer(&buf[..n], from.ip(), me));
                if let Some(reply) = reply {
                    let _ = socket.send_to(&reply, from);
                }
            }
        })?;
        Ok(Responder { stop, thread: Some(thread) })
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Asks each of `ips` whether Annex is there, waiting up to `wait` for answers.
pub fn probe(ips: &[IpAddr], port: u16, wait: Duration) -> Vec<(IpAddr, Presence)> {
    let Ok(socket) = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))) else { return Vec::new() };
    let q = ask();
    for ip in ips.iter().filter(|ip| ip.is_ipv4()) {
        let _ = socket.send_to(&q, SocketAddr::new(*ip, port));
    }
    let deadline = Instant::now() + wait;
    let mut out: Vec<(IpAddr, Presence)> = Vec::new();
    let mut buf = [0u8; ASK_LEN];
    while let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) {
        let _ = socket.set_read_timeout(Some(left));
        let Ok((n, from)) = socket.recv_from(&mut buf) else { break };
        if !ips.contains(&from.ip()) {
            continue;
        }
        if let Some(p) = parse_answer(&buf[..n])
            && !out.iter().any(|(ip, _)| *ip == from.ip())
        {
            out.push((from.ip(), p));
        }
    }
    out
}

/// The other computers on any private network this one is in, as their mesh clients list them:
/// online ones only, Macs and PCs only when the client says what they run.
pub fn mesh_peers() -> Vec<IpAddr> {
    let mut out = Vec::new();
    for (names, args, parse) in SOURCES {
        if !client_running(names[0]) {
            continue;
        }
        let Some(json) = names.iter().find_map(|n| run(n, args)) else { continue };
        out.extend(parse(&json));
    }
    // Testing: more addresses to ask, comma-separated (e.g. this computer's own mesh address).
    if let Ok(extra) = std::env::var("ANNEX_MESH_PEERS") {
        out.extend(extra.split(',').filter_map(|a| a.trim().parse::<IpAddr>().ok()));
    }
    out.sort();
    out.dedup();
    out
}

type Parse = fn(&str) -> Vec<IpAddr>;

/// Mesh clients with a command-line status, where they're installed, and how to read it.
const SOURCES: &[(&[&str], &[&str], Parse)] = &[
    (
        &[
            "tailscale",
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            "/usr/local/bin/tailscale",
            "/opt/homebrew/bin/tailscale",
            r"C:\Program Files\Tailscale\tailscale.exe",
        ],
        &["status", "--json"],
        parse_tailscale,
    ),
    (
        &["netbird", "/usr/local/bin/netbird", "/opt/homebrew/bin/netbird", r"C:\Program Files\Netbird\netbird.exe"],
        &["status", "--json"],
        parse_netbird,
    ),
];

/// `tailscale status --json` (also Headscale networks): `Peer.*` with `Online`, `OS`, `TailscaleIPs`.
pub fn parse_tailscale(json: &str) -> Vec<IpAddr> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Status {
        #[serde(default)]
        peer: std::collections::HashMap<String, Peer>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Peer {
        #[serde(default)]
        online: bool,
        #[serde(default, rename = "OS")]
        os: String,
        #[serde(default, rename = "TailscaleIPs")]
        ips: Vec<IpAddr>,
    }
    let Ok(s) = serde_json::from_str::<Status>(json) else { return Vec::new() };
    s.peer
        .into_values()
        .filter(|p| p.online && maybe_desktop(&p.os))
        .filter_map(|p| p.ips.into_iter().find(IpAddr::is_ipv4))
        .collect()
}

/// `netbird status --json`: `peers.details[*]` with `status` and `netbirdIp`.
pub fn parse_netbird(json: &str) -> Vec<IpAddr> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let Some(peers) = v.pointer("/peers/details").and_then(|d| d.as_array()) else { return Vec::new() };
    peers
        .iter()
        .filter(|p| p.get("status").and_then(|s| s.as_str()).is_some_and(|s| s.eq_ignore_ascii_case("connected")))
        .filter_map(|p| p.get("netbirdIp")?.as_str()?.split('/').next()?.parse().ok())
        .collect()
}

/// Annex runs on Macs, PCs and Linux computers; phones can't answer. (A Linux server without
/// Annex just doesn't reply to the probe.)
fn maybe_desktop(os: &str) -> bool {
    let os = os.to_ascii_lowercase();
    os.is_empty() || os == "macos" || os == "windows" || os == "linux"
}

/// Whether asking `client` for its status leaves the computer as it was. On a Mac, Tailscale's
/// command (`/usr/local/bin/tailscale` too) is the Tailscale app itself: run while the app isn't
/// running, it opens the app. So ask only while the app (or Homebrew's `tailscaled`) runs.
fn client_running(client: &str) -> bool {
    if !cfg!(target_os = "macos") || client != "tailscale" {
        return true;
    }
    ["Tailscale", "tailscaled"].iter().any(|name| {
        Command::new("pgrep")
            .args(["-xq", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Runs a mesh client's status command, if it's installed, for at most 3 seconds.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        io::Read::read_to_string(&mut stdout, &mut s).ok().map(|_| s)
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return if status.success() { reader.join().ok().flatten() } else { None },
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Presence {
        Presence { v: 1, id: "ab".repeat(32), name: "Dev's Mac".into(), sharing: true, extend: false, port: 47801 }
    }

    #[test]
    fn answers_only_private_full_size_questions() {
        let private: IpAddr = "100.102.43.114".parse().unwrap();
        let a = answer(&ask(), private, &me()).unwrap();
        assert_eq!(parse_answer(&a), Some(me()));
        assert!(answer(&ask()[..100], private, &me()).is_none(), "short question");
        assert!(answer(&ask(), "8.8.8.8".parse().unwrap(), &me()).is_none(), "public address");
        assert!(answer(b"hello", private, &me()).is_none());
    }

    #[test]
    fn private_networks() {
        for ip in [
            "10.0.0.1",
            "172.20.1.1",
            "192.168.1.5",
            "100.64.0.1",
            "100.127.255.1",
            "169.254.3.3",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "::ffff:100.100.1.1",
        ] {
            assert!(is_private_network(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "100.63.255.255", "100.128.0.1", "2a02:8070::5", "203.0.113.4"] {
            assert!(!is_private_network(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn reads_mesh_client_status() {
        let ts = r#"{"Self":{"TailscaleIPs":["100.102.43.114"]},"Peer":{
            "a":{"Online":true,"OS":"windows","TailscaleIPs":["100.70.1.2","fd7a:115c:a1e0::2"]},
            "b":{"Online":true,"OS":"android","TailscaleIPs":["100.70.1.3"]},
            "c":{"Online":false,"OS":"macOS","TailscaleIPs":["100.70.1.4"]},
            "d":{"Online":true,"OS":"macOS","TailscaleIPs":["100.70.1.5"]},
            "e":{"Online":true,"OS":"linux","TailscaleIPs":["100.70.1.6"]},
            "f":{"Online":true,"OS":"iOS","TailscaleIPs":["100.70.1.7"]}}}"#;
        let mut ips = parse_tailscale(ts);
        ips.sort();
        assert_eq!(
            ips,
            ["100.70.1.2".parse::<IpAddr>().unwrap(), "100.70.1.5".parse().unwrap(), "100.70.1.6".parse().unwrap()]
        );
        assert!(parse_tailscale("{}").is_empty());
        let nb = r#"{"peers":{"total":2,"details":[{"fqdn":"a","netbirdIp":"100.90.0.2","status":"Connected"},
            {"fqdn":"b","netbirdIp":"100.90.0.3/16","status":"Idle"}]}}"#;
        assert_eq!(parse_netbird(nb), ["100.90.0.2".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn probe_finds_a_responder() {
        let me = Arc::new(Mutex::new(Some(me())));
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        let _r = Responder::start(me, port).unwrap();
        let found = probe(&["127.0.0.1".parse().unwrap()], port, Duration::from_secs(1));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1.name, "Dev's Mac");
    }
}
