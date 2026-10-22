//! LAN discovery over mDNS / DNS-SD.
//!
//! Every running Annex advertises `_annex._tcp.local.` with TXT records `v` (protocol major), `id`
//! (certificate fingerprint), `name` (device name), `role`, `sharing` (`1` while it accepts
//! viewers) and `extend` (`1` if it can add an extra display), so devices see each other as soon as
//! the app is open. The fingerprint is public information (it is
//! what users compare), never a secret; pairing secrets are never put in TXT records.
//! Discovery is only a convenience: the TLS handshake and trust store decide who is genuine.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
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
pub fn peer_from_parts<'a>(
    instance: &str,
    port: u16,
    addrs: impl IntoIterator<Item = IpAddr>,
    txt: impl Fn(&str) -> Option<&'a str>,
) -> PeerInfo {
    let mut ips: Vec<IpAddr> = addrs.into_iter().collect();
    ips.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    ips.dedup();
    let fingerprint = txt("id")
        .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|s| s.to_ascii_lowercase());
    let name =
        txt("name").map(str::to_string).unwrap_or_else(|| instance.split("._").next().unwrap_or(instance).to_string());
    PeerInfo {
        instance: instance.to_string(),
        name,
        addresses: ips.iter().map(|ip| ip.to_string()).collect(),
        port,
        fingerprint,
        proto_major: txt("v").and_then(|v| v.parse().ok()),
        sharing: txt("sharing") != Some("0"),
        extend: txt("extend").map(|v| v == "1"),
    }
}

/// Owns the mDNS daemon. Dropping it stops advertising and browsing.
pub struct Discovery {
    daemon: ServiceDaemon,
    own_fingerprint: String,
}

/// Keeps an advertisement alive; unregisters on drop.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
    }
}

/// Runs a browse loop on a background thread; stops on drop.
pub struct Browser {
    daemon: ServiceDaemon,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.daemon.stop_browse(MDNS_SERVICE_TCP);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Discovery {
    pub fn new(own_fingerprint: &str) -> Result<Self, DiscoveryError> {
        Ok(Discovery { daemon: ServiceDaemon::new()?, own_fingerprint: own_fingerprint.to_string() })
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
        let info = ServiceInfo::new(MDNS_SERVICE_TCP, &label, &host, "", port, &props[..])?.enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        self.daemon.register(info)?;
        Ok(Advertisement { daemon: self.daemon.clone(), fullname })
    }

    /// Browses for hosts, calling `on_event` from a background thread. Our own
    /// advertisement is filtered out.
    pub fn browse(&self, on_event: impl Fn(DiscoveryEvent) + Send + 'static) -> Result<Browser, DiscoveryError> {
        let rx = self.daemon.browse(MDNS_SERVICE_TCP)?;
        let stop = Arc::new(AtomicBool::new(false));
        let own = self.own_fingerprint.clone();
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name("annex-mdns-browse".into())
            .spawn(move || {
                while !stop2.load(Ordering::Acquire) {
                    let Ok(ev) = rx.recv_timeout(Duration::from_millis(250)) else { continue };
                    match ev {
                        ServiceEvent::ServiceResolved(svc) => {
                            let peer = peer_from_parts(
                                &svc.fullname,
                                svc.port,
                                svc.addresses.iter().map(|a| a.to_ip_addr()),
                                |k| svc.txt_properties.get_property_val_str(k),
                            );
                            if peer.fingerprint.as_deref() == Some(own.as_str()) {
                                continue;
                            }
                            on_event(DiscoveryEvent::PeerFound(peer));
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            on_event(DiscoveryEvent::PeerLost { instance: fullname });
                        }
                        _ => {}
                    }
                }
            })
            .expect("spawn browse thread");
        Ok(Browser { daemon: self.daemon.clone(), stop, thread: Some(thread) })
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ["fe80::1".parse().unwrap(), "192.168.1.5".parse().unwrap(), "192.168.1.5".parse().unwrap()],
            txt,
        );
        assert_eq!(p.addresses, vec!["192.168.1.5", "fe80::1"]);
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
                return;
            }
        }
        panic!("advertisement not discovered");
    }
}
