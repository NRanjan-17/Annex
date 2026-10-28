//! LAN discovery over mDNS / DNS-SD.
//!
//! Every running Annex advertises `_annex._tcp.local.` with TXT records `v` (protocol major), `id`
//! (certificate fingerprint), `name` (device name), `role`, `sharing` (`1` while it accepts
//! viewers) and `extend` (`1` if it can add an extra display), so devices see each other as soon as
//! the app is open. The fingerprint is public information (it is
//! what users compare), never a secret; pairing secrets are never put in TXT records.
//! Discovery is only a convenience: the TLS handshake and trust store decide who is genuine.
//!
//! Private networks that don't carry multicast (mesh VPNs) are covered by [`mesh`]: the same
//! fields, asked for directly.

pub mod mesh;

use std::collections::HashMap;
use std::sync::Mutex;

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mdns_sd::{IfKind, ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;

pub use annex_proto::MDNS_SERVICE_TCP;

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("mdns: {0}")]
    Mdns(#[from] mdns_sd::Error),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PeerInfo {
    /// mDNS full instance name; stable key for `peer_lost`.
    pub instance: String,
    pub name: String,
    /// IPv4 addresses first (directly connectable), then IPv6.
    pub addresses: Vec<String>,
    pub port: u16,
    pub fingerprint: Option<String>,
    pub proto_major: Option<u16>,
    /// Accepting viewers now. Peers that don't say (older versions) only advertised while sharing.
    pub sharing: bool,
    /// Can add an extra display for a viewer (Extend). `None` if the peer doesn't say.
    pub extend: Option<bool>,
    /// Found on a private network (mesh VPN) rather than the local one.
    pub mesh: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DiscoveryEvent {
    PeerFound(PeerInfo),
    PeerLost { instance: String },
}

/// Instance label: the device name plus a short fingerprint suffix so two machines with the
/// same name don't collide.
pub fn instance_label(device_name: &str, fingerprint: &str) -> String {
    let clean: String = device_name.chars().filter(|c| !c.is_control() && *c != '.').take(40).collect();
    format!("{} ({})", clean.trim(), &fingerprint[..fingerprint.len().min(4)])
}

/// Builds a [`PeerInfo`] from resolved record data. Pure, for testing.
///
/// `addrs` pairs each address with the index of the interface it was seen on (0 if unknown).
/// IPv6 link-local addresses keep it as a numeric zone (`fe80::1%4`): without one they can't be
/// connected to.
pub fn peer_from_parts<'a>(
    instance: &str,
    port: u16,
    addrs: impl IntoIterator<Item = (IpAddr, u32)>,
    txt: impl Fn(&str) -> Option<&'a str>,
) -> PeerInfo {
    let mut ips: Vec<(IpAddr, u32)> = addrs
        .into_iter()
        .map(|(ip, scope)| match ip {
            IpAddr::V6(v6) if v6.is_unicast_link_local() => (ip, scope),
            _ => (ip, 0),
        })
        .collect();
    ips.sort_by_key(|(ip, scope)| (ip.is_ipv6(), *ip, *scope));
    ips.dedup();
    let fingerprint = txt("id")
        .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|s| s.to_ascii_lowercase());
    let name =
        txt("name").map(str::to_string).unwrap_or_else(|| instance.split("._").next().unwrap_or(instance).to_string());
    PeerInfo {
        instance: instance.to_string(),
        name,
        addresses: ips
            .iter()
            .map(|(ip, scope)| if *scope == 0 { ip.to_string() } else { format!("{ip}%{scope}") })
            .collect(),
        port,
        fingerprint,
        proto_major: txt("v").and_then(|v| v.parse().ok()),
        sharing: txt("sharing") != Some("0"),
        extend: txt("extend").map(|v| v == "1"),
        mesh: false,
    }
}

/// Owns the mDNS daemon. Dropping it stops advertising and browsing.
pub struct Discovery {
    daemon: ServiceDaemon,
    own_fingerprint: String,
    /// What this device answers presence questions with (see [`mesh`]): what it advertises.
    presence: Arc<Mutex<Option<mesh::Presence>>>,
    _responder: Option<mesh::Responder>,
}

/// Keeps an advertisement alive; unregisters on drop.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
    withdraw_on_drop: bool,
}

impl Advertisement {
    /// The mDNS full instance name.
    pub fn fullname(&self) -> &str {
        &self.fullname
    }

    /// `new` takes over from this record. Under the same name the registration was already
    /// updated in place, so nothing is withdrawn: other devices see the change, never a
    /// goodbye (which can expire the fresh record along with the old one). Under another name
    /// (the device was renamed) the old record is withdrawn.
    pub fn replaced_by(mut self, new: &Advertisement) {
        if self.fullname == new.fullname {
            self.withdraw_on_drop = false;
        }
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        if self.withdraw_on_drop {
            let _ = self.daemon.unregister(&self.fullname);
        }
    }
}

/// Runs a browse loop on a background thread; stops on drop.
pub struct Browser {
    daemon: ServiceDaemon,
    stop: Arc<AtomicBool>,
    refresh: Arc<AtomicBool>,
    mesh_refresh: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    mesh_thread: Option<JoinHandle<()>>,
}

impl Browser {
    /// Re-checks every listed device (those that don't answer are reported lost) and asks the
    /// network again, so the list matches what's there now.
    pub fn refresh(&self) {
        self.refresh.store(true, Ordering::Release);
        self.mesh_refresh.store(true, Ordering::Release);
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.daemon.stop_browse(MDNS_SERVICE_TCP);
        for t in [self.thread.take(), self.mesh_thread.take()].into_iter().flatten() {
            let _ = t.join();
        }
    }
}

impl Discovery {
    pub fn new(own_fingerprint: &str) -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()?;
        // Loopback addresses (127.0.0.1, ::1, and macOS's fe80::1 on lo0) mean nothing to another
        // computer; advertising them only made viewers waste connection attempts.
        let _ = daemon.disable_interface(vec![
            IfKind::LoopbackV4,
            IfKind::LoopbackV6,
            IfKind::Name("lo0".into()),
            IfKind::Name("lo".into()),
        ]);
        let presence = Arc::new(Mutex::new(None));
        // One per computer: a second copy of Annex (or a test) simply doesn't answer.
        let responder = mesh::Responder::start(presence.clone(), mesh::PRESENCE_PORT)
            .inspect_err(|e| tracing::info!("presence: not answering ({e})"))
            .ok();
        Ok(Discovery { daemon, own_fingerprint: own_fingerprint.to_string(), presence, _responder: responder })
    }

    /// Advertises this device as a host that is sharing, on all interfaces.
    pub fn advertise_host(&self, device_name: &str, port: u16) -> Result<Advertisement, DiscoveryError> {
        self.advertise(device_name, port, true, false)
    }

    /// Advertises this device. `sharing`: accepting viewers on `port`; otherwise it's just present
    /// (listed as "not sharing"). `extend`: can add extra displays.
    pub fn advertise(
        &self,
        device_name: &str,
        port: u16,
        sharing: bool,
        extend: bool,
    ) -> Result<Advertisement, DiscoveryError> {
        let label = instance_label(device_name, &self.own_fingerprint);
        let host = format!("annex-{}.local.", &self.own_fingerprint[..8]);
        let v = annex_proto::PROTOCOL_MAJOR.to_string();
        let flag = |b: bool| if b { "1" } else { "0" };
        let props = [
            ("v", v.as_str()),
            ("id", self.own_fingerprint.as_str()),
            ("name", device_name),
            ("role", if sharing { "host" } else { "idle" }),
            ("sharing", flag(sharing)),
            ("extend", flag(sharing && extend)),
        ];
        let mut info = ServiceInfo::new(MDNS_SERVICE_TCP, &label, &host, "", port, &props[..])?.enable_addr_auto();
        // The name already carries this device's fingerprint, so it can't clash with another
        // device. Probing would instead mistake our own previous announcement (still on the
        // network, with other TXT values) for a clash and rename us "… (2)", "… (3)", …
        info.set_requires_probe(false);
        let fullname = info.get_fullname().to_string();
        // Registering a name that is already registered updates it in place.
        self.daemon.register(info)?;
        *self.presence.lock().unwrap() = Some(mesh::Presence {
            v: annex_proto::PROTOCOL_MAJOR,
            id: self.own_fingerprint.clone(),
            name: device_name.chars().take(64).collect(),
            sharing,
            extend: sharing && extend,
            port,
        });
        Ok(Advertisement { daemon: self.daemon.clone(), fullname, withdraw_on_drop: true })
    }

    /// Browses for hosts, calling `on_event` from a background thread. Our own
    /// advertisement is filtered out.
    ///
    /// A device that stops or starts sharing withdraws its record and re-announces it under the
    /// same name straight away. The withdrawal expires the record here a second later, which can
    /// swallow the fresh announcement, so the device would vanish until the next query. The
    /// browse therefore asks again shortly after any removal, and every 30 s as a safety net.
    pub fn browse(&self, on_event: impl Fn(DiscoveryEvent) + Send + Sync + 'static) -> Result<Browser, DiscoveryError> {
        let on_event: Arc<dyn Fn(DiscoveryEvent) + Send + Sync> = Arc::new(on_event);
        let mut rx = self.daemon.browse(MDNS_SERVICE_TCP)?;
        // Fingerprints listed from the local network, by instance: those win over the mesh.
        let lan: Arc<Mutex<HashMap<String, String>>> = Arc::default();
        let mesh_refresh = Arc::new(AtomicBool::new(true));
        let daemon = self.daemon.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let refresh = Arc::new(AtomicBool::new(false));
        let own = self.own_fingerprint.clone();
        let (stop2, refresh2) = (stop.clone(), refresh.clone());
        let (lan2, on_event2) = (lan.clone(), on_event.clone());
        let thread = std::thread::Builder::new()
            .name("annex-mdns-browse".into())
            .spawn(move || {
                let mut requery: Vec<Instant> = Vec::new();
                let mut next_refresh = Instant::now() + REFRESH_EVERY;
                let mut listed: Vec<String> = Vec::new();
                while !stop2.load(Ordering::Acquire) {
                    let now = Instant::now();
                    let asked = refresh2.swap(false, Ordering::AcqRel);
                    if asked {
                        for instance in &listed {
                            let _ = daemon.verify(instance.clone(), VERIFY_TIMEOUT);
                        }
                    }
                    let due = asked || requery.iter().any(|t| *t <= now) || now >= next_refresh;
                    if due {
                        requery.retain(|t| *t > now);
                        next_refresh = now + REFRESH_EVERY;
                        // A new browse of the same type sends fresh queries and replaces the old one.
                        if let Ok(fresh) = daemon.browse(MDNS_SERVICE_TCP) {
                            rx = fresh;
                        }
                    }
                    let Ok(ev) = rx.recv_timeout(Duration::from_millis(250)) else { continue };
                    match ev {
                        ServiceEvent::ServiceResolved(svc) => {
                            let peer = peer_from_parts(
                                &svc.fullname,
                                svc.port,
                                svc.addresses.iter().map(|a| match a {
                                    ScopedIp::V6(v6) => (a.to_ip_addr(), v6.scope_id().index),
                                    _ => (a.to_ip_addr(), 0),
                                }),
                                |k| svc.txt_properties.get_property_val_str(k),
                            );
                            if peer.fingerprint.as_deref() == Some(own.as_str()) {
                                continue;
                            }
                            tracing::info!(name = %peer.name, sharing = peer.sharing, addrs = ?peer.addresses, port = peer.port, "nearby: found");
                            if !listed.contains(&peer.instance) {
                                listed.push(peer.instance.clone());
                            }
                            if let Some(fp) = &peer.fingerprint {
                                lan2.lock().unwrap().insert(peer.instance.clone(), fp.clone());
                            }
                            on_event2(DiscoveryEvent::PeerFound(peer));
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            tracing::info!(instance = %fullname, "nearby: gone");
                            listed.retain(|i| *i != fullname);
                            lan2.lock().unwrap().remove(&fullname);
                            on_event2(DiscoveryEvent::PeerLost { instance: fullname });
                            let now = Instant::now();
                            requery.extend(REQUERY_AFTER_REMOVAL.iter().map(|d| now + *d));
                        }
                        _ => {}
                    }
                }
            })
            .expect("spawn browse thread");
        let mesh_thread = {
            let (stop, refresh, own) = (stop.clone(), mesh_refresh.clone(), self.own_fingerprint.clone());
            std::thread::Builder::new()
                .name("annex-mesh-browse".into())
                .spawn(move || {
                    browse_mesh(&own, mesh::mesh_peers, mesh::PRESENCE_PORT, &stop, &refresh, &lan, &*on_event)
                })
                .expect("spawn mesh browse thread")
        };
        Ok(Browser {
            daemon: self.daemon.clone(),
            stop,
            refresh,
            mesh_refresh,
            thread: Some(thread),
            mesh_thread: Some(mesh_thread),
        })
    }
}

/// Lists computers on private networks (see [`mesh`]) every [`MESH_EVERY`], or when asked to,
/// until `stop`. A computer also found on the local network is listed from there instead.
fn browse_mesh(
    own: &str,
    peers: impl Fn() -> Vec<IpAddr>,
    port: u16,
    stop: &AtomicBool,
    refresh: &AtomicBool,
    lan: &Mutex<HashMap<String, String>>,
    on_event: &(dyn Fn(DiscoveryEvent) + Send + Sync),
) {
    // Listed from the mesh: instance → (what was said, rounds unanswered).
    let mut listed: HashMap<String, (PeerInfo, u32)> = HashMap::new();
    let mut next = Instant::now();
    let mut last_round = None;
    while !stop.load(Ordering::Acquire) {
        if !refresh.swap(false, Ordering::AcqRel) && Instant::now() < next {
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        next = Instant::now() + MESH_EVERY;
        let ips = peers();
        let found = if ips.is_empty() { Vec::new() } else { mesh::probe(&ips, port, MESH_WAIT) };
        // Each round, so an empty Nearby can be explained from the log: who was asked, who
        // answered. At info only when that changes.
        let round = (ips.len(), found.len());
        if last_round != Some(round) {
            tracing::info!(asked = ips.len(), answered = found.len(), "private network: round");
            last_round = Some(round);
        } else {
            tracing::debug!(asked = ips.len(), answered = found.len(), "private network: round");
        }
        let on_lan: Vec<String> = lan.lock().unwrap().values().cloned().collect();
        let mut seen = Vec::new();
        for (ip, p) in found {
            if p.id == own || on_lan.contains(&p.id) {
                continue;
            }
            let peer = PeerInfo {
                instance: format!("mesh:{}", p.id),
                name: p.name,
                addresses: vec![ip.to_string()],
                port: p.port,
                fingerprint: Some(p.id),
                proto_major: Some(p.v),
                sharing: p.sharing,
                extend: Some(p.extend),
                mesh: true,
            };
            seen.push(peer.instance.clone());
            if listed.get(&peer.instance).is_none_or(|(old, _)| *old != peer) {
                tracing::info!(name = %peer.name, sharing = peer.sharing, addr = %ip, "private network: found");
                on_event(DiscoveryEvent::PeerFound(peer.clone()));
            }
            listed.insert(peer.instance.clone(), (peer, 0));
        }
        listed.retain(|instance, (peer, missed)| {
            if !seen.contains(instance) {
                *missed += 1;
            }
            let gone = on_lan.iter().any(|fp| peer.fingerprint.as_deref() == Some(fp)) || *missed >= MESH_MISSES;
            if gone {
                tracing::info!(instance = %instance, "private network: gone");
                on_event(DiscoveryEvent::PeerLost { instance: instance.clone() });
            }
            !gone
        });
    }
}

/// How often computers on private networks are re-checked, how long they have to answer, and
/// after how many unanswered rounds one is reported gone.
const MESH_EVERY: Duration = Duration::from_secs(15);
const MESH_WAIT: Duration = Duration::from_millis(1500);
const MESH_MISSES: u32 = 2;

/// When to ask again after a device's record goes away (it may have just re-announced).
const REQUERY_AFTER_REMOVAL: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(6)];
/// How long a refreshed device has to answer before it's reported lost.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(3);
/// Periodic re-query so a missed announcement never hides a device for long.
const REFRESH_EVERY: Duration = Duration::from_secs(30);

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_private_network_computers_unless_on_the_local_network() {
        use std::sync::mpsc;
        let me = mesh::Presence {
            v: 1,
            id: "cd".repeat(32),
            name: "Office PC".into(),
            sharing: true,
            extend: true,
            port: 47801,
        };
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        let _r = mesh::Responder::start(Arc::new(Mutex::new(Some(me))), port).unwrap();
        let (stop, refresh) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(true)));
        let lan: Arc<Mutex<HashMap<String, String>>> = Arc::default();
        let (tx, rx) = mpsc::channel();
        let t = {
            let (stop, refresh, lan) = (stop.clone(), refresh.clone(), lan.clone());
            std::thread::spawn(move || {
                let send = move |e| tx.send(e).unwrap();
                browse_mesh(&"ab".repeat(32), || vec!["127.0.0.1".parse().unwrap()], port, &stop, &refresh, &lan, &send)
            })
        };
        let Ok(DiscoveryEvent::PeerFound(p)) = rx.recv_timeout(Duration::from_secs(5)) else { panic!("not found") };
        assert_eq!(
            (p.instance.as_str(), p.name.as_str(), p.port, p.mesh),
            (&*format!("mesh:{}", "cd".repeat(32)), "Office PC", 47801, true)
        );
        assert_eq!(p.addresses, ["127.0.0.1"]);
        // Now seen on the local network too: that listing wins.
        lan.lock().unwrap().insert("Office PC._annex._tcp.local.".into(), "cd".repeat(32));
        refresh.store(true, Ordering::Release);
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(DiscoveryEvent::PeerLost { .. })));
        stop.store(true, Ordering::Release);
        t.join().unwrap();
    }

    #[test]
    fn label_is_sanitized_and_suffixed() {
        assert_eq!(instance_label("My.Mac\n", "abcdef"), "MyMac (abcd)");
    }

    #[test]
    fn peer_parsing_orders_addresses_and_validates_id() {
        let fp = "AB".repeat(32);
        let txt = |k: &str| match k {
            "id" => Some(fp.as_str()),
            "name" => Some("Studio"),
            "v" => Some("1"),
            _ => None,
        };
        let p = peer_from_parts(
            "Studio (abab)._annex._tcp.local.",
            47800,
            [
                ("fe80::1".parse().unwrap(), 4),
                ("192.168.1.5".parse().unwrap(), 4),
                ("192.168.1.5".parse().unwrap(), 7),
                ("2001:db8::1".parse().unwrap(), 4),
                ("fe80::2".parse().unwrap(), 0),
            ],
            txt,
        );
        // Only link-local IPv6 keeps its interface; it's what makes the address usable.
        assert_eq!(p.addresses, vec!["192.168.1.5", "2001:db8::1", "fe80::1%4", "fe80::2"]);
        assert_eq!(
            "[fe80::1%4]:47800".parse::<std::net::SocketAddr>().map(|a| a.to_string()),
            Ok("[fe80::1%4]:47800".to_string()),
            "viewers parse the zone back"
        );
        assert_eq!(p.fingerprint.as_deref(), Some("ab".repeat(32).as_str()));
        assert_eq!(p.name, "Studio");
        assert_eq!(p.proto_major, Some(1));
        assert!(p.sharing, "peers without the flag only advertised while sharing");
        assert_eq!(p.extend, None);
        let idle = peer_from_parts("I._annex._tcp.local.", 47800, [], |k| match k {
            "sharing" => Some("0"),
            "extend" => Some("1"),
            _ => None,
        });
        assert!(!idle.sharing);
        assert_eq!(idle.extend, Some(true));

        let bad = peer_from_parts("X._annex._tcp.local.", 1, [], |k| (k == "id").then_some("nothex"));
        assert_eq!(bad.fingerprint, None);
        assert_eq!(bad.name, "X");
    }

    #[test]
    fn event_json_shape() {
        let e = DiscoveryEvent::PeerLost { instance: "a".into() };
        assert_eq!(serde_json::to_string(&e).unwrap(), r#"{"type":"peer_lost","instance":"a"}"#);
    }

    /// Needs working multicast on the machine; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_advertise_and_browse() {
        let host = Discovery::new(&"1".repeat(64)).unwrap();
        let _ad = host.advertise_host("Live Test", 47999).unwrap();
        let viewer = Discovery::new(&"2".repeat(64)).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _b = viewer
            .browse(move |e| {
                let _ = tx.send(e);
            })
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Ok(DiscoveryEvent::PeerFound(p)) = rx.recv_timeout(Duration::from_millis(500))
                && p.port == 47999
            {
                assert_eq!(p.name, "Live Test");
                assert!(
                    !p.addresses
                        .iter()
                        .any(|a| ["127.0.0.1", "::1", "fe80::1"].contains(&a.split('%').next().unwrap())),
                    "loopback addresses advertised: {:?}",
                    p.addresses
                );
                return;
            }
        }
        panic!("advertisement not discovered");
    }

    /// Stop/start sharing swaps the record (withdraw + re-announce under the same name); the
    /// other side must still list the device afterwards. Needs multicast; run with `--ignored`.
    #[test]
    #[ignore]
    fn live_record_swap_keeps_peer_listed() {
        let host = Discovery::new(&"3".repeat(64)).unwrap();
        let idle = host.advertise("Swap Test", 47998, false, false).unwrap();
        let viewer = Discovery::new(&"4".repeat(64)).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _b = viewer
            .browse(move |e| {
                let _ = tx.send(e);
            })
            .unwrap();
        let mut listed: Option<bool> = None; // Some(sharing) while listed
        let mut pump = |rx: &std::sync::mpsc::Receiver<DiscoveryEvent>, until: std::time::Instant| {
            while std::time::Instant::now() < until {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(DiscoveryEvent::PeerFound(p)) if p.port == 47998 => listed = Some(p.sharing),
                    Ok(DiscoveryEvent::PeerLost { instance }) if instance.starts_with("Swap Test") => listed = None,
                    _ => {}
                }
            }
            listed
        };
        let now = std::time::Instant::now;
        assert_eq!(pump(&rx, now() + Duration::from_secs(4)), Some(false), "idle record seen");
        drop(idle);
        let _sharing = host.advertise("Swap Test", 47998, true, false).unwrap();
        assert_eq!(pump(&rx, now() + Duration::from_secs(10)), Some(true), "still listed, now sharing");
    }
}
