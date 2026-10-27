//! What the app knows and does, apart from drawing it: this computer's identity, the computers
//! nearby, sharing, viewing and pairing. The same behaviour and wording as the Mac's `AppModel`.
//!
//! Everything here runs on the GTK main thread. The core calls back on its own threads; those
//! callbacks only send a [`Msg`] to the main thread ([`Model::handle`]).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use annex_discovery::{Advertisement, Browser, Discovery, DiscoveryEvent, PeerInfo};
use annex_session::{
    Core, DisplayInfo, Event, EventSink, FrameSink, InternetServers, JoinCode, LatencyStats, LinkKind, Mode, OwnRelay,
    Quality, TrustPolicy, ViewerConfig, ViewerHandle,
};

use crate::drop_tray::TransferKey;
use crate::host::Host;
use crate::settings::{self, Settings};

/// Sent from the core's threads to the main thread.
pub enum Msg {
    Discovery(DiscoveryEvent),
    /// `generation` tells a replaced viewer's last events apart from the current one's.
    Viewer {
        generation: u64,
        event: Event,
    },
    Host(Event),
    /// The desktop's permission to share the screen, to skip its dialog next time.
    ScreenToken(String),
    /// Something was copied on this computer.
    Copied {
        mime: String,
        data: Vec<u8>,
    },
    /// What's on the clipboard now (asked for when the video window comes to the front).
    ClipboardNow {
        mime: String,
        data: Vec<u8>,
    },
    /// Files dropped on the island or the viewer window, to send.
    DropFiles(Vec<std::path::PathBuf>),
    /// The island's Pause (true) or Resume (false) on a file being sent.
    PauseTransfer(TransferKey, bool),
    /// The island's Unsend on a file being sent, or Stop on one arriving.
    CancelTransfer(TransferKey),
    /// wl-paste stopped (no clipboard access for other programs here): use GTK's clipboard.
    ClipboardWatchEnded,
    /// The sharing pipeline stopped by itself (the screen-sharing permission was refused, the
    /// encoder failed, …).
    HostFailed(String),
    /// GitHub's releases list (its JSON), or why it couldn't be fetched.
    UpdateChecked(Result<String, String>),
}

/// What the UI should do, besides redrawing.
pub enum Change {
    Render,
    /// Viewer: ask for the PIN the other computer shows.
    PinRequired {
        request_id: u64,
        peer_name: String,
        fingerprint: String,
    },
    /// Host: show this PIN until `PinDone` for the same request.
    ShowPin {
        request_id: u64,
        pin: String,
        peer_name: String,
        fingerprint: String,
    },
    PinDone {
        request_id: u64,
    },
    /// Host: a viewer wants to control this computer; ask before allowing it.
    InputRequested {
        peer_name: String,
        fingerprint: String,
    },
    /// A desktop notification (sessions starting and ending).
    Notify {
        title: String,
        body: String,
    },
    /// The session's first picture is coming: open the video window.
    ViewerOpen,
    ViewerClose,
    /// The stream changed size (Extend: the other computer's display was resized).
    ViewerStream {
        width: u32,
        height: u32,
    },
}

/// This computer's part in a live session, said from its own side.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SessionRole {
    Sharing { with: String, extend: bool },
    Viewing { from: String, extend: bool },
}

impl SessionRole {
    pub fn headline(&self) -> String {
        match self {
            SessionRole::Sharing { extend, .. } => {
                format!("This computer is sharing · {}", if *extend { "Extend" } else { "Mirror" })
            }
            SessionRole::Viewing { extend, .. } => {
                format!("This computer is viewing · {}", if *extend { "Extend" } else { "Mirror" })
            }
        }
    }
}

/// Sharing over the internet with a code.
#[derive(Clone, PartialEq, Debug)]
pub enum InternetShare {
    Starting,
    Code { code: String, expires: std::time::Instant },
    Joining(String),
    Relay { name: String, relayed: bool, limited: bool },
    Ended(Option<String>),
}

/// Checking GitHub for a newer release. Linux only says so and opens the release page: the
/// package manager installs it (.deb or .rpm).
#[derive(Clone, PartialEq, Debug)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(annex_update::Update),
    Failed(String),
}

impl UpdateState {
    /// "Annex 1.2.3 is available".
    pub fn headline(u: &annex_update::Update) -> String {
        if u.downgrade {
            format!("Annex {} is available (leaves the beta)", u.version)
        } else if u.prerelease {
            format!("Annex {} (beta) is available", u.version)
        } else {
            format!("Annex {} is available", u.version)
        }
    }
}

/// A session this computer shows (host side).
#[derive(Clone, PartialEq, Debug)]
pub struct HostSession {
    pub id: u64,
    pub name: String,
    pub fingerprint: String,
    pub extend: bool,
    pub input: bool,
}

/// The viewer's link to the video window, so frames skip the main thread.
pub type FrameTarget = Arc<Mutex<Option<FrameSink>>>;

pub struct Viewer {
    pub handle: Arc<ViewerHandle>,
    pub frames: FrameTarget,
}

#[derive(Default)]
struct Presence {
    enabled: bool,
    hosting: bool,
    advert: Option<Advertisement>,
}

pub struct Model {
    pub rt: tokio::runtime::Runtime,
    pub core: Arc<Core>,
    pub settings: Settings,
    pub startup_error: Option<String>,
    discovery: Option<Arc<Discovery>>,
    _browser: Option<Browser>,
    presence: Presence,
    peers: BTreeMap<String, PeerInfo>,
    pub trusted: Vec<annex_session::TrustedPeer>,

    // Viewing.
    pub viewer: Option<Viewer>,
    generation: u64,
    pub viewer_status: Option<String>,
    pub viewer_peer: String,
    pub viewer_extends: Option<bool>,
    pub viewer_route: String,
    pub viewer_by_code: bool,
    pub controlling: bool,
    pub pointer_deltas: bool,
    pub latency: Option<LatencyStats>,
    /// Viewer: the stream's size, for the window.
    pub stream_size: Option<(u32, u32)>,

    // Sharing.
    pub host: Option<Host>,
    pub host_error: Option<String>,
    pub host_sessions: Vec<HostSession>,
    pub internet: Option<InternetShare>,
    /// The port sharing listens on (for the addresses on the Share page).
    pub host_port: Option<u16>,

    // Updates.
    pub update: UpdateState,
    /// The version whose banner was closed with Not Now.
    pub update_dismissed: Option<String>,
    /// When the last check finished (Unix seconds).
    pub update_checked_at: Option<i64>,

    clipboard: Option<crate::clipboard::Clipboard>,
    drop_tray: Option<crate::drop_tray::DropTray>,
    /// Viewer: the video window takes this computer's keyboard.
    pub viewer_focused: bool,
    /// Viewer: something was copied while the video window wasn't in front.
    copied_while_away: bool,

    tx: async_channel::Sender<Msg>,
    /// What the UI should do once it lets go of the model (see [`Model::take_changes`]).
    changes: Vec<Change>,
}

pub type Shared = Rc<RefCell<Model>>;

impl Model {
    pub fn new(tx: async_channel::Sender<Msg>) -> Shared {
        let settings = Settings::load();
        let data = settings::data_dir();
        annex_log::init(&data, true);
        if settings.verbose_logging {
            annex_log::set_verbose(true);
        }
        let name = settings.device_name.clone().unwrap_or_else(system_name);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("annex-core")
            .enable_all()
            .build()
            .expect("tokio runtime");
        let (core, startup_error) = match Core::open(&data, &name, TrustPolicy::Pair) {
            Ok(c) => (c, None),
            Err(e) => {
                tracing::error!("couldn't open the identity in {}: {e}", data.display());
                let err = format!("Annex couldn't read its identity in {}: {e}", data.display());
                (Core::ephemeral(&name, TrustPolicy::Pair).expect("in-memory identity"), Some(err))
            }
        };
        tracing::info!(target: "annex::pairing", name, fingerprint = core.fingerprint(), "identity ready");
        core.set_headless(settings.headless);
        core.set_downloads(settings.receive_files.then(downloads_dir));
        core.set_audio(settings.audio());
        let model = Rc::new(RefCell::new(Model {
            rt,
            core,
            settings,
            startup_error,
            discovery: None,
            _browser: None,
            presence: Presence::default(),
            peers: BTreeMap::new(),
            trusted: Vec::new(),
            viewer: None,
            generation: 0,
            viewer_status: None,
            viewer_peer: String::new(),
            viewer_extends: None,
            viewer_route: String::new(),
            viewer_by_code: false,
            controlling: false,
            pointer_deltas: false,
            latency: None,
            stream_size: None,
            host: None,
            host_error: None,
            host_sessions: Vec::new(),
            internet: None,
            host_port: None,
            update: UpdateState::Idle,
            update_dismissed: None,
            update_checked_at: None,
            clipboard: None,
            drop_tray: None,
            viewer_focused: false,
            copied_while_away: false,
            tx,
            changes: Vec::new(),
        }));
        {
            let mut m = model.borrow_mut();
            m.refresh_trusted();
            m.start_discovery();
            m.presence.enabled = true;
            m.refresh_presence();
        }
        model
    }

    /// The UI acts on changes after the model's borrow ends, so its handlers can read the model.
    fn emit(&mut self, c: Change) {
        if matches!(c, Change::Render) && self.changes.iter().any(|c| matches!(c, Change::Render)) {
            return;
        }
        self.changes.push(c);
    }

    pub fn render(&mut self) {
        self.emit(Change::Render)
    }

    pub fn take_changes(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.changes)
    }

    pub fn device_name(&self) -> String {
        self.core.device_name()
    }

    pub fn fingerprint_display(&self) -> String {
        display_fingerprint(self.core.fingerprint())
    }

    pub fn save_settings(&self) {
        self.settings.save();
    }

    // ---- Nearby -------------------------------------------------------------------------

    fn start_discovery(&mut self) {
        let d = match self.discovery() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("discovery unavailable: {e}");
                return;
            }
        };
        let tx = self.tx.clone();
        match d.browse(move |e| {
            let _ = tx.send_blocking(Msg::Discovery(e));
        }) {
            Ok(b) => {
                tracing::info!(target: "annex::network", "nearby: looking for computers");
                self._browser = Some(b)
            }
            Err(e) => tracing::warn!("couldn't look for computers nearby: {e}"),
        }
    }

    fn discovery(&mut self) -> Result<Arc<Discovery>, String> {
        if let Some(d) = &self.discovery {
            return Ok(d.clone());
        }
        let d = Arc::new(Discovery::new(self.core.fingerprint()).map_err(|e| e.to_string())?);
        self.discovery = Some(d.clone());
        Ok(d)
    }

    pub fn refresh_nearby(&self) {
        if let Some(b) = &self._browser {
            b.refresh();
        }
    }

    /// Computers nearby, by name.
    pub fn peers(&self) -> Vec<PeerInfo> {
        let mut v: Vec<PeerInfo> = self.peers.values().cloned().collect();
        v.sort_by_key(|p| p.name.to_lowercase());
        v
    }

    pub fn is_paired(&self, peer: &PeerInfo) -> bool {
        peer.fingerprint.as_deref().is_some_and(|fp| self.trusted.iter().any(|t| t.fingerprint == fp))
    }

    // ---- Presence: "Annex is open here" on other computers ------------------------------

    fn refresh_presence(&mut self) {
        self.replace_presence(None);
    }

    /// Records with the same name are updated in place rather than withdrawn and re-registered
    /// (see `Advertisement::replaced_by`), so other computers never see this one vanish.
    fn replace_presence(&mut self, outgoing: Option<Advertisement>) {
        let old: Vec<Advertisement> = self.presence.advert.take().into_iter().chain(outgoing).collect();
        if self.presence.enabled && !self.presence.hosting {
            let name = self.core.device_name();
            match self
                .discovery()
                .and_then(|d| d.advertise(&name, annex_session::DEFAULT_PORT, false, false).map_err(|e| e.to_string()))
            {
                Ok(new) => {
                    tracing::debug!(target: "annex::network", name, "nearby: advertising that Annex is open here");
                    for a in old {
                        a.replaced_by(&new);
                    }
                    self.presence.advert = Some(new);
                    return;
                }
                Err(e) => tracing::warn!("presence advertisement failed: {e}"),
            }
        }
        drop(old);
    }

    /// A host starts advertising this computer as sharing; its record replaces the presence one.
    pub fn advertise_host(&mut self, port: u16, extend: bool) -> Option<Advertisement> {
        self.host_port = Some(port);
        let presence = self.presence.advert.take();
        self.presence.hosting = true;
        let name = self.core.device_name();
        match self.discovery().and_then(|d| d.advertise(&name, port, true, extend).map_err(|e| e.to_string())) {
            Ok(a) => {
                tracing::info!(target: "annex::network", name, port, extend, "nearby: advertising sharing");
                if let Some(old) = presence {
                    old.replaced_by(&a);
                }
                Some(a)
            }
            Err(e) => {
                tracing::warn!("couldn't advertise sharing: {e}");
                self.presence.hosting = false;
                self.presence.advert = presence;
                None
            }
        }
    }

    pub fn end_hosting(&mut self, host_advert: Option<Advertisement>) {
        self.host_port = None;
        self.presence.hosting = false;
        self.replace_presence(host_advert);
    }

    pub fn rename(&mut self, name: &str) {
        let trimmed = name.trim();
        let fallback = system_name();
        let stored = self.core.set_device_name(if trimmed.is_empty() { &fallback } else { trimmed });
        tracing::info!(target: "annex::settings", name = stored, own = !trimmed.is_empty(), "settings: renamed");
        self.settings.device_name = if trimmed.is_empty() { None } else { Some(stored) };
        self.settings.save();
        self.refresh_presence();
        if self.host.is_some() {
            // The sharing record carries the name too.
            self.set_hosting(false);
            self.set_hosting(true);
        }
        self.render();
    }

    // ---- Paired devices -----------------------------------------------------------------

    pub fn refresh_trusted(&mut self) {
        self.trusted = self.core.trusted_peers();
        self.trusted.sort_by_key(|p| p.name.to_lowercase());
    }

    pub fn forget(&mut self, fingerprint: &str) {
        tracing::info!(target: "annex::pairing", fingerprint, "paired devices: forget");
        if let Err(e) = self.core.untrust(fingerprint) {
            tracing::warn!("couldn't remove a paired device: {e}");
        }
        self.refresh_trusted();
        self.render();
    }

    pub fn set_allow_input(&mut self, fingerprint: &str, allow: bool) {
        tracing::info!(target: "annex::pairing", fingerprint, allow, "paired devices: mouse and keyboard");
        if let Err(e) = self.core.set_allow_input(fingerprint, allow) {
            tracing::warn!("couldn't change input permission: {e}");
        }
        self.refresh_trusted();
        self.render();
    }

    pub fn submit_pin(&self, request_id: u64, pin: &str) {
        tracing::info!(target: "annex::pairing", request_id, digits = pin.len(), "pairing: PIN typed");
        self.core.submit_pin(request_id, pin);
    }

    pub fn cancel_request(&self, request_id: u64) {
        tracing::info!(target: "annex::pairing", request_id, "pairing: cancelled here");
        self.core.cancel_request(request_id);
    }

    // ---- Names --------------------------------------------------------------------------

    /// Another computer's name as Annex shows it. Two computers of one owner can have the same
    /// name; then "(other computer)" says which one is meant.
    pub fn label(&self, name: &str) -> String {
        if self.has_this_name(name) { format!("{name} (other computer)") } else { name.to_string() }
    }

    pub fn has_this_name(&self, name: &str) -> bool {
        fn key(s: &str) -> String {
            s.replace('’', "'").trim().to_lowercase()
        }
        let mine = self.core.device_name();
        !mine.is_empty() && key(name) == key(&mine)
    }

    /// A computer nearby or in the session with this computer's name, for the rename notice.
    pub fn same_name_peer(&self) -> Option<String> {
        let session = self.role().map(|r| match r {
            SessionRole::Sharing { with, .. } => with,
            SessionRole::Viewing { from, .. } => from,
        });
        session.into_iter().chain(self.peers.values().map(|p| p.name.clone())).find(|n| self.has_this_name(n))
    }

    pub fn role(&self) -> Option<SessionRole> {
        if self.viewer.is_some()
            && let Some(extend) = self.viewer_extends
        {
            return Some(SessionRole::Viewing { from: self.viewer_peer.clone(), extend });
        }
        let s = self.host_sessions.iter().find(|s| s.extend).or(self.host_sessions.first())?;
        Some(SessionRole::Sharing { with: s.name.clone(), extend: s.extend })
    }

    // ---- Viewing ------------------------------------------------------------------------

    fn quality(&self) -> Quality {
        match self.settings.quality {
            1 => Quality::Balanced,
            2 => Quality::Fast,
            _ => Quality::Best,
        }
    }

    pub fn connect_peer(&mut self, peer: &PeerInfo, extend: bool, display: DisplayInfo) {
        let Some(address) = connect_address(peer) else {
            tracing::warn!(target: "annex::viewer", peer = peer.name, "viewer: no address to connect to");
            self.viewer_status = Some(format!("{} didn't say how to reach it.", peer.name));
            self.render();
            return;
        };
        // Pin the certificate of a computer already paired with, so a look-alike can't step in.
        let pinned = peer.fingerprint.clone().filter(|fp| self.trusted.iter().any(|t| &t.fingerprint == fp));
        tracing::info!(
            target: "annex::viewer",
            peer = peer.name,
            address,
            extend,
            paired = pinned.is_some(),
            "viewer: connect to a computer nearby"
        );
        self.connect(address, peer.name.clone(), pinned, extend, display, None);
    }

    pub fn connect_address(&mut self, address: &str, extend: bool, display: DisplayInfo) {
        let address = address.trim().to_string();
        if address.is_empty() {
            return;
        }
        tracing::info!(target: "annex::viewer", address, extend, "viewer: connect by address");
        self.connect(address.clone(), address, None, extend, display, None);
    }

    pub fn connect_code(&mut self, code: &str, extend: bool, display: DisplayInfo) {
        let code = code.trim().to_uppercase();
        if code.is_empty() {
            return;
        }
        // Never the code itself.
        tracing::info!(target: "annex::viewer", extend, server = self.settings.server(), "viewer: connect with a code");
        let servers = InternetServers {
            rendezvous: self.settings.server().to_string(),
            stun: annex_session::DEFAULT_STUN.to_string(),
        };
        self.connect(
            String::new(),
            "the other computer".into(),
            None,
            extend,
            display,
            Some(JoinCode { code: code.clone(), servers }),
        );
        self.viewer_status = Some(format!("Connecting with code {code}…"));
        self.viewer_by_code = true;
        self.render();
    }

    fn connect(
        &mut self,
        address: String,
        title: String,
        expected: Option<String>,
        extend: bool,
        display: DisplayInfo,
        join: Option<JoinCode>,
    ) {
        self.disconnect();
        self.generation += 1;
        let generation = self.generation;
        let tx = self.tx.clone();
        let events: EventSink = Arc::new(move |event| {
            annex_session::log_event(&event);
            let _ = tx.send_blocking(Msg::Viewer { generation, event });
        });
        let frames: FrameTarget = Arc::new(Mutex::new(None));
        let target = frames.clone();
        let frame_sink: FrameSink = Arc::new(move |f| {
            if let Some(sink) = target.lock().unwrap().as_ref() {
                sink(f);
            }
        });
        let cfg = ViewerConfig {
            address,
            display,
            codecs: vec![annex_session::Codec::H264],
            expected_fingerprint: expected,
            mode: if extend { Mode::Extend } else { Mode::Mirror },
            input: self.settings.use_mouse_and_keyboard,
            quality: self.quality(),
            latency_csv: std::env::var_os("ANNEX_LATENCY_CSV").map(Into::into),
            internet: None,
            join,
        };
        tracing::debug!(
            target: "annex::viewer",
            generation,
            input = cfg.input,
            quality = ?cfg.quality,
            width = cfg.display.width_px,
            height = cfg.display.height_px,
            hz = cfg.display.refresh_hz,
            scale = cfg.display.scale_percent,
            "viewer: starting"
        );
        let _enter = self.rt.enter();
        let handle = Arc::new(annex_session::connect_viewer(self.core.clone(), cfg, events, frame_sink));
        self.viewer = Some(Viewer { handle, frames });
        self.viewer_peer = title.clone();
        self.viewer_status = Some(format!("Connecting to {title}…"));
        self.viewer_extends = None;
        self.viewer_by_code = false;
        self.render();
    }

    /// Ends the session. The connection closes in the background: it waits up to 300 ms for the
    /// goodbye to go out, which would otherwise hold up the window closing.
    pub fn disconnect(&mut self) {
        self.generation += 1;
        if let Some(v) = self.viewer.take() {
            tracing::info!(target: "annex::viewer", peer = self.viewer_peer, "viewer: disconnecting");
            *v.frames.lock().unwrap() = None;
            let handle = v.handle;
            self.rt.spawn(async move { handle.disconnect().await });
        }
        self.viewer_status = None;
        self.viewer_extends = None;
        self.controlling = false;
        self.viewer_focused = false;
        self.copied_while_away = false;
        self.latency = None;
        self.stream_size = None;
        self.emit(Change::ViewerClose);
        self.render();
    }

    fn handle_viewer(&mut self, event: Event) {
        match event {
            Event::SessionStarted { mode, peer_name, stream, link, input, pointer_deltas, .. } => {
                let extend = mode == Mode::Extend;
                let label = self.label(&peer_name);
                let via = match link {
                    LinkKind::Direct => " · cable",
                    LinkKind::Internet => " · internet",
                    LinkKind::Relayed => " · relay",
                    LinkKind::Network => "",
                };
                self.viewer_status = Some(format!(
                    "{}{via}",
                    if extend { format!("Extra display from {label}") } else { format!("Viewing {label}") }
                ));
                self.viewer_route = match link {
                    LinkKind::Direct => "Over a cable",
                    LinkKind::Internet => "Over the internet",
                    LinkKind::Relayed => "Through a relay",
                    LinkKind::Network => "",
                }
                .into();
                self.viewer_peer = peer_name;
                self.viewer_extends = Some(extend);
                self.controlling = input;
                self.pointer_deltas = extend && pointer_deltas;
                self.stream_size = Some((stream.width, stream.height));
                self.emit(Change::ViewerOpen);
            }
            Event::StreamConfig { stream } => {
                self.stream_size = Some((stream.width, stream.height));
                self.emit(Change::ViewerStream { width: stream.width, height: stream.height });
            }
            Event::InputAllowed { allowed, .. } => {
                self.controlling = allowed;
            }
            Event::LatencyStats { stats, .. } => {
                self.latency = Some(stats);
                // Numbers only; the window and the session bar redraw on their own.
                self.render();
                return;
            }
            Event::PairingPinRequired { request_id, peer_name, fingerprint_display } => {
                self.emit(Change::PinRequired { request_id, peer_name, fingerprint: fingerprint_display });
            }
            Event::PairingFinished { request_id, success, peer_name } => {
                self.emit(Change::PinDone { request_id });
                if success {
                    self.refresh_trusted();
                } else {
                    self.viewer_status =
                        Some(format!("Pairing with {peer_name} didn't finish. Check the PIN and try again."));
                }
            }
            Event::SessionEnded { reason, .. } => {
                let status = if reason == "user" {
                    None
                } else {
                    Some(friendly_reason(&reason, &self.label(&self.viewer_peer), self.viewer_by_code))
                };
                self.disconnect();
                self.viewer_status = status;
            }
            Event::ClipboardReceived { mime, data, .. } => {
                self.clipboard_received(&mime, &data);
                return;
            }
            e @ (Event::TransferStarted { .. } | Event::TransferProgress { .. } | Event::TransferEnded { .. }) => {
                self.transfer(e, true);
                return;
            }
            _ => {}
        }
        self.render();
    }

    // ---- Sharing ------------------------------------------------------------------------

    pub fn is_hosting(&self) -> bool {
        self.host.is_some()
    }

    pub fn set_hosting(&mut self, on: bool) {
        if on == self.host.is_some() {
            return;
        }
        if on {
            tracing::info!(target: "annex::host", "sharing: turning on");
            self.host_error = None;
            match Host::start(self) {
                Ok(h) => self.host = Some(h),
                Err(e) => {
                    tracing::error!(target: "annex::host", "sharing: didn't start: {e}");
                    self.host_error = Some(e)
                }
            }
        } else if let Some(h) = self.host.take() {
            tracing::info!(target: "annex::host", sessions = self.host_sessions.len(), "sharing: turning off");
            let advert = h.stop(&self.rt);
            self.host_sessions.clear();
            self.internet = None;
            self.end_hosting(advert);
        }
        self.render();
    }

    pub fn share_code(&mut self) {
        if self.host.is_none() {
            return;
        }
        let servers = InternetServers {
            rendezvous: self.settings.server().to_string(),
            stun: annex_session::DEFAULT_STUN.into(),
        };
        let s = &self.settings;
        let own_relay = (!s.relay_url.trim().is_empty()).then(|| OwnRelay {
            url: s.relay_url.trim().to_string(),
            username: s.relay_username.clone(),
            password: s.relay_password.clone(),
        });
        let name = self.core.device_name();
        tracing::info!(
            target: "annex::network",
            server = self.settings.server(),
            own_relay = own_relay.is_some(),
            "sharing: getting a code"
        );
        if let Some(h) = self.host.as_mut() {
            h.share_code(&self.rt, name, servers, own_relay);
        }
        self.internet = Some(InternetShare::Starting);
        self.render();
    }

    pub fn stop_code(&mut self) {
        tracing::info!(target: "annex::network", "sharing: code stopped here");
        if let Some(h) = self.host.as_mut() {
            h.stop_code();
        }
        self.internet = None;
        self.render();
    }

    fn handle_host(&mut self, event: Event) {
        match event {
            Event::SessionStarted { session_id, mode, peer_name, fingerprint, input, .. } => {
                let extend = mode == Mode::Extend;
                self.host_sessions.retain(|s| s.id != session_id);
                self.host_sessions.push(HostSession {
                    id: session_id,
                    name: peer_name.clone(),
                    fingerprint,
                    extend,
                    input,
                });
                self.emit(Change::Notify {
                    title: if extend {
                        format!("{} is using this computer as a display", self.label(&peer_name))
                    } else {
                        format!("{} is viewing this screen", self.label(&peer_name))
                    },
                    body: "Stop sharing in Annex to end it.".into(),
                });
            }
            Event::SessionEnded { session_id, .. } => {
                self.host_sessions.retain(|s| s.id != session_id);
            }
            Event::InputAllowed { session_id, allowed } => {
                if let Some(s) = self.host_sessions.iter_mut().find(|s| s.id == session_id) {
                    s.input = allowed;
                }
            }
            Event::InputRequested { peer_name, fingerprint, .. } => {
                self.emit(Change::InputRequested { peer_name, fingerprint });
            }
            Event::PairingPin { request_id, pin, peer_name, fingerprint_display } => {
                self.emit(Change::ShowPin { request_id, pin, peer_name, fingerprint: fingerprint_display });
            }
            Event::PairingFinished { request_id, success, .. } => {
                self.emit(Change::PinDone { request_id });
                if success {
                    self.refresh_trusted();
                }
            }
            Event::InternetCode { code, expires_in } => {
                self.internet = Some(InternetShare::Code {
                    code,
                    expires: std::time::Instant::now() + std::time::Duration::from_secs(expires_in.into()),
                });
            }
            Event::InternetJoined { peer_name } => self.internet = Some(InternetShare::Joining(peer_name)),
            Event::InternetRelay { relayed, limited } => {
                if let Some(InternetShare::Joining(name)) = &self.internet {
                    self.internet = Some(InternetShare::Relay { name: name.clone(), relayed, limited });
                }
            }
            Event::InternetCodeEnded { reason } => {
                self.internet = match (reason.as_str(), &self.internet) {
                    ("stopped", _) => None,
                    ("used", Some(InternetShare::Relay { relayed: false, .. })) => {
                        Some(InternetShare::Ended(Some("It couldn't connect directly, and no relay was free.".into())))
                    }
                    ("used", _) => Some(InternetShare::Ended(None)),
                    ("expired", _) => Some(InternetShare::Ended(Some("The code expired.".into()))),
                    (r, _) => Some(InternetShare::Ended(Some(friendly_internet_error(r)))),
                };
            }
            Event::ClipboardReceived { mime, data, .. } => {
                self.clipboard_received(&mime, &data);
                return;
            }
            e @ (Event::TransferStarted { .. } | Event::TransferProgress { .. } | Event::TransferEnded { .. }) => {
                self.transfer(e, false);
                return;
            }
            Event::Error { message } => {
                tracing::debug!(target: "annex::host", "sharing: showing the error in the banner");
                self.host_error = Some(message);
            }
            _ => return,
        }
        self.render();
    }

    pub fn handle(&mut self, msg: Msg) {
        self.handle_msg(msg);
        self.update_drop_target();
    }

    fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Discovery(DiscoveryEvent::PeerFound(p)) => {
                tracing::debug!(
                    target: "annex::network",
                    name = p.name,
                    addresses = ?p.addresses,
                    port = p.port,
                    sharing = p.sharing,
                    extend = ?p.extend,
                    mesh = p.mesh,
                    "nearby: found"
                );
                self.peers.insert(p.instance.clone(), p);
                self.render();
            }
            Msg::Discovery(DiscoveryEvent::PeerLost { instance }) => {
                tracing::debug!(target: "annex::network", instance, "nearby: gone");
                if self.peers.remove(&instance).is_some() {
                    self.render();
                }
            }
            Msg::Viewer { generation, event } => {
                if generation == self.generation {
                    self.handle_viewer(event);
                } else {
                    tracing::trace!(target: "annex::viewer", generation, "viewer: event from an ended session, ignored");
                }
            }
            Msg::Host(event) => self.handle_host(event),
            Msg::ScreenToken(t) => {
                tracing::info!(target: "annex::permissions", "sharing: the desktop's permission is saved for next time");
                self.settings.screen_token = Some(t);
                self.settings.save();
            }
            Msg::Copied { mime, data } => self.copied(mime, data),
            Msg::DropFiles(files) => self.send_files(files),
            Msg::PauseTransfer(key, paused) => self.pause_transfer(key, paused),
            Msg::CancelTransfer(key) => self.cancel_transfer(key),
            Msg::ClipboardNow { mime, data } => {
                const T: &str = "annex::clipboard";
                match &self.viewer {
                    Some(v) if self.controlling && self.clipboard.is_some() => {
                        tracing::debug!(target: T, mime, bytes = data.len(), "clipboard: sent what was copied while away");
                        v.handle.send_clipboard(&mime, data);
                    }
                    Some(_) if self.clipboard.is_none() => {
                        tracing::debug!(target: T, "clipboard: not sent, sharing is off")
                    }
                    Some(_) => tracing::debug!(target: T, "clipboard: not sent, not controlling"),
                    None => tracing::debug!(target: T, "clipboard: not sent, the session ended"),
                }
            }
            Msg::ClipboardWatchEnded => {
                tracing::info!("clipboard: wl-paste can't watch the clipboard here; using GTK's");
                if self.clipboard.is_some() {
                    self.clipboard = crate::clipboard::Clipboard::start_gtk(self.tx.clone());
                }
            }
            Msg::HostFailed(why) => {
                tracing::warn!(target: "annex::host", "sharing: stopping, it failed: {why}");
                self.set_hosting(false);
                self.host_error = Some(why);
                self.render();
            }
            Msg::UpdateChecked(result) => self.update_checked(result),
        }
    }

    // ---- Updates ------------------------------------------------------------------------

    /// Asks GitHub for the releases list, in the background. No HTTP client is built in: `curl`
    /// fetches it (see [`fetch_releases`]).
    pub fn check_for_updates(&mut self) {
        if self.update == UpdateState::Checking {
            return;
        }
        tracing::info!(
            target: "annex::update",
            current = annex_update::whats_new::CURRENT,
            beta = self.settings.beta_updates,
            "update: checking"
        );
        self.update = UpdateState::Checking;
        let tx = self.tx.clone();
        let spawned = std::thread::Builder::new().name("annex-update".into()).spawn(move || {
            let _ = tx.send_blocking(Msg::UpdateChecked(fetch_releases()));
        });
        if let Err(e) = spawned {
            tracing::warn!(target: "annex::update", "update: couldn't start the check: {e}");
            self.update = UpdateState::Failed(e.to_string());
        }
        self.render();
    }

    fn update_checked(&mut self, result: Result<String, String>) {
        const T: &str = "annex::update";
        self.update_checked_at =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64);
        self.update = match result.and_then(|json| pick_update(&json, self.settings.beta_updates)) {
            Ok(Some(u)) => {
                tracing::info!(target: T, version = u.version, asset = u.asset_name, "update: available");
                UpdateState::Available(u)
            }
            Ok(None) => {
                tracing::info!(target: T, "update: this is the latest version");
                UpdateState::UpToDate
            }
            Err(e) => {
                tracing::warn!(target: T, "update: check failed: {e}");
                UpdateState::Failed(e)
            }
        };
        self.render();
    }

    /// The banner on Nearby and Share: an update, unless its banner was closed.
    pub fn update_banner(&self) -> Option<&annex_update::Update> {
        match &self.update {
            UpdateState::Available(u) if self.update_dismissed.as_deref() != Some(u.version.as_str()) => Some(u),
            _ => None,
        }
    }

    // ---- Clipboard and files ------------------------------------------------------------

    pub fn start_drop_tray(&mut self) {
        self.drop_tray = crate::drop_tray::DropTray::new(self.tx.clone());
        tracing::info!(target: "annex::drop", island = self.drop_tray.is_some(), "drop: island ready");
    }

    /// Who dropped files go to: the host this computer controls, else a viewer controlling it.
    pub fn update_drop_target(&self) {
        let Some(tray) = &self.drop_tray else { return };
        let peer = if !self.settings.drop_at_top {
            None
        } else if self.viewer.is_some() && self.controlling {
            Some(self.label(&self.viewer_peer))
        } else if self.host.is_some() {
            self.host_sessions.iter().find(|s| s.input).map(|s| self.label(&s.name))
        } else {
            None
        };
        tray.set_target(peer);
    }

    fn files_target(&self) -> &'static str {
        if self.viewer.is_some() && self.controlling {
            "the computer this one controls"
        } else if self.host.is_some() {
            "viewers that control this computer"
        } else {
            "nothing connected"
        }
    }

    fn send_files(&mut self, files: Vec<std::path::PathBuf>) {
        const T: &str = "annex::transfer";
        let bytes: u64 = files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
        tracing::info!(target: T, count = files.len(), bytes, to = self.files_target(), "files: dropped, sending");
        for f in files {
            tracing::debug!(target: T, file = %f.display(), "files: sending");
            let result = if let Some(v) = self.viewer.as_ref().filter(|_| self.controlling) {
                v.handle.send_file(&f).map(drop)
            } else if let Some(h) = &self.host {
                self.host_sessions.iter().filter(|s| s.input).try_for_each(|s| h.send_file(s.id, &f).map(drop))
            } else {
                Err("Nothing is connected.".into())
            };
            if let Err(e) = result {
                tracing::warn!(target: T, file = %f.display(), "files: couldn't send: {e}");
                let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.emit(Change::Notify { title: format!("Couldn't send {name}"), body: e });
            }
        }
    }

    /// `viewer`: from the session this computer views (else one it shares).
    fn transfer(&mut self, e: Event, viewer: bool) {
        let key = |session: u64, id: u64, incoming: bool| TransferKey { viewer, session, id, incoming };
        match e {
            Event::TransferStarted { session_id, id, incoming, name, size } => {
                if let Some(t) = &self.drop_tray {
                    t.started(key(session_id, id, incoming), name, size);
                }
            }
            Event::TransferProgress { session_id, id, incoming, done } => {
                if let Some(t) = &self.drop_tray {
                    t.progress(key(session_id, id, incoming), done);
                }
            }
            Event::TransferEnded { session_id, id, incoming, ok, path, error } => {
                if let Some(t) = &self.drop_tray {
                    t.ended(key(session_id, id, incoming), ok, path, error);
                } else if incoming && ok {
                    // No island here (GNOME, X11): say so where it lands.
                    let name = path
                        .as_deref()
                        .and_then(|p| std::path::Path::new(p).file_name())
                        .map(|n| n.to_string_lossy().into_owned());
                    self.emit(Change::Notify {
                        title: format!("Received {}", name.unwrap_or_else(|| "a file".into())),
                        body: "It's in Downloads.".into(),
                    });
                }
            }
            _ => {}
        }
    }

    /// The island's Pause or Resume: only shown paused once the core took it.
    fn pause_transfer(&mut self, key: TransferKey, paused: bool) {
        const T: &str = "annex::transfer";
        let took = if key.incoming {
            false
        } else if key.viewer {
            self.viewer.as_ref().is_some_and(|v| v.handle.pause_transfer(key.id, paused))
        } else {
            self.host.as_ref().is_some_and(|h| h.pause_transfer(key.session, key.id, paused))
        };
        if !took {
            tracing::warn!(target: T, ?key, paused, "files: couldn't pause or resume: it isn't being sent");
            return;
        }
        if let Some(t) = &self.drop_tray {
            t.set_paused(key, paused);
        }
    }

    /// The island's Unsend (the other side deletes what arrived) or Stop.
    fn cancel_transfer(&mut self, key: TransferKey) {
        const T: &str = "annex::transfer";
        let done = if key.viewer {
            match &self.viewer {
                Some(v) => {
                    v.handle.cancel_transfer(key.incoming, key.id);
                    true
                }
                None => false,
            }
        } else {
            match &self.host {
                Some(h) => {
                    h.cancel_transfer(key.session, key.incoming, key.id);
                    true
                }
                None => false,
            }
        };
        if !done {
            tracing::warn!(target: T, ?key, "files: couldn't stop the transfer: its session is gone");
            return;
        }
        if let Some(t) = &self.drop_tray {
            t.cancelled(key);
        }
    }

    pub fn set_clipboard_sharing(&mut self, on: bool) {
        if !on {
            tracing::info!(target: "annex::clipboard", "clipboard: sharing off");
            self.clipboard = None;
            self.copied_while_away = false;
        } else if self.clipboard.is_none() {
            self.clipboard = crate::clipboard::Clipboard::start(self.tx.clone());
            if self.clipboard.is_none() {
                tracing::warn!(target: "annex::clipboard", "clipboard: no way to watch the clipboard here");
            }
        }
    }

    pub fn set_receive_files(&self, on: bool) {
        tracing::info!(target: "annex::transfer", on, "files: receive into Downloads");
        self.core.set_downloads(on.then(downloads_dir));
    }

    /// Copied here: to the host this computer controls (if copied elsewhere, once its window
    /// comes to the front, and only what's on the clipboard then), and to the viewers controlling
    /// this computer.
    fn copied(&mut self, mime: String, data: Vec<u8>) {
        const T: &str = "annex::clipboard";
        let bytes = data.len();
        if self.clipboard.is_none() {
            tracing::debug!(target: T, mime, bytes, "clipboard: copied here; not sent, sharing is off");
            return;
        }
        tracing::debug!(target: T, mime, bytes, "clipboard: copied here");
        if let Some(h) = &self.host {
            for s in self.host_sessions.iter().filter(|s| s.input) {
                let sent = h.send_clipboard(s.id, &mime, data.clone());
                tracing::debug!(target: T, session = s.id, peer = s.name, sent, "clipboard: to a viewer");
            }
            if !self.host_sessions.iter().any(|s| s.input) {
                tracing::debug!(target: T, "clipboard: no viewer controls this computer; not sent to viewers");
            }
        }
        match self.viewer.as_ref() {
            Some(v) if self.controlling => {
                if self.viewer_focused {
                    tracing::debug!(target: T, peer = self.viewer_peer, "clipboard: to the computer this one controls");
                    v.handle.send_clipboard(&mime, data);
                } else {
                    tracing::debug!(target: T, "clipboard: the video window isn't in front; sent once it is");
                    self.copied_while_away = true;
                }
            }
            Some(_) => tracing::debug!(target: T, "clipboard: viewing without control; not sent"),
            None => {}
        }
    }

    /// The video window came to the front.
    pub fn viewer_focus(&mut self, focused: bool) {
        if focused != self.viewer_focused {
            tracing::debug!(target: "annex::viewer", focused, "viewer: the video window {}", if focused { "came to the front" } else { "went to the back" });
        }
        self.viewer_focused = focused;
        if focused
            && self.controlling
            && std::mem::take(&mut self.copied_while_away)
            && let Some(c) = &self.clipboard
        {
            tracing::debug!(target: "annex::clipboard", "clipboard: reading it again, something was copied while away");
            c.read_now();
        }
    }

    fn clipboard_received(&self, mime: &str, data: &[u8]) {
        match &self.clipboard {
            Some(c) => c.apply(mime, data),
            None => {
                tracing::debug!(target: "annex::clipboard", mime, bytes = data.len(), "clipboard: received; not applied, sharing is off")
            }
        }
    }

    pub fn sender(&self) -> async_channel::Sender<Msg> {
        self.tx.clone()
    }

    /// Quitting: stop sharing (viewers get the goodbye) and end the session.
    pub fn quit(&mut self) {
        self.disconnect();
        self.set_hosting(false);
        self.presence.enabled = false;
        self.presence.advert = None;
    }
}

/// Every address as `host:port` (IPv6 bracketed), comma-separated. The core races them and
/// prefers a direct cable (link-local address).
pub fn connect_address(peer: &PeerInfo) -> Option<String> {
    if peer.addresses.is_empty() {
        return None;
    }
    Some(
        peer.addresses
            .iter()
            .map(|a| if a.contains(':') { format!("[{a}]:{}", peer.port) } else { format!("{a}:{}", peer.port) })
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// The computer's own name: the "pretty" hostname desktops let people set, else the hostname.
pub fn system_name() -> String {
    let pretty = std::fs::read_to_string("/etc/machine-info").ok().and_then(|s| {
        s.lines().find_map(|l| l.strip_prefix("PRETTY_HOSTNAME=").map(|v| v.trim_matches('"').to_string()))
    });
    pretty
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Linux computer".into())
}

/// How another computer reaches this one at a local address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum LocalLink {
    Network,
    /// A mesh VPN (Tailscale, ZeroTier).
    Private,
    /// 169.254.x.x: a cable straight between the two computers.
    Cable,
}

impl LocalLink {
    pub fn label(self) -> &'static str {
        match self {
            LocalLink::Network => "Network",
            LocalLink::Private => "Private network",
            LocalLink::Cable => "Cable",
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct LocalAddress {
    pub link: LocalLink,
    pub interface: String,
    pub ip: std::net::Ipv4Addr,
}

/// This computer's IPv4 addresses (easier to type) on interfaces that are up, for telling the
/// user what to type on the other computer: the local network first, then private networks, then
/// cables. Loopback and container or VM bridges nobody would type are left out.
pub fn local_addresses() -> Vec<LocalAddress> {
    const SKIP: [&str; 10] = ["docker", "br-", "veth", "virbr", "vnet", "lxc", "lxd", "podman", "cni", "flannel"];
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out: Vec<LocalAddress> = interfaces
        .into_iter()
        .filter(|i| i.is_oper_up() && !i.is_loopback() && !SKIP.iter().any(|s| i.name.starts_with(s)))
        .filter_map(|i| {
            let if_addrs::IfAddr::V4(v4) = &i.addr else {
                return None;
            };
            let ip = v4.ip;
            let [a, b, ..] = ip.octets();
            let link = if ip.is_link_local() {
                LocalLink::Cable
            } else if i.name.starts_with("tailscale")
                || i.name.starts_with("zt")
                || (a == 100 && (64..128).contains(&b))
            {
                LocalLink::Private
            } else {
                LocalLink::Network
            };
            Some(LocalAddress { link, interface: i.name, ip })
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// "34CB B46A 8CDC 86A2 4102": the first 20 hex digits, in groups of four.
pub fn display_fingerprint(fp: &str) -> String {
    fp.chars()
        .take(20)
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect::<String>().to_uppercase())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn friendly_reason(reason: &str, peer: &str, by_code: bool) -> String {
    match reason {
        "extend_not_supported" => format!("{peer} can't add a display. Choose Show Its Screen instead."),
        "busy" => format!("{peer} is already sharing with 4 computers."),
        "pairing_failed" => "Wrong PIN. Try again.".into(),
        "pairing_cancelled" => "Pairing was cancelled on the other computer.".into(),
        "pairing_locked" => "Too many wrong PINs. Try again in 10 minutes.".into(),
        "host_stopped" => format!("{peer} stopped sharing."),
        "host_ended_session" => format!("{peer} ended the session."),
        "unknown_code" => "That code doesn't work. Codes work once, for 10 minutes: ask for a new one.".into(),
        "too_many_attempts" => "Too many codes tried. Wait 10 minutes, then try again.".into(),
        "code_expired" => "The code expired. Ask for a new one.".into(),
        "closed" | "connection closed" => format!("Disconnected from {peer}."),
        r if by_code => format!("Couldn't connect over the internet: {r}"),
        r => format!("Couldn't connect to {peer}: {r}"),
    }
}

fn friendly_internet_error(r: &str) -> String {
    if r.contains("rendezvous") || r.contains("connect") {
        "Couldn't reach Annex's server. Check the internet connection.".into()
    } else {
        format!("Sharing with a code stopped: {r}")
    }
}

/// GitHub's releases list, with `curl` (installed nearly everywhere; the app has no HTTP client).
fn fetch_releases() -> Result<String, String> {
    let url = annex_update::RELEASES_URL.to_str().unwrap_or_default();
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "30", "-H", "Accept: application/vnd.github+json", "-A"])
        .arg(format!("Annex/{}", annex_update::whats_new::CURRENT))
        .arg(url)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "curl isn't installed".to_string()
            } else {
                format!("couldn't run curl: {e}")
            }
        })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("curl failed ({})", out.status) } else { err });
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

/// The release to offer: its .deb, or .rpm on RPM systems (each release has both, per CPU, named
/// as scripts/package-linux.sh names them: `Annex-<v>-linux-<x86_64|arm64>.<deb|rpm>`).
fn pick_update(json: &str, beta: bool) -> Result<Option<annex_update::Update>, String> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        a => a,
    };
    let rpm = !std::path::Path::new("/usr/bin/dpkg").exists() && std::path::Path::new("/usr/bin/rpm").exists();
    let kinds = if rpm { ["rpm", "deb"] } else { ["deb", "rpm"] };
    for kind in kinds {
        let suffix = format!("-linux-{arch}.{kind}");
        if let Some(u) =
            annex_update::pick(json, annex_update::whats_new::CURRENT, beta, &suffix).map_err(|e| e.to_string())?
        {
            return Ok(Some(u));
        }
    }
    Ok(None)
}

/// Where files from the other computer go.
fn downloads_dir() -> std::path::PathBuf {
    gtk::glib::user_special_dir(gtk::glib::UserDirectory::Downloads)
        .unwrap_or_else(|| gtk::glib::home_dir().join("Downloads"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_grouped() {
        assert_eq!(
            display_fingerprint("34cbb46a8cdc86a241024f8b368fa99dcb68388eae4cc3a14309c8f6e49bddc9"),
            "34CB B46A 8CDC 86A2 4102"
        );
    }

    #[test]
    fn addresses_are_raced() {
        let p = PeerInfo {
            instance: "a".into(),
            name: "Mac".into(),
            addresses: vec!["10.0.0.2".into(), "fe80::1%en0".into()],
            port: 47800,
            fingerprint: None,
            proto_major: Some(1),
            sharing: true,
            extend: Some(true),
            mesh: false,
        };
        assert_eq!(connect_address(&p).unwrap(), "10.0.0.2:47800,[fe80::1%en0]:47800");
    }
}
