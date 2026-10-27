//! Sharing this screen: one capture for every Mirror viewer, a new display per Extend viewer,
//! and the viewers' mouse and keyboard. Each desktop has its own way (docs/LINUX.md):
//!
//! | Desktop | Picture | Mouse and keyboard | Extend |
//! |---|---|---|---|
//! | GNOME | Mutter screen cast | Mutter remote desktop | Mutter virtual monitor |
//! | KDE, other Wayland | ScreenCast portal | RemoteDesktop portal | portal virtual source (KDE) |
//! | labwc, sway | wlr-screencopy | virtual pointer and keyboard | headless output |
//! | X11 | XShm (ximagesrc) | XTest | — |
//!
//! Capture lifecycles follow the sessions, like the Windows host: the core's events are handled
//! on its own threads, so input never waits for the window.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use annex_discovery::Advertisement;
use annex_media::geometry::stream_size;
use annex_session::{
    Codec, DisplayInfo, Event, EventSink, HostConfig, HostHandle, InternetServers, MAX_H264, Mode, OwnRelay,
    StreamConfig,
};

use crate::desktop::{self, Session};
use crate::encode::{self, EncodedFrame, Pipeline};
use crate::inject::Injector;
use crate::model::{Model, Msg};
use crate::screencopy::Screencopy;

/// What Mirror viewers see.
enum Screen {
    Portal(crate::portal::Started),
    Gnome(crate::gnome::Started),
    Wlr { output: String },
    X11,
}

/// A capture → encoder pipeline, and the screencopy thread feeding it (wlroots).
struct Running {
    _pipeline: Pipeline,
    stop: Arc<AtomicBool>,
}

impl Running {
    fn pipeline(&self) -> &Pipeline {
        &self._pipeline
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[derive(Default)]
struct ExtendSession {
    stop: Arc<AtomicBool>,
    running: Arc<Mutex<Option<Running>>>,
    injector: Arc<Mutex<Option<Injector>>>,
    display: Option<crate::extend::Display>,
    /// GNOME/KDE: the session that owns the virtual monitor.
    gnome: Option<crate::gnome::Started>,
    portal: Option<crate::portal::Started>,
}

struct State {
    session: Session,
    prefer_hardware: bool,
    stream: StreamConfig,
    rt: tokio::runtime::Handle,
    tx: async_channel::Sender<Msg>,
    host: OnceLock<Weak<HostHandle>>,
    /// None until the desktop allowed sharing (GNOME and portals answer asynchronously).
    screen: Mutex<Option<Screen>>,
    screen_failed: AtomicBool,
    mirror: Mutex<Option<Running>>,
    mirror_sessions: Mutex<HashSet<u64>>,
    /// Latest adaptive-bitrate hint per mirror session; the shared encoder follows the lowest.
    mirror_scales: Mutex<HashMap<u64, u32>>,
    injector: Mutex<Option<Injector>>,
    extends: Mutex<HashMap<u64, ExtendSession>>,
}

pub struct Host {
    handle: Arc<HostHandle>,
    advert: Option<Advertisement>,
    state: Arc<State>,
    code: Option<tokio::task::JoinHandle<()>>,
}

/// Whether this desktop can add a display for a viewer.
fn extend_supported(s: Session) -> bool {
    match s {
        Session::Gnome | Session::Kde => true,
        Session::Wlroots(w) => crate::extend::supported(w),
        Session::OtherWayland | Session::X11 => false,
    }
}

/// A line for the Share page about Extend on this desktop.
pub fn extend_note() -> &'static str {
    if extend_supported(desktop::session()) {
        "Other computers can also use this one as an extra display."
    } else {
        "This desktop can't be used as an extra display yet; viewers see this screen."
    }
}

fn first_output() -> Option<(String, (i32, i32))> {
    let wl = crate::wl::Wayland::connect().ok()?;
    wl.state.outputs.iter().find(|o| !o.name.is_empty()).map(|o| (o.name.clone(), o.size))
}

impl Host {
    pub fn start(m: &mut Model) -> Result<Host, String> {
        let session = desktop::session();
        let prefer_hardware = m.settings.prefer_hardware_encoder;
        tracing::info!("sharing on {}", session.describe());

        // The screen's size, for the stream.
        let display = crate::viewer::display_info(None::<&gtk::Widget>);
        let (sw, sh) = match session {
            Session::Wlroots(_) => first_output()
                .map(|(_, (w, h))| (w.max(2) as u32, h.max(2) as u32))
                .unwrap_or((display.width_px, display.height_px)),
            _ => (display.width_px, display.height_px),
        };
        // The graphics chip handles 1440p60; the software encoders are sized for 1080p.
        let (max_w, max_h) = if encode::hardware(prefer_hardware) { (2560, 1600) } else { (1920, 1200) };
        let (width, height) = stream_size(sw, sh, max_w, max_h);
        // Software encoders keep up with 1080p at 30 fps on a Raspberry Pi 5 (at 60, frames
        // queue up and the delay jumps).
        let fps = if encode::hardware(prefer_hardware) { display.refresh_hz.clamp(30, 60) } else { 30 };
        let stream = StreamConfig { codec: Codec::H264, width, height, fps };

        let state = Arc::new(State {
            session,
            prefer_hardware,
            stream: stream.clone(),
            rt: m.rt.handle().clone(),
            tx: m.sender(),
            host: OnceLock::new(),
            screen: Mutex::new(None),
            screen_failed: AtomicBool::new(false),
            mirror: Mutex::new(None),
            mirror_sessions: Mutex::new(HashSet::new()),
            mirror_scales: Mutex::new(HashMap::new()),
            injector: Mutex::new(None),
            extends: Mutex::new(HashMap::new()),
        });
        let events: EventSink = {
            let (state, tx) = (state.clone(), m.sender());
            Arc::new(move |e: Event| {
                state.on_event(&e);
                // Input and encoder hints are handled here; the window needs the rest.
                if !matches!(e, Event::Input { .. } | Event::KeyframeRequested { .. } | Event::BitrateHint { .. }) {
                    let _ = tx.send_blocking(Msg::Host(e));
                }
            })
        };
        let extend = extend_supported(session);
        // Annex's port if it's free, else any (the CLI may hold it).
        let port = annex_session::DEFAULT_PORT;
        let port = if std::net::TcpListener::bind(("0.0.0.0", port)).is_ok() { port } else { 0 };
        let cfg = HostConfig { port, stream, extend, udp: None };
        let handle = match m.rt.block_on(annex_session::start_host(m.core.clone(), cfg, events)) {
            Ok(h) => h,
            Err(e) => return Err(format!("Couldn't start sharing: {e}")),
        };
        let handle = Arc::new(handle);
        let _ = state.host.set(Arc::downgrade(&handle));
        let advert = m.advertise_host(handle.port(), extend);

        state.open_screen(m.settings.screen_token.clone());
        Ok(Host { handle, advert, state, code: None })
    }

    /// Stops sharing; returns the advertisement for the presence record to replace.
    pub fn stop(mut self, rt: &tokio::runtime::Runtime) -> Option<Advertisement> {
        if let Some(c) = self.code.take() {
            c.abort();
        }
        self.state.stop_all();
        let handle = self.handle.clone();
        rt.block_on(async move {
            let _ = tokio::time::timeout(Duration::from_secs(1), handle.stop()).await;
        });
        self.advert.take()
    }

    pub fn share_code(
        &mut self,
        rt: &tokio::runtime::Runtime,
        name: String,
        servers: InternetServers,
        own_relay: Option<OwnRelay>,
    ) {
        self.stop_code_task();
        let handle = self.handle.clone();
        let tx = self.state.tx.clone();
        let events: EventSink = Arc::new(move |e| {
            let _ = tx.send_blocking(Msg::Host(e));
        });
        self.code = Some(rt.spawn(annex_session::share_code(handle, name, servers, own_relay, events)));
    }

    fn stop_code_task(&mut self) {
        if let Some(c) = self.code.take() {
            c.abort();
        }
    }

    pub fn stop_code(&mut self) {
        self.stop_code_task();
    }

    /// Sends a file to a viewer; progress comes as transfer events.
    pub fn send_file(&self, session_id: u64, path: &std::path::Path) -> Result<u64, String> {
        self.handle.send_file(session_id, path)
    }

    /// Puts what was copied here on a viewer's clipboard.
    pub fn send_clipboard(&self, session_id: u64, mime: &str, data: Vec<u8>) -> bool {
        self.handle.send_clipboard(session_id, mime, data)
    }
}

impl State {
    fn host(&self) -> Option<Arc<HostHandle>> {
        self.host.get().and_then(|w| w.upgrade())
    }

    fn fail(&self, why: String) {
        tracing::error!("sharing: {why}");
        self.screen_failed.store(true, Ordering::Release);
        let _ = self.tx.send_blocking(Msg::HostFailed(why));
    }

    /// Gets the desktop's permission to share the screen now, while the user is at the computer,
    /// rather than when a viewer connects.
    fn open_screen(self: &Arc<Self>, token: Option<String>) {
        match self.session {
            Session::Gnome => {
                let me = self.clone();
                self.rt.spawn(async move {
                    match crate::gnome::start(false).await {
                        Ok(g) => {
                            let size = (me.stream.width as i32, me.stream.height as i32);
                            *me.injector.lock().unwrap() =
                                Some(Injector::Portal(crate::inject::Portal::new(g.input.clone(), size)));
                            *me.screen.lock().unwrap() = Some(Screen::Gnome(g));
                        }
                        Err(e) => me.fail(e),
                    }
                });
            }
            Session::Kde | Session::OtherWayland => {
                let me = self.clone();
                self.rt.spawn(async move {
                    match crate::portal::start(token, false).await {
                        Ok(p) => {
                            if let Some(t) = &p.token {
                                let _ = me.tx.send(Msg::ScreenToken(t.clone())).await;
                            }
                            let size = p.size.unwrap_or((me.stream.width as i32, me.stream.height as i32));
                            if let Some(input) = &p.input {
                                *me.injector.lock().unwrap() =
                                    Some(Injector::Portal(crate::inject::Portal::new(input.clone(), size)));
                            }
                            *me.screen.lock().unwrap() = Some(Screen::Portal(p));
                        }
                        Err(e) => me.fail(e),
                    }
                });
            }
            Session::Wlroots(_) => match first_output() {
                Some((output, _)) => {
                    match crate::inject::Wlr::new(Some(&output)) {
                        Ok(w) => *self.injector.lock().unwrap() = Some(Injector::Wlr(Box::new(w))),
                        Err(e) => tracing::warn!("no mouse and keyboard control: {e}"),
                    }
                    *self.screen.lock().unwrap() = Some(Screen::Wlr { output });
                }
                None => self.fail("No screen found to share.".into()),
            },
            Session::X11 => {
                match crate::inject::X11::new(None) {
                    Ok(x) => *self.injector.lock().unwrap() = Some(Injector::X11(Box::new(x))),
                    Err(e) => tracing::warn!("no mouse and keyboard control: {e}"),
                }
                *self.screen.lock().unwrap() = Some(Screen::X11);
            }
        }
    }

    fn output(&self, extend_id: Option<u64>) -> encode::Output {
        let host = self.host.get().cloned().unwrap_or_default();
        Arc::new(move |f: EncodedFrame<'_>| {
            if let Some(h) = host.upgrade() {
                match extend_id {
                    Some(id) => {
                        h.submit_session_frame(id, f.data, f.capture_us, f.keyframe);
                    }
                    None => {
                        h.submit_frame(f.data, f.capture_us, f.keyframe);
                    }
                }
            }
        })
    }

    fn start_mirror(self: &Arc<Self>) {
        // The desktop may still be asking the user.
        let deadline = Instant::now() + Duration::from_secs(120);
        while self.screen.lock().unwrap().is_none() {
            if self.screen_failed.load(Ordering::Acquire) || Instant::now() > deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut slot = self.mirror.lock().unwrap();
        if let Some(p) = slot.as_ref() {
            // Another mirror viewer joined: one capture feeds everyone; just resync.
            p.pipeline().request_keyframe();
            return;
        }
        // The last viewer may have left before this thread got here.
        if self.mirror_sessions.lock().unwrap().is_empty() {
            return;
        }
        let StreamConfig { width, height, fps, .. } = self.stream;
        let source = match self.screen.lock().unwrap().as_ref() {
            Some(Screen::Portal(p)) => Source::Gst(p.source()),
            Some(Screen::Gnome(g)) => Source::Gst(g.source(None)),
            Some(Screen::Wlr { output }) => Source::Screencopy(output.clone()),
            Some(Screen::X11) => {
                Source::Gst(format!("ximagesrc use-damage=false show-pointer=true ! video/x-raw,framerate={fps}/1"))
            }
            None => return,
        };
        match self.run(source, width, height, fps, self.output(None)) {
            Ok(r) => *slot = Some(r),
            Err(e) => {
                drop(slot);
                self.fail(format!("Screen capture didn't start: {e}"));
            }
        }
    }

    fn run(
        &self,
        source: Source,
        width: u32,
        height: u32,
        fps: u32,
        output: encode::Output,
    ) -> Result<Running, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let bitrate = encode::bitrate(width, height, fps);
        let cfg = |source: String| encode::Config {
            source,
            width,
            height,
            fps,
            bitrate,
            prefer_hardware: self.prefer_hardware,
        };
        let pipeline = match source {
            Source::Gst(s) => Pipeline::start(cfg(s), output)?,
            Source::Screencopy(name) => {
                let sc = Screencopy::open(&name)?;
                let src = format!("appsrc name=screen is-live=true format=time do-timestamp=true caps={}", sc.caps());
                let stop2 = stop.clone();
                Pipeline::start_with(cfg(src), output, move |p| {
                    use gst::prelude::*;
                    if let Some(app) = p.by_name("screen").and_then(|e| e.downcast::<gst_app::AppSrc>().ok()) {
                        std::thread::Builder::new()
                            .name("annex-screencopy".into())
                            .spawn(move || sc.run(app, fps, stop2))
                            .ok();
                    }
                })?
            }
        };
        Ok(Running { _pipeline: pipeline, stop })
    }

    fn start_extend(self: &Arc<Self>, id: u64, display: DisplayInfo) {
        let Some(host) = self.host() else { return };
        let hz = if display.refresh_hz == 0 { 60 } else { display.refresh_hz.clamp(24, 120) };
        // The display runs at the viewer's rate; the software encoders stream it at 30 fps.
        let rate = if encode::hardware(self.prefer_hardware) { hz } else { hz.min(30) };
        let (w, h) = stream_size(display.width_px.max(640), display.height_px.max(480), MAX_H264.0, MAX_H264.1);
        let (w, h) = (w & !1, h & !1);
        let (stop, running, injector) = {
            let ext = self.extends.lock().unwrap();
            let Some(s) = ext.get(&id) else { return };
            (s.stop.clone(), s.running.clone(), s.injector.clone())
        };
        let fail = |e: String| {
            tracing::error!("extend session {id} failed: {e}");
            host.end_session(id);
        };

        let source = match self.session {
            Session::Gnome => match self.rt.block_on(crate::gnome::start(true)) {
                Ok(g) => {
                    // Mutter paints a new virtual monitor (and so sends its first frame) only
                    // once something changes on it: the pointer arriving does.
                    let _ =
                        g.input.send(crate::portal::PortalInput::Motion { x: f64::from(w / 2), y: f64::from(h / 2) });
                    *injector.lock().unwrap() =
                        Some(Injector::Portal(crate::inject::Portal::new(g.input.clone(), (w as i32, h as i32))));
                    let src = g.source(Some((w, h)));
                    if let Some(s) = self.extends.lock().unwrap().get_mut(&id) {
                        s.gnome = Some(g);
                    }
                    Source::Gst(src)
                }
                Err(e) => return fail(e),
            },
            Session::Kde => match self.rt.block_on(crate::portal::start(None, true)) {
                Ok(p) => {
                    if let Some(input) = &p.input {
                        *injector.lock().unwrap() =
                            Some(Injector::Portal(crate::inject::Portal::new(input.clone(), (w as i32, h as i32))));
                    }
                    let src = format!("{} ! video/x-raw,width={w},height={h}", p.source());
                    if let Some(s) = self.extends.lock().unwrap().get_mut(&id) {
                        s.portal = Some(p);
                    }
                    Source::Gst(src)
                }
                Err(e) => return fail(e),
            },
            Session::Wlroots(kind) => match crate::extend::add(kind, w, h, hz) {
                Ok(d) => {
                    match crate::inject::Wlr::new(Some(&d.output)) {
                        Ok(i) => *injector.lock().unwrap() = Some(Injector::Wlr(Box::new(i))),
                        Err(e) => tracing::warn!("extend: no mouse and keyboard: {e}"),
                    }
                    let out = d.output.clone();
                    match self.extends.lock().unwrap().get_mut(&id) {
                        Some(s) => s.display = Some(d),
                        // Ended while the display came up.
                        None => {
                            d.remove();
                            return;
                        }
                    }
                    Source::Screencopy(out)
                }
                Err(e) => return fail(e),
            },
            Session::OtherWayland | Session::X11 => return fail("this desktop can't add a display".into()),
        };

        // Follow the display's size (screencopy: the output's; elsewhere: what was asked for).
        let mut size = (w, h);
        if let Source::Screencopy(name) = &source
            && let Ok(sc) = Screencopy::open(name)
        {
            size = sc.size();
        }
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let (sw, sh) = stream_size(size.0, size.1, MAX_H264.0, MAX_H264.1);
            host.set_session_stream(
                id,
                StreamConfig { codec: Codec::H264, width: sw & !1, height: sh & !1, fps: rate },
            );
            match self.run(source.clone(), sw, sh, rate, self.output(Some(id))) {
                Ok(r) => {
                    tracing::info!(id, sw, sh, "extend: capturing the new display");
                    *running.lock().unwrap() = Some(r);
                }
                Err(e) => return fail(e),
            }
            // Screencopy stops when the output is resized (Settings → Screen Configuration).
            let Source::Screencopy(name) = &source else { break };
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let now = crate::wl::Wayland::connect()
                    .ok()
                    .and_then(|wl| wl.output(name).map(|o| (o.size.0.max(2) as u32, o.size.1.max(2) as u32)));
                match now {
                    Some(s) if s != size => {
                        running.lock().unwrap().take();
                        size = s;
                        break;
                    }
                    Some(_) => {}
                    None => return fail("the display went away".into()),
                }
            }
        }
        // A pipeline started after end_extend took the last one.
        running.lock().unwrap().take();
    }

    fn end_extend(&self, id: u64) {
        let Some(s) = self.extends.lock().unwrap().remove(&id) else { return };
        s.stop.store(true, Ordering::Release);
        s.running.lock().unwrap().take();
        if let Some(d) = &s.display {
            // The desktop moves the display's windows back to the others.
            d.remove();
        }
        // GNOME/KDE: dropping the session removes the virtual monitor.
    }

    fn stop_all(&self) {
        self.mirror.lock().unwrap().take();
        let ids: Vec<u64> = self.extends.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.end_extend(id);
        }
        self.screen.lock().unwrap().take();
        self.injector.lock().unwrap().take();
    }

    fn on_event(self: &Arc<Self>, e: &Event) {
        match e {
            Event::SessionStarted { session_id, mode: Mode::Mirror, .. } => {
                self.mirror_sessions.lock().unwrap().insert(*session_id);
                let me = self.clone();
                std::thread::spawn(move || me.start_mirror());
            }
            Event::SessionStarted { session_id, mode: Mode::Extend, viewer_display, .. } => {
                // Registered now, before SessionEnded can run end_extend.
                self.extends.lock().unwrap().insert(*session_id, ExtendSession::default());
                let me = self.clone();
                let id = *session_id;
                let display = viewer_display.clone().unwrap_or(DisplayInfo {
                    width_px: 1920,
                    height_px: 1080,
                    width_mm: 0,
                    height_mm: 0,
                    refresh_hz: 60,
                    scale_percent: 100,
                    refresh_rates: vec![],
                });
                std::thread::spawn(move || me.start_extend(id, display));
            }
            Event::SessionEnded { session_id, .. } => {
                if self.mirror_sessions.lock().unwrap().remove(session_id) {
                    self.mirror_scales.lock().unwrap().remove(session_id);
                    if self.mirror_sessions.lock().unwrap().is_empty() {
                        self.mirror.lock().unwrap().take();
                    }
                } else {
                    let me = self.clone();
                    let id = *session_id;
                    std::thread::spawn(move || me.end_extend(id));
                }
            }
            Event::BitrateHint { session_id, scale_percent } => {
                let extend = self.extends.lock().unwrap().get(session_id).map(|e| e.running.clone());
                match extend {
                    Some(r) => {
                        if let Some(r) = r.lock().unwrap().as_ref() {
                            r.pipeline().set_bitrate_scale(*scale_percent);
                        }
                    }
                    None => {
                        let lowest = {
                            let mut scales = self.mirror_scales.lock().unwrap();
                            scales.insert(*session_id, *scale_percent);
                            scales.values().copied().min().unwrap_or(100)
                        };
                        if let Some(r) = self.mirror.lock().unwrap().as_ref() {
                            r.pipeline().set_bitrate_scale(lowest);
                        }
                    }
                }
            }
            Event::KeyframeRequested { session_id } => {
                let extend = self.extends.lock().unwrap().get(session_id).map(|e| e.running.clone());
                match extend {
                    Some(r) => {
                        if let Some(r) = r.lock().unwrap().as_ref() {
                            r.pipeline().request_keyframe();
                        }
                    }
                    None => {
                        if let Some(r) = self.mirror.lock().unwrap().as_ref() {
                            r.pipeline().request_keyframe();
                        }
                    }
                }
            }
            Event::Input { session_id, event } => {
                let extend = self.extends.lock().unwrap().get(session_id).map(|e| e.injector.clone());
                match extend {
                    Some(i) => {
                        if let Some(i) = i.lock().unwrap().as_mut() {
                            i.inject(*event);
                        }
                    }
                    None => {
                        if let Some(i) = self.injector.lock().unwrap().as_mut() {
                            i.inject(*event);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone)]
enum Source {
    Gst(String),
    Screencopy(String),
}
