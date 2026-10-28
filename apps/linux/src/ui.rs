//! The main window, in the Mac app's look: near-black, a floating sidebar with the app's name and
//! icons (Nearby, Share This Screen, Paired Devices, then Settings and About at the bottom), and
//! pages made of small uppercase section labels over rounded groups of rows divided by hairlines.
//! White is the emphasis; green only marks something live.
//!
//! Pages keep their widgets; [`render`] refreshes only what changes, so typing in a field isn't
//! interrupted.

use std::cell::RefCell;
use std::collections::HashMap;

use annex_update::whats_new::{self, Release};
use gtk::prelude::*;
use gtk::{gio, glib};

use crate::model::{Change, InternetShare, Model, SessionRole, Shared, UpdateState};
use crate::settings_keys::Effect;

const REPO: &str = "https://github.com/NRanjan-17/Annex";

/// The sidebar: page name, title, symbolic icon. The first three at the top, the rest at the bottom.
const PAGES: [(&str, &str, &str); 5] = [
    ("home", "Nearby", "network-wireless-symbolic"),
    ("share", "Share This Screen", "computer-symbolic"),
    ("devices", "Paired Devices", "avatar-default-symbolic"),
    ("settings", "Settings", "emblem-system-symbolic"),
    ("about", "About", "help-about-symbolic"),
];

/// The update banner at the top of Nearby and Share.
struct UpdateBanner {
    root: gtk::Box,
    text: gtk::Label,
}

struct Ui {
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    nav: Vec<(&'static str, gtk::Button)>,
    /// Nearby: how many computers are sharing.
    nav_badge: gtk::Label,
    /// Share: a green dot while sharing.
    nav_live: gtk::Box,
    banner: gtk::Label,
    update_banners: Vec<UpdateBanner>,
    // Home
    live: gtk::Box,
    /// Delay, smoothness and picture size in the live card (updated without rebuilding it).
    live_stats: Vec<gtk::Label>,
    same_name: gtk::Box,
    status: gtk::Label,
    waiting: gtk::Box,
    nearby_section: gtk::Box,
    nearby: gtk::Box,
    /// Connect by address or code: stops a connection that hasn't started yet.
    cancel_connect: gtk::Button,
    // Share
    share_switch: gtk::Switch,
    share_title: gtk::Label,
    share_note: gtk::Label,
    sessions_section: gtk::Box,
    sessions: gtk::Box,
    code: gtk::Box,
    /// "On the same network": this computer's name and addresses, while sharing.
    local_section: gtk::Box,
    local: gtk::Box,
    local_footer: gtk::Label,
    // Paired devices
    devices: gtk::Box,
    own_identity: gtk::Label,
    // About
    about_details: gtk::Label,
    update_spinner: gtk::Spinner,
    update_icon: gtk::Image,
    update_text: gtk::Label,
    update_download: gtk::Button,
    check_now: gtk::Button,
    last_checked: gtk::Label,
    /// Open PIN windows, by request.
    pins: HashMap<u64, gtk::Window>,
    /// The last render's lists, so unchanged lists aren't rebuilt.
    last: HashMap<&'static str, String>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    /// The desktop's inhibit cookie while a session keeps this computer awake (0: refused).
    static KEEP_AWAKE: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    /// "What's new" waiting for the window (Annex started in the tray).
    static WHATS_NEW: RefCell<Vec<Release>> = const { RefCell::new(Vec::new()) };
}

/// Scoped to Annex's own windows (`.annex`, `.annex-content`): the drop island and the video
/// window have their own styles.
const CSS: &str = "
window.annex {
  background-color: #050505;
  background-image: radial-gradient(circle farthest-side at 28% 16%, rgba(255, 255, 255, 0.075), transparent),
    radial-gradient(circle farthest-side at 82% 90%, rgba(255, 255, 255, 0.045), transparent);
  color: #f2f2f2;
}
.annex-content scrolledwindow, .annex-content viewport, .annex-content stack { background: none; }
.annex-content { color: #f2f2f2; }

/* Sidebar: a floating glass panel. */
.annex-content .sidebar {
  margin: 10px; padding: 8px; border-radius: 22px;
  background-color: rgba(255, 255, 255, 0.05); border: 1px solid rgba(255, 255, 255, 0.09);
}
.annex-content .app-header { margin: 6px 8px 14px 8px; }
.annex-content .app-name { font-family: 'SF Pro Rounded', 'Nunito', 'Quicksand', 'Cantarell', sans-serif;
  font-size: 15px; font-weight: 600; }
.annex-content button.nav {
  background: none; border: none; box-shadow: none; border-radius: 10px;
  padding: 0 10px; min-height: 32px; color: #e6e6e6;
}
.annex-content button.nav image { opacity: 0.7; }
.annex-content button.nav:hover { background-color: rgba(255, 255, 255, 0.06); }
.annex-content button.nav.selected {
  background-color: rgba(255, 255, 255, 0.92); color: black; box-shadow: 0 0 12px rgba(255, 255, 255, 0.18);
}
.annex-content button.nav.selected image { opacity: 1; }
.annex-content button.nav.selected label { font-weight: 600; }
.annex-content .nav-badge { font-size: 10.5px; font-weight: 700; opacity: 0.7; }

/* Pages. */
.annex-content .page { padding: 28px; }
.annex-content .hero-title {
  font-family: 'SF Pro Rounded', 'Nunito', 'Quicksand', 'Cantarell', sans-serif;
  font-size: 28px; font-weight: 600; color: white;
}
.annex-content .eyebrow { font-size: 10.5px; font-weight: 600; letter-spacing: 1.2px; color: rgba(255, 255, 255, 0.55); }
.annex-content .group {
  background-color: rgba(255, 255, 255, 0.045); border: 1px solid rgba(255, 255, 255, 0.08); border-radius: 14px;
}
.annex-content .row { padding: 10px 16px; min-height: 24px; }
.annex-content separator.row-sep { background-color: rgba(255, 255, 255, 0.08); min-height: 1px; margin: 0 16px; }
.annex-content .row-title { color: #f2f2f2; }
.annex-content .strong { font-weight: 600; }
.annex-content .dim, .annex-content .subtitle { color: rgba(255, 255, 255, 0.58); }
.annex-content .subtitle { font-size: 0.93em; }
.annex-content .footnote { font-size: 0.85em; color: rgba(255, 255, 255, 0.45); }
.annex-content .ok { color: #3dd185; }
.annex-content .warn { color: #f0b429; }

/* Controls: clear glass; the main action white. */
.annex-content button {
  background-image: none; background-color: rgba(255, 255, 255, 0.08); border: 1px solid rgba(255, 255, 255, 0.10);
  box-shadow: none; border-radius: 9px; color: #f2f2f2; padding: 4px 12px;
}
.annex-content button:hover { background-color: rgba(255, 255, 255, 0.14); }
.annex-content button:disabled { opacity: 0.45; }
.annex-content button.suggested-action { background-color: rgba(255, 255, 255, 0.92); color: black; border-color: transparent; }
.annex-content button.suggested-action:hover { background-color: white; }
.annex-content button.destructive-action { color: #ff7b72; }
.annex-content button.link-button { background: none; border: none; color: rgba(255, 255, 255, 0.75); padding: 4px 6px; }
.annex-content button.link-button:hover { color: white; }
.annex-content entry {
  background-image: none; background-color: rgba(255, 255, 255, 0.06); border: 1px solid rgba(255, 255, 255, 0.10);
  box-shadow: none; border-radius: 9px; color: #f2f2f2; min-height: 30px;
}
/* Switches drawn from scratch: themes like Mint-Y paint them with images that would show through. */
.annex-content switch {
  background-image: none; background-color: rgba(255, 255, 255, 0.14); border: 1px solid rgba(255, 255, 255, 0.10);
  box-shadow: none; outline: none; border-radius: 13px; min-width: 44px; min-height: 24px; padding: 0;
}
.annex-content switch image { opacity: 0; }
.annex-content switch slider {
  background-image: none; background-color: #f2f2f2; border: none; outline: none;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.45); border-radius: 11px; min-width: 20px; min-height: 20px; margin: 2px;
}
.annex-content switch:checked { background-color: rgba(255, 255, 255, 0.92); border-color: transparent; }
.annex-content switch:checked slider { background-color: #141414; box-shadow: none; }
.annex-content switch:disabled { opacity: 0.45; }

/* Something live: the only colour. */
.annex-content .live-dot { min-width: 8px; min-height: 8px; border-radius: 4px; background-color: #3dd185;
  animation: annex-breathe 1.6s ease-in-out infinite; }
.annex-content .peer-dot { min-width: 7px; min-height: 7px; border-radius: 4px; background-color: #3dd185; }
.annex-content .peer-dot.off { background-color: rgba(255, 255, 255, 0.25); }
@keyframes annex-breathe { 0% { opacity: 1; } 50% { opacity: 0.35; } 100% { opacity: 1; } }
.annex-content .live {
  padding: 16px 22px; border-radius: 26px;
  background-color: rgba(255, 255, 255, 0.06); border: 1px solid rgba(255, 255, 255, 0.12);
}
.annex-content .live-eyebrow { font-size: 10.5px; font-weight: 700; letter-spacing: 1.4px; color: #3dd185; }
.annex-content .live-flow { font-family: 'SF Pro Rounded', 'Nunito', 'Quicksand', 'Cantarell', sans-serif;
  font-size: 18px; font-weight: 600; }
.annex-content .stat-value { font-size: 34px; font-weight: 300; font-feature-settings: 'tnum'; }
.annex-content .stat-unit { font-size: 13px; font-weight: 500; color: rgba(255, 255, 255, 0.58); }
.annex-content .stat-label { font-size: 11px; color: rgba(255, 255, 255, 0.58); }

/* Waiting for a computer. */
.annex-content .waiting {
  padding: 30px 16px; border-radius: 18px;
  background-color: rgba(255, 255, 255, 0.03); border: 1px dashed rgba(255, 255, 255, 0.12);
}
.annex-content .waiting-title { font-family: 'SF Pro Rounded', 'Nunito', 'Quicksand', 'Cantarell', sans-serif;
  font-size: 17px; font-weight: 600; }

.annex-content .about-title { font-size: 22px; font-weight: 700; }
.annex-content .about-card { padding: 28px; }
.annex-content .dialog-title { font-size: 15px; font-weight: 700; }
.annex-content .role { font-size: 15px; font-weight: 700; }
.annex-content .version { font-family: 'SF Pro Rounded', 'Nunito', 'Quicksand', 'Cantarell', sans-serif;
  font-size: 17px; font-weight: 600; margin-top: 8px; }
.annex-content .code { font-family: monospace; font-size: 28px; font-weight: 700; letter-spacing: 4px; }
.annex-content .pin { font-family: monospace; font-size: 40px; font-weight: 700; letter-spacing: 8px; }
.annex-content .mono { font-family: monospace; }
.annex-content .banner { padding: 8px 28px; background-color: rgba(229, 165, 10, 0.18); color: #f5d488; }
.video-window, .video { background: black; }
";

pub fn load_css() {
    // The Mac app is always dark; so is this one, whatever the desktop's theme.
    if let Some(s) = gtk::Settings::default() {
        s.set_gtk_application_prefer_dark_theme(true);
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

/// Runs `f` on the model, then acts on what changed once the borrow is over.
pub fn act(app: &gtk::Application, model: &Shared, f: impl FnOnce(&mut Model)) {
    f(&mut model.borrow_mut());
    apply(app, model);
}

/// Acts on the model's queued changes (the model never calls the UI while borrowed).
pub fn apply(app: &gtk::Application, model: &Shared) {
    loop {
        let changes = model.borrow_mut().take_changes();
        if changes.is_empty() {
            break;
        }
        for c in changes {
            match c {
                Change::Render => {
                    keep_awake(app, model);
                    render(model);
                    crate::tray::refresh(&model.borrow());
                }
                Change::PinRequired { request_id, peer_name, fingerprint } => {
                    if model.borrow().cli_viewer {
                        // `annex connect`: the PIN is typed with `annex pin enter`.
                        tracing::info!(target: "annex::pairing", request_id, peer = peer_name, "pairing: PIN asked for on the command line");
                    } else {
                        ask_pin(app, model, request_id, &peer_name, &fingerprint)
                    }
                }
                Change::ShowPin { request_id, pin, peer_name, fingerprint } => {
                    show_pin(app, model, request_id, &pin, &peer_name, &fingerprint)
                }
                Change::PinDone { request_id } => {
                    tracing::debug!(target: "annex::pairing", request_id, "pairing: done; closing its window");
                    if let Some(w) = with_ui(|ui| ui.pins.remove(&request_id)).flatten() {
                        w.destroy();
                    }
                }
                Change::InputRequested { peer_name, fingerprint } => ask_input(app, model, &peer_name, &fingerprint),
                Change::Notify { title, body } => {
                    tracing::debug!(title, "ui: notification");
                    let n = gio::Notification::new(&title);
                    n.set_body(Some(&body));
                    app.send_notification(Some("session"), &n);
                }
                Change::ViewerOpen => crate::viewer::open(app, model),
                Change::ViewerClose => crate::viewer::close(),
                Change::ViewerStream { width, height } => crate::viewer::stream_size(width, height),
            }
        }
    }
}

/// Held while a session runs (viewing, or this computer shown to someone), so neither screen
/// sleeps: the viewer gets no local input, and the host's screen is what the viewer watches.
/// Also held while sharing with no one at this screen: a locked screen can't be shared (GNOME
/// refuses), and no one is there to unlock it.
fn keep_awake(app: &gtk::Application, model: &Shared) {
    let Ok(m) = model.try_borrow() else { return };
    let active = m.viewer.is_some() || !m.host_sessions.is_empty() || (m.settings.headless && m.is_hosting());
    drop(m);
    KEEP_AWAKE.with(|k| match (active, k.get()) {
        (true, None) => {
            let cookie = app.inhibit(
                None::<&gtk::Window>,
                gtk::ApplicationInhibitFlags::IDLE | gtk::ApplicationInhibitFlags::SUSPEND,
                Some("An Annex session is running"),
            );
            if cookie == 0 {
                tracing::warn!(target: "annex::session", "session: couldn't keep this computer awake (the desktop refused)");
            } else {
                tracing::info!(target: "annex::session", cookie, "session: keeping this computer awake while the session runs");
            }
            // Held even when refused, so it isn't asked again on every redraw.
            k.set(Some(cookie));
        }
        (false, Some(cookie)) => {
            if cookie != 0 {
                app.uninhibit(cookie);
            }
            tracing::info!(target: "annex::session", "session: no session; this computer may sleep again");
            k.set(None);
        }
        _ => {}
    });
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| u.borrow_mut().as_mut().map(f))
}

pub fn present(app: &gtk::Application, model: &Shared) {
    if with_ui(|ui| ui.window.present()).is_none() {
        build(app, model);
        with_ui(|ui| ui.window.present());
    }
    let notes = WHATS_NEW.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !notes.is_empty() {
        let (app, model) = (app.clone(), model.clone());
        // Once the window is up, so the dialog sits on it.
        glib::idle_add_local_once(move || {
            tracing::info!(target: "annex::update", "update: showing what's new for {}", notes[0].version);
            show_whats_new(&app, &notes);
            mark_seen(&model);
        });
    }
}

/// After an update, the new releases' notes show once, with the window. A first install shows
/// nothing and only remembers this version.
pub fn whats_new_at_launch(model: &Shared, used_before: bool) {
    let current = whats_new::CURRENT;
    let saved = model.borrow().settings.last_seen_version.clone();
    // 0.3.0 didn't save it: Annex already used here means an update from 0.3.0.
    let last_seen = saved.clone().or_else(|| used_before.then(|| "0.3.0".to_string()));
    let notes = whats_new::since(last_seen.as_deref(), current);
    tracing::info!(
        target: "annex::update",
        current,
        last_seen = last_seen.as_deref().unwrap_or("none (first install)"),
        releases = notes.len(),
        "update: what's new"
    );
    if !notes.is_empty() {
        WHATS_NEW.with(|w| *w.borrow_mut() = notes);
    } else if saved.as_deref() != Some(current) {
        mark_seen(model);
    }
}

fn mark_seen(model: &Shared) {
    let mut m = model.borrow_mut();
    m.settings.last_seen_version = Some(whats_new::CURRENT.to_string());
    m.save_settings();
}

/// Shows the window at a page: "home", "share", "devices", "settings" or "about".
pub fn show_pane(app: &gtk::Application, model: &Shared, page: &str) {
    present(app, model);
    // Outside `with_ui`: the stack's notify handler reads the UI too.
    if let Some(stack) = with_ui(|ui| ui.stack.clone()) {
        stack.set_visible_child_name(page);
    }
}

// ---- Building blocks -----------------------------------------------------------------------
//
// One design language, the Mac's: a title and a line on what the page is for, then sections,
// each a small uppercase label over one rounded group of full-width rows divided by hairlines.
// Rows carry text on the left and one control on the right.

fn label(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_wrap(true);
    if !class.is_empty() {
        l.add_css_class(class);
    }
    l
}

/// A label that stays on one line (sidebar, buttons, numbers).
fn plain(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    if !class.is_empty() {
        l.add_css_class(class);
    }
    l
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

fn spacer() -> gtk::Box {
    let s = hbox(0);
    s.set_hexpand(true);
    s
}

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

fn page(content: &gtk::Box) -> gtk::ScrolledWindow {
    content.add_css_class("page");
    gtk::ScrolledWindow::builder().child(content).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build()
}

/// The top of a page: its title and one line on what it's for.
fn hero(title: &str, text: &str) -> gtk::Box {
    let b = vbox(4);
    b.append(&label(title, "hero-title"));
    b.append(&label(text, "dim"));
    b
}

/// A small uppercase label over a group.
fn eyebrow(text: &str) -> gtk::Label {
    let l = plain(&text.to_uppercase(), "eyebrow");
    l.set_margin_start(4);
    l
}

/// A section: an optional label, one rounded group (returned second, for rows), a footnote.
fn section(title: Option<&str>, footer: Option<&str>) -> (gtk::Box, gtk::Box) {
    let outer = vbox(8);
    if let Some(t) = title {
        outer.append(&eyebrow(t));
    }
    let group = vbox(0);
    group.add_css_class("group");
    outer.append(&group);
    if let Some(f) = footer {
        let l = label(f, "footnote");
        l.set_margin_start(4);
        l.set_margin_end(4);
        l.set_selectable(true);
        outer.append(&l);
    }
    (outer, group)
}

/// Adds a row to a group, with a hairline above it unless it's the first.
fn add_row(group: &gtk::Box, row: &impl IsA<gtk::Widget>) {
    if group.first_child().is_some() {
        let s = gtk::Separator::new(gtk::Orientation::Horizontal);
        s.add_css_class("row-sep");
        group.append(&s);
    }
    group.append(row);
}

/// A row: a title and an optional subtitle.
fn row(title: &str, subtitle: Option<&str>) -> gtk::Box {
    let r = hbox(12);
    r.add_css_class("row");
    let text = vbox(2);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    text.append(&label(title, "row-title"));
    if let Some(s) = subtitle {
        text.append(&label(s, "subtitle"));
    }
    r.append(&text);
    r
}

/// A row with one control on its right.
fn row_with(title: &str, subtitle: Option<&str>, trailing: &impl IsA<gtk::Widget>) -> gtk::Box {
    let r = row(title, subtitle);
    trailing.set_valign(gtk::Align::Center);
    r.append(trailing);
    r
}

/// A cell of a group for anything that isn't a title-and-control row.
fn cell(spacing: i32) -> gtk::Box {
    let c = vbox(spacing);
    c.add_css_class("row");
    c
}

type Toggle = Box<dyn Fn(&mut Model, bool)>;

/// A row with a switch on its right; `f` applies it, then the settings are saved.
fn switch_row(
    app: &gtk::Application,
    model: &Shared,
    group: &gtk::Box,
    title: &str,
    subtitle: Option<&str>,
    on: bool,
    f: Toggle,
) -> gtk::Switch {
    let sw = gtk::Switch::new();
    sw.set_active(on);
    let (app, model, name) = (app.clone(), model.clone(), title.to_string());
    sw.connect_state_set(move |_, on| {
        tracing::info!(target: "annex::settings", setting = name, on, "settings: switched");
        act(&app, &model, |m| {
            f(m, on);
            m.save_settings();
        });
        glib::Propagation::Proceed
    });
    add_row(group, &row_with(title, subtitle, &sw));
    sw
}

/// The app's icon (installed with the package), or a computer where it isn't installed.
fn app_icon(size: i32) -> gtk::Image {
    let installed = gtk::gdk::Display::default().is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon("annex"));
    let img = gtk::Image::from_icon_name(if installed { "annex" } else { "computer-symbolic" });
    img.set_pixel_size(size);
    img
}

fn live_dot() -> gtk::Box {
    let d = hbox(0);
    d.add_css_class("live-dot");
    d.set_valign(gtk::Align::Center);
    d.set_halign(gtk::Align::Center);
    d
}

fn open_uri(uri: &str) {
    tracing::info!(target: "annex::ui", uri, "ui: opening a link");
    if let Err(e) = gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>) {
        tracing::warn!(target: "annex::ui", uri, "ui: couldn't open a link: {e}");
    }
}

/// "Copy", which puts `text` on the clipboard and says "Copied". `what` is for the log (never the
/// text itself: it may be a code).
fn copy_button(text: &str, what: &'static str) -> gtk::Button {
    let b = gtk::Button::with_label("Copy");
    let text = text.to_string();
    b.connect_clicked(move |b| {
        tracing::info!(target: "annex::ui", "ui: Copy pressed ({what})");
        match gtk::gdk::Display::default() {
            Some(d) => {
                d.clipboard().set_text(&text);
                b.set_label("Copied");
            }
            None => tracing::warn!(target: "annex::ui", "ui: no display to copy to"),
        }
    });
    b
}

/// "Works for m:ss" until `expires`, then "Expired.".
fn code_time_left(expires: std::time::Instant) -> String {
    let left = expires.saturating_duration_since(std::time::Instant::now()).as_secs();
    if left == 0 {
        "Expired.".into()
    } else {
        format!(
            "Type it on the other computer, in Nearby → Connect by address or code. Works for {}:{:02}.",
            left / 60,
            left % 60
        )
    }
}

fn link_button(text: &str, uri: &'static str) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    b.connect_clicked(move |_| open_uri(uri));
    b
}

// ---- The window ----------------------------------------------------------------------------

fn build(app: &gtk::Application, model: &Shared) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Annex")
        .default_width(940)
        .default_height(640)
        .build();
    window.add_css_class("annex");
    // Closing the window keeps Annex running in the tray.
    window.set_hide_on_close(true);

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_hexpand(true);

    // ---- Sidebar ----
    let sidebar = vbox(2);
    sidebar.add_css_class("sidebar");
    sidebar.set_width_request(216);
    let header = hbox(9);
    header.add_css_class("app-header");
    header.append(&app_icon(26));
    header.append(&plain("Annex", "app-name"));
    sidebar.append(&header);
    let nav_badge = plain("", "nav-badge");
    nav_badge.set_visible(false);
    let nav_live = live_dot();
    nav_live.set_visible(false);
    let mut nav = Vec::new();
    for (i, (name, title, icon)) in PAGES.into_iter().enumerate() {
        if i == 3 {
            let gap = vbox(0);
            gap.set_vexpand(true);
            sidebar.append(&gap);
        }
        let content = hbox(10);
        let img = gtk::Image::from_icon_name(icon);
        img.set_pixel_size(16);
        content.append(&img);
        let text = plain(title, "");
        text.set_hexpand(true);
        content.append(&text);
        match name {
            "home" => content.append(&nav_badge),
            "share" => content.append(&nav_live),
            _ => {}
        }
        let b = gtk::Button::builder().child(&content).build();
        b.add_css_class("nav");
        let s = stack.clone();
        b.connect_clicked(move |_| s.set_visible_child_name(name));
        sidebar.append(&b);
        nav.push((name, b));
    }

    // ---- Home ----
    let home = vbox(22);
    home.append(&hero("Nearby", "Computers around you with Annex open. Show one's screen, or use it as a display."));
    let home_update = update_banner(model);
    home.append(&home_update.root);
    let live = vbox(0);
    let same_name = vbox(0);
    let status = label("", "dim");
    status.set_margin_start(4);
    home.append(&live);
    home.append(&same_name);
    home.append(&status);
    let waiting = waiting_card();
    home.append(&waiting);
    let (nearby_section, nearby) = section(Some("On the same network"), None);
    home.append(&nearby_section);

    let (connect_section, connect_group) = section(
        Some("Connect by address or code"),
        Some(
            "A code connects over the internet; the other computer gets one from Share This Screen → Share with a code. \
             Through a VPN like Tailscale or ZeroTier, type the address it gives.",
        ),
    );
    let entry = gtk::Entry::builder()
        .placeholder_text("192.168.1.20, name.local, or a code like K7M-4QX")
        .secondary_icon_name("edit-clear-symbolic")
        .secondary_icon_tooltip_text("Clear")
        .hexpand(true)
        .build();
    let connect_row = hbox(8);
    connect_row.add_css_class("row");
    connect_row.append(&entry);
    let show_btn = gtk::Button::with_label("Show Its Screen");
    let extend_btn = gtk::Button::with_label("Use as Display");
    let cancel_connect = gtk::Button::with_label("Cancel");
    cancel_connect.set_visible(false);
    connect_row.append(&show_btn);
    connect_row.append(&extend_btn);
    connect_row.append(&cancel_connect);
    add_row(&connect_group, &connect_row);
    home.append(&connect_section);
    entry.connect_icon_press(|e, _| e.set_text(""));
    {
        let (app, model) = (app.clone(), model.clone());
        // Typing again clears the last attempt's error.
        entry.connect_changed(move |_| {
            act(&app, &model, |m| {
                if m.viewer.is_none() && m.viewer_status.take().is_some() {
                    m.render();
                }
            })
        });
    }
    {
        let (app, model, entry) = (app.clone(), model.clone(), entry.clone());
        cancel_connect.connect_clicked(move |_| {
            tracing::info!(target: "annex::viewer", "ui: Cancel pressed while connecting");
            act(&app, &model, |m| m.disconnect());
            entry.set_text("");
        });
    }
    let connect_typed = {
        let (app, model, entry) = (app.clone(), model.clone(), entry.clone());
        move |widget: &gtk::Widget, extend: bool| {
            let text = entry.text().to_string();
            if text.trim().is_empty() {
                return;
            }
            let display = crate::viewer::display_info(Some(widget));
            act(&app, &model, |m| {
                if looks_like_code(&text) {
                    m.connect_code(&text, extend, display)
                } else {
                    m.connect_address(&text, extend, display)
                }
            });
            // Fresh for next time (a code works only once anyway).
            entry.set_text("");
        }
    };
    for (btn, extend) in [(show_btn, false), (extend_btn, true)] {
        let connect = connect_typed.clone();
        btn.connect_clicked(move |b| {
            tracing::info!(target: "annex::viewer", extend, "ui: {} pressed", if extend { "Use as Display" } else { "Show Its Screen" });
            connect(b.upcast_ref(), extend);
        });
    }
    {
        // Enter: the default action (Settings → Viewing → Extend display by default).
        let model = model.clone();
        entry.connect_activate(move |e| {
            let extend = model.borrow().settings.extend_by_default;
            tracing::info!(target: "annex::viewer", extend, "ui: Enter pressed in Connect by address or code");
            connect_typed(e.upcast_ref(), extend);
        });
    }

    // ---- Share ----
    let share = vbox(22);
    share.append(&hero("Share This Screen", "Let another computer show this screen, or use it as an extra display."));
    let share_update = update_banner(model);
    share.append(&share_update.root);
    let (share_main, share_group) = section(None, None);
    let share_switch = gtk::Switch::new();
    let share_row = hbox(12);
    share_row.add_css_class("row");
    let share_text = vbox(2);
    share_text.set_hexpand(true);
    let share_title = label("", "row-title");
    share_title.add_css_class("strong");
    let share_note = label("", "subtitle");
    share_text.append(&share_title);
    share_text.append(&share_note);
    share_row.append(&share_text);
    share_switch.set_valign(gtk::Align::Center);
    share_row.append(&share_switch);
    add_row(&share_group, &share_row);
    share.append(&share_main);
    let (sessions_section, sessions) = section(Some("Connected now"), None);
    sessions_section.set_visible(false);
    share.append(&sessions_section);
    let (code_section, code_group) = section(
        Some("Over the internet"),
        Some(
            "The server only introduces the two computers. The screen goes straight between them, \
             encrypted, or through a relay when a network blocks that.",
        ),
    );
    let code = cell(8);
    add_row(&code_group, &code);
    share.append(&code_section);
    let (local_section, local) = section(Some("On the same network"), None);
    let local_footer = label("", "footnote");
    local_footer.set_margin_start(4);
    local_footer.set_selectable(true);
    local_footer.set_visible(false);
    local_section.append(&local_footer);
    local_section.set_visible(false);
    share.append(&local_section);
    {
        let (app, model) = (app.clone(), model.clone());
        share_switch.connect_state_set(move |_, on| {
            // Rendering sets the switch too. Only a change the model doesn't already show is the
            // user's: queuing the others let a stale one undo a newer state, and two of them
            // flipped sharing on and off for good.
            let ours = model.try_borrow().map(|m| m.is_hosting() == on).unwrap_or(true);
            if ours {
                return glib::Propagation::Proceed;
            }
            tracing::debug!(target: "annex::host", on, "ui: Share This Screen switch");
            let (app, model) = (app.clone(), model.clone());
            // After the switch's own handler: hosting may open a portal dialog. Checked again then,
            // in case sharing changed meanwhile.
            glib::idle_add_local_once(move || {
                act(&app, &model, |m| {
                    if m.is_hosting() != on {
                        m.set_hosting(on)
                    }
                })
            });
            glib::Propagation::Proceed
        });
    }

    // ---- Paired devices ----
    let devices_page = vbox(22);
    devices_page.append(&hero(
        "Paired Devices",
        "Paired devices connect without a PIN. Only they can use this computer's mouse and keyboard, when allowed.",
    ));
    let devices = vbox(22);
    devices_page.append(&devices);
    let own_identity = label("", "footnote");
    own_identity.set_selectable(true);
    own_identity.set_margin_start(4);
    devices_page.append(&own_identity);

    // ---- Settings ----
    let settings = settings_page(app, model);

    // ---- About ----
    let about = about_page(app, model);

    stack.add_named(&page(&home), Some("home"));
    stack.add_named(&page(&share), Some("share"));
    stack.add_named(&page(&devices_page), Some("devices"));
    stack.add_named(&page(&settings), Some("settings"));
    stack.add_named(&page(&about.page), Some("about"));

    let banner = label("", "banner");
    banner.set_visible(false);
    let main = hbox(0);
    main.add_css_class("annex-content");
    main.append(&sidebar);
    let right = vbox(0);
    right.set_hexpand(true);
    right.append(&banner);
    right.append(&stack);
    main.append(&right);
    window.set_child(Some(&main));

    {
        let (model, entry) = (model.clone(), entry.clone());
        stack.connect_visible_child_name_notify(move |s| {
            let name = s.visible_child_name();
            // try_borrow: this can fire while the UI is being changed.
            UI.with(|u| {
                if let Ok(u) = u.try_borrow()
                    && let Some(ui) = u.as_ref()
                {
                    select_nav(ui, name.as_deref());
                }
            });
            if name.as_deref() == Some("home") {
                entry.grab_focus();
                if let Ok(m) = model.try_borrow() {
                    m.refresh_nearby();
                }
            }
        });
    }

    UI.with(|u| {
        *u.borrow_mut() = Some(Ui {
            window,
            stack,
            nav,
            nav_badge,
            nav_live,
            banner,
            update_banners: vec![home_update, share_update],
            live,
            live_stats: Vec::new(),
            same_name,
            status,
            waiting,
            nearby_section,
            nearby,
            cancel_connect,
            share_switch,
            share_title,
            share_note,
            sessions_section,
            sessions,
            code,
            local_section,
            local,
            local_footer,
            devices,
            own_identity,
            about_details: about.details,
            update_spinner: about.spinner,
            update_icon: about.icon,
            update_text: about.text,
            update_download: about.download,
            check_now: about.check_now,
            last_checked: about.last_checked,
            pins: HashMap::new(),
            last: HashMap::new(),
        })
    });
    with_ui(|ui| {
        let name = ui.stack.visible_child_name();
        select_nav(ui, name.as_deref());
    });
    render(model);
}

fn select_nav(ui: &Ui, page: Option<&str>) {
    for (name, b) in &ui.nav {
        if Some(*name) == page {
            b.add_css_class("selected");
        } else {
            b.remove_css_class("selected");
        }
    }
}

pub fn looks_like_code(s: &str) -> bool {
    crate::cli::looks_like_code(s)
}

/// "Annex X is available": Download opens the release page (the package manager installs it).
fn update_banner(model: &Shared) -> UpdateBanner {
    let (root, group) = section(None, None);
    root.set_visible(false);
    let r = hbox(10);
    r.add_css_class("row");
    let icon = gtk::Image::from_icon_name("software-update-available-symbolic");
    icon.set_pixel_size(16);
    r.append(&icon);
    let text = label("", "row-title");
    text.set_hexpand(true);
    r.append(&text);
    let not_now = gtk::Button::with_label("Not Now");
    not_now.set_valign(gtk::Align::Center);
    let download = gtk::Button::with_label("Download");
    download.add_css_class("suggested-action");
    download.set_valign(gtk::Align::Center);
    r.append(&not_now);
    r.append(&download);
    add_row(&group, &r);
    {
        let model = model.clone();
        not_now.connect_clicked(move |_| {
            tracing::info!(target: "annex::update", "update: banner closed (Not Now)");
            {
                let mut m = model.borrow_mut();
                let version = m.update_banner().map(|u| u.version.clone());
                m.update_dismissed = version;
            }
            render(&model);
        });
    }
    {
        let model = model.clone();
        download.connect_clicked(move |_| open_update_page(&model));
    }
    UpdateBanner { root, text }
}

fn open_update_page(model: &Shared) {
    let page = match &model.borrow().update {
        UpdateState::Available(u) => Some(u.page_url.clone()),
        _ => None,
    };
    if let Some(page) = page.filter(|p| !p.is_empty()) {
        tracing::info!(target: "annex::update", "update: Download pressed; opening the release page");
        open_uri(&page);
    }
}

/// Nobody found yet: rings widening out from this computer's icon, like the Mac's.
fn waiting_card() -> gtk::Box {
    let card = vbox(14);
    card.add_css_class("waiting");
    let rings = gtk::DrawingArea::new();
    rings.set_content_width(220);
    rings.set_content_height(150);
    rings.set_draw_func(|_, cr, w, h| {
        let t = glib::monotonic_time() as f64 / 1_000_000.0;
        let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
        cr.set_line_width(1.0);
        for i in 0..3_i32 {
            let p = (t / 2.4 + f64::from(i) / 3.0).fract();
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.28 * (1.0 - p));
            cr.arc(cx, cy, 36.0 + 45.0 * p, 0.0, std::f64::consts::TAU);
            let _ = cr.stroke();
        }
    });
    // Ticks only run while it's on screen.
    rings.add_tick_callback(|area, _| {
        area.queue_draw();
        glib::ControlFlow::Continue
    });
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&rings));
    let icon = app_icon(56);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    overlay.add_overlay(&icon);
    overlay.set_halign(gtk::Align::Center);
    card.append(&overlay);
    let title = plain("Looking for computers…", "waiting-title");
    title.set_xalign(0.5);
    card.append(&title);
    let text = label(
        "Open Annex on the other computer and turn on sharing. It shows up here when it's on the same network or \
         tailnet. On another network? Use a code below.",
        "dim",
    );
    text.set_xalign(0.5);
    text.set_justify(gtk::Justification::Center);
    text.set_max_width_chars(60);
    text.set_halign(gtk::Align::Center);
    card.append(&text);
    card
}

// ---- Settings ------------------------------------------------------------------------------

type SetText = fn(&mut Model, String);

/// The options most people change first, then everything else under Advanced, closed by default.
fn settings_page(app: &gtk::Application, model: &Shared) -> gtk::Box {
    let b = vbox(22);
    b.append(&hero("Settings", "How Annex starts, shares and shows other computers."));
    let s = model.borrow().settings.clone();

    // ---- The main options ----
    let (general, g) = section(None, None);
    let name_entry = gtk::Entry::builder()
        .text(s.device_name.clone().unwrap_or_default())
        .placeholder_text(crate::model::system_name())
        .width_chars(22)
        .build();
    let save = gtk::Button::with_label("Rename");
    let name_box = hbox(8);
    name_box.append(&name_entry);
    name_box.append(&save);
    add_row(
        &g,
        &row_with(
            "Device name",
            Some("Shown to other computers. Leave empty to use the computer's own name."),
            &name_box,
        ),
    );
    {
        let (app, model, e) = (app.clone(), model.clone(), name_entry.clone());
        let rename = move || {
            let text = e.text().to_string();
            act(&app, &model, |m| m.rename(&text));
        };
        let r = rename.clone();
        save.connect_clicked(move |_| r());
        name_entry.connect_activate(move |_| rename());
    }
    switch_row(
        app,
        model,
        &g,
        "Open at login",
        None,
        s.open_at_login,
        Box::new(|m, on| {
            m.settings.open_at_login = on;
            // Logs what went wrong.
            let _ = m.after_setting(Effect::OpenAtLogin);
        }),
    );
    switch_row(
        app,
        model,
        &g,
        "Share this screen when Annex opens",
        None,
        s.start_sharing_at_launch,
        Box::new(|m, on| m.settings.start_sharing_at_launch = on),
    );
    switch_row(
        app,
        model,
        &g,
        "No one at this screen",
        Some(
            "For a computer reached over SSH: pairing PINs also go to pairing-pin.txt in Annex's data folder, and \
             paired devices can control it without asking.",
        ),
        s.headless,
        Box::new(|m, on| {
            m.settings.headless = on;
            let _ = m.after_setting(Effect::Headless);
        }),
    );
    let quality = gtk::DropDown::from_strings(&["Best", "Balanced", "Fast (Wi-Fi)"]);
    quality.set_selected(s.quality.min(2));
    {
        let (app, model) = (app.clone(), model.clone());
        quality.connect_selected_notify(move |d| {
            let q = d.selected();
            tracing::info!(target: "annex::settings", quality = q, "settings: Quality");
            act(&app, &model, |m| {
                m.settings.quality = q;
                m.save_settings();
            });
        });
    }
    add_row(&g, &row_with("Quality", Some("Use Fast if the picture stutters on Wi-Fi."), &quality));
    switch_row(
        app,
        model,
        &g,
        "Control the other computer",
        Some("With this computer's mouse and keyboard, when it allows it."),
        s.use_mouse_and_keyboard,
        Box::new(|m, on| m.settings.use_mouse_and_keyboard = on),
    );
    b.append(&general);

    // ---- Advanced ----
    let chevron = gtk::Image::from_icon_name("pan-end-symbolic");
    let head = hbox(6);
    head.append(&chevron);
    head.append(&plain("Advanced", "strong"));
    head.append(&spacer());
    let hint = plain("You don't need to change these.", "dim");
    head.append(&hint);
    let advanced_btn = gtk::Button::builder().child(&head).build();
    advanced_btn.add_css_class("flat");
    advanced_btn.add_css_class("nav");
    let advanced = vbox(22);
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    revealer.set_child(Some(&advanced));
    {
        let revealer = revealer.clone();
        advanced_btn.connect_clicked(move |_| {
            let open = !revealer.reveals_child();
            tracing::debug!(target: "annex::settings", open, "settings: Advanced");
            revealer.set_reveal_child(open);
            chevron.set_icon_name(Some(if open { "pan-down-symbolic" } else { "pan-end-symbolic" }));
            hint.set_visible(!open);
        });
    }
    b.append(&advanced_btn);
    b.append(&revealer);

    // Sharing
    let (sharing, g) = section(Some("Sharing"), None);
    switch_row(
        app,
        model,
        &g,
        "Hardware video encoder",
        Some("Recommended: the graphics chip's encoder, lowest delay. Applies the next time sharing turns on."),
        s.prefer_hardware_encoder,
        Box::new(|m, on| {
            m.settings.prefer_hardware_encoder = on;
            let _ = m.after_setting(Effect::Render);
        }),
    );
    const SCALES: [u32; 6] = [0, 100, 125, 150, 175, 200];
    let scale = gtk::DropDown::from_strings(&["Match the other screen", "100 %", "125 %", "150 %", "175 %", "200 %"]);
    scale.set_selected(SCALES.iter().position(|&p| p == s.extend_scale).unwrap_or(0) as u32);
    {
        let (app, model) = (app.clone(), model.clone());
        scale.connect_selected_notify(move |d| {
            let p = SCALES[(d.selected() as usize).min(SCALES.len() - 1)];
            tracing::info!(target: "annex::settings", percent = p, "settings: Extended display scaling");
            act(&app, &model, |m| {
                m.settings.extend_scale = p;
                m.save_settings();
            });
        });
    }
    add_row(
        &g,
        &row_with(
            "Extended display scaling",
            Some("Text size on a computer using this one as an extra display. Turn sharing off and on to apply."),
            &scale,
        ),
    );
    advanced.append(&sharing);

    // Viewing
    let (viewing, g) = section(Some("Viewing"), None);
    switch_row(
        app,
        model,
        &g,
        "Open in full screen",
        None,
        s.open_full_screen,
        Box::new(|m, on| m.settings.open_full_screen = on),
    );
    switch_row(
        app,
        model,
        &g,
        "Extend display by default",
        Some(
            "Enter, and the first button by each computer in Nearby, use it as a display instead of showing its screen.",
        ),
        s.extend_by_default,
        Box::new(|m, on| {
            m.settings.extend_by_default = on;
            let _ = m.after_setting(Effect::Render);
        }),
    );
    advanced.append(&viewing);

    // Sound
    let (sound_section, g) = section(Some("Sound"), None);
    type Switch = (&'static str, &'static str, bool, fn(&mut crate::settings::Settings, bool));
    let sound: [Switch; 3] = [
        ("Share this computer's sound", "People viewing this computer hear what it plays.", s.share_sound, |s, on| {
            s.share_sound = on
        }),
        (
            "Hear the other computer",
            "Plays the shared computer's sound here, and a viewer's microphone.",
            s.hear_sound,
            |s, on| s.hear_sound = on,
        ),
        (
            "Send my microphone",
            "Your voice plays on the computer you're viewing. Annex takes its sound back out, so neither side hears an echo.",
            s.send_mic,
            |s, on| s.send_mic = on,
        ),
    ];
    let virtual_mic = annex_session::audio::virtual_microphone_here().then_some((
        "Use a viewer's microphone as Annex Microphone",
        "Apps here, like Zoom or Meet, can pick Annex Microphone. Off: it plays aloud.",
        s.virtual_mic,
        (|s, on| s.virtual_mic = on) as fn(&mut crate::settings::Settings, bool),
    ));
    for (text, caption, on, set) in sound.into_iter().chain(virtual_mic) {
        switch_row(
            app,
            model,
            &g,
            text,
            Some(caption),
            on,
            Box::new(move |m, on| {
                set(&mut m.settings, on);
                let _ = m.after_setting(Effect::Audio);
            }),
        );
    }
    advanced.append(&sound_section);

    // Clipboard and files
    let (files, g) = section(Some("Clipboard and files"), None);
    switch_row(
        app,
        model,
        &g,
        "Share the clipboard",
        Some("Copy on one computer, paste on the other, while one controls the other."),
        s.share_clipboard,
        Box::new(|m, on| {
            m.settings.share_clipboard = on;
            let _ = m.after_setting(Effect::Clipboard);
        }),
    );
    switch_row(
        app,
        model,
        &g,
        "Receive files",
        Some("Files the other computer sends go to Downloads."),
        s.receive_files,
        Box::new(|m, on| {
            m.settings.receive_files = on;
            let _ = m.after_setting(Effect::ReceiveFiles);
        }),
    );
    switch_row(
        app,
        model,
        &g,
        "Drop files at the top of the screen",
        Some("While connected, drag files to the top of the screen to send them."),
        s.drop_at_top,
        Box::new(|m, on| {
            m.settings.drop_at_top = on;
            let _ = m.after_setting(Effect::DropIsland);
        }),
    );
    advanced.append(&files);

    // Internet
    let (internet, g) = section(
        Some("Internet"),
        Some(
            "Codes use Annex's server to swap the two computers' internet addresses; the screen goes straight between \
             them, encrypted. Some networks block that: then it goes through a relay, yours if you set one here.",
        ),
    );
    let fields: [(&str, &str, &str, String, SetText); 4] = [
        (
            "Server",
            "Optional. Leave empty to use Annex's server.",
            crate::settings::ANNEX_SERVER,
            s.rendezvous_server.clone(),
            |m, v| m.settings.rendezvous_server = v,
        ),
        (
            "Your relay",
            "Optional. A TURN server, for networks that block direct connections.",
            "turn:relay.example.com:3478",
            s.relay_url.clone(),
            |m, v| m.settings.relay_url = v,
        ),
        ("Relay user name", "", "User name", s.relay_username.clone(), |m, v| m.settings.relay_username = v),
        (
            "Relay password",
            "Kept in Annex's settings file, readable only by you.",
            "Password",
            s.relay_password.clone(),
            |m, v| m.settings.relay_password = v,
        ),
    ];
    for (i, (title, subtitle, hint, value, set)) in fields.into_iter().enumerate() {
        let e = gtk::Entry::builder().placeholder_text(hint).text(value).width_chars(28).build();
        if i == 3 {
            e.set_visibility(false);
        }
        let (app, model) = (app.clone(), model.clone());
        e.connect_changed(move |e| {
            let v = e.text().to_string();
            // The field's name only: never what's typed (a relay password, for one).
            tracing::debug!(target: "annex::settings", field = title, empty = v.is_empty(), "settings: edited");
            act(&app, &model, |m| {
                set(m, v);
                m.save_settings();
            });
        });
        add_row(&g, &row_with(title, (!subtitle.is_empty()).then_some(subtitle), &e));
    }
    advanced.append(&internet);

    // Developer
    let (developer, g) = section(Some("Developer"), None);
    switch_row(
        app,
        model,
        &g,
        "Show technical details",
        Some("Identity codes, connection routes and the video decoder and encoder."),
        s.show_technical_details,
        Box::new(|m, on| {
            m.settings.show_technical_details = on;
            let _ = m.after_setting(Effect::Render);
        }),
    );
    switch_row(
        app,
        model,
        &g,
        "Verbose logging",
        Some(
            "Also logs every mouse and keyboard event, never which keys. The log fills faster; turn it on to chase a \
             problem.",
        ),
        s.verbose_logging,
        Box::new(|m, on| {
            m.settings.verbose_logging = on;
            let _ = m.after_setting(Effect::VerboseLog);
        }),
    );
    let log_dir = annex_log::dir().unwrap_or_else(crate::settings::data_dir);
    let show = gtk::Button::with_label("Show Log Files");
    let log_text = format!(
        "Always on: {}, and a copy of what led up to each error in its crashes folder. It stays on this computer; \
         Annex never sends it anywhere. Attach it to a bug report if something goes wrong.",
        log_dir.join("annex.log").display()
    );
    add_row(&g, &row_with("Log files", Some(log_text.as_str()), &show));
    show.connect_clicked(move |_| {
        tracing::info!(target: "annex::settings", "settings: Show Log Files");
        let uri = gio::File::for_path(&log_dir).uri();
        if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
            tracing::warn!(target: "annex::settings", "settings: couldn't open the log folder: {e}");
        }
    });
    advanced.append(&developer);
    b
}

// ---- About ---------------------------------------------------------------------------------

struct About {
    page: gtk::Box,
    details: gtk::Label,
    spinner: gtk::Spinner,
    icon: gtk::Image,
    text: gtk::Label,
    download: gtk::Button,
    check_now: gtk::Button,
    last_checked: gtk::Label,
}

fn about_page(app: &gtk::Application, model: &Shared) -> About {
    let b = vbox(22);

    // The header card: icon, name, version, links.
    let card = vbox(0);
    card.add_css_class("group");
    card.add_css_class("about-card");
    let icon = app_icon(96);
    icon.set_halign(gtk::Align::Center);
    card.append(&icon);
    let name = plain("Annex", "about-title");
    name.set_halign(gtk::Align::Center);
    name.set_margin_top(10);
    card.append(&name);
    // The release's version (the core's; this crate's own isn't stamped).
    let version = plain(&format!("Version {}", whats_new::CURRENT), "dim");
    version.set_halign(gtk::Align::Center);
    version.set_margin_top(2);
    card.append(&version);
    let blurb = label(
        "Use another computer as a second display, or mirror its screen.\nLinux, Mac and Windows, over your network \
         or a cable, encrypted end to end.",
        "dim",
    );
    blurb.set_xalign(0.5);
    blurb.set_justify(gtk::Justification::Center);
    blurb.set_halign(gtk::Align::Center);
    blurb.set_margin_top(14);
    card.append(&blurb);
    let links = hbox(10);
    links.set_halign(gtk::Align::Center);
    links.set_margin_top(16);
    let news = gtk::Button::with_label("What's New");
    {
        let app = app.clone();
        news.connect_clicked(move |_| {
            tracing::info!(target: "annex::update", "update: What's New pressed");
            show_whats_new(&app, &whats_new::all());
        });
    }
    links.append(&news);
    let notes = gtk::Button::with_label("Release Notes");
    notes.connect_clicked(|_| open_uri(&format!("{REPO}/releases/tag/v{}", whats_new::CURRENT)));
    links.append(&notes);
    links.append(&link_button("Report an Issue", "https://github.com/NRanjan-17/Annex/issues"));
    links.append(&link_button("View on GitHub", REPO));
    card.append(&links);
    let license = plain("MIT License · © 2026 Nalinish Ranjan and Saidev Dhal", "footnote");
    license.set_halign(gtk::Align::Center);
    license.set_margin_top(16);
    card.append(&license);
    b.append(&card);

    // Updates.
    let s = model.borrow().settings.clone();
    let (updates, g) = section(Some("Updates"), None);
    switch_row(
        app,
        model,
        &g,
        "Check for updates automatically",
        Some("Once a day, from GitHub. Nothing about you is sent."),
        s.check_for_updates,
        Box::new(|m, on| {
            m.settings.check_for_updates = on;
            let _ = m.after_setting(Effect::CheckUpdates);
        }),
    );
    switch_row(
        app,
        model,
        &g,
        "Receive beta updates",
        Some("Beta versions include features in development and may contain bugs."),
        s.beta_updates,
        Box::new(|m, on| {
            m.settings.beta_updates = on;
            m.settings.beta_updates_chosen = true;
            let _ = m.after_setting(Effect::BetaUpdates);
        }),
    );
    let status = hbox(10);
    status.add_css_class("row");
    let spinner = gtk::Spinner::new();
    spinner.set_visible(false);
    status.append(&spinner);
    let status_icon = gtk::Image::from_icon_name("emblem-ok-symbolic");
    status_icon.set_pixel_size(16);
    status_icon.set_visible(false);
    status.append(&status_icon);
    let text = label("", "row-title");
    text.set_hexpand(true);
    text.set_selectable(true);
    status.append(&text);
    let download = gtk::Button::with_label("Download");
    download.add_css_class("suggested-action");
    download.set_valign(gtk::Align::Center);
    download.set_visible(false);
    download.set_tooltip_text(Some("Opens the release page. Install the .deb or .rpm with your package manager."));
    {
        let model = model.clone();
        download.connect_clicked(move |_| open_update_page(&model));
    }
    status.append(&download);
    add_row(&g, &status);
    let check_now = gtk::Button::with_label("Check Now");
    {
        let (app, model) = (app.clone(), model.clone());
        check_now.connect_clicked(move |_| {
            tracing::info!(target: "annex::update", "update: Check Now pressed");
            act(&app, &model, |m| {
                m.update_dismissed = None;
                m.check_for_updates();
            });
        });
    }
    let last_checked = plain("Not checked yet.", "footnote");
    last_checked.set_hexpand(true);
    let check_row = hbox(12);
    check_row.add_css_class("row");
    check_row.append(&last_checked);
    check_now.set_valign(gtk::Align::Center);
    check_row.append(&check_now);
    add_row(&g, &check_row);
    b.append(&updates);

    // This computer.
    let (this, g) = section(Some("This computer"), None);
    let details = label("", "subtitle");
    details.set_selectable(true);
    let c = cell(0);
    c.append(&details);
    add_row(&g, &c);
    b.append(&this);

    About { page: b, details, spinner, icon: status_icon, text, download, check_now, last_checked }
}

// ---- Rendering -----------------------------------------------------------------------------

/// Refreshes the window from the model.
pub fn render(model: &Shared) {
    let Ok(m) = model.try_borrow() else { return };
    with_ui(|ui| {
        let banner = m.startup_error.clone().or_else(|| m.host_error.clone());
        ui.banner.set_visible(banner.is_some());
        ui.banner.set_text(banner.as_deref().unwrap_or(""));

        render_live(ui, &m, model);
        render_nearby(ui, &m, model);
        render_share(ui, &m, model);
        render_devices(ui, &m, model);
        render_update(ui, &m);

        let sharing = m.peers().iter().filter(|p| p.sharing).count();
        ui.nav_badge.set_text(&sharing.to_string());
        ui.nav_badge.set_visible(sharing > 0);
        ui.nav_live.set_visible(m.is_hosting());

        let details = format!(
            "This computer: {}\nIdentity code: {}\nDesktop: {}\nVideo decoder: {}\nVideo encoder: {}",
            m.device_name(),
            m.fingerprint_display(),
            crate::desktop::session().describe(),
            crate::viewer::decoder().unwrap_or_else(|| "none found".into()),
            crate::encode::describe(m.settings.prefer_hardware_encoder),
        );
        ui.about_details.set_text(&details);
    });
}

/// Rebuilds a list only when its content changed (`key` is a summary of what it shows).
fn changed(ui: &mut Ui, list: &'static str, key: String) -> bool {
    if ui.last.get(list) == Some(&key) {
        return false;
    }
    ui.last.insert(list, key);
    true
}

fn app_of(ui: &Ui) -> gtk::Application {
    ui.window.application().expect("window has an application")
}

fn render_update(ui: &mut Ui, m: &Model) {
    let (text, icon, class) = match &m.update {
        UpdateState::Idle => (format!("Annex {}", whats_new::CURRENT), None, ""),
        UpdateState::Checking => ("Checking for updates…".to_string(), None, ""),
        UpdateState::UpToDate => ("You're on the latest version.".to_string(), Some("emblem-ok-symbolic"), "ok"),
        UpdateState::Available(u) => (UpdateState::headline(u), Some("software-update-available-symbolic"), ""),
        UpdateState::Failed(e) => (format!("Couldn't check for updates: {e}"), Some("dialog-warning-symbolic"), "warn"),
    };
    ui.update_text.set_text(&text);
    let checking = m.update == UpdateState::Checking;
    ui.update_spinner.set_visible(checking);
    ui.update_spinner.set_spinning(checking);
    ui.update_icon.set_visible(icon.is_some());
    if let Some(icon) = icon {
        ui.update_icon.set_icon_name(Some(icon));
    }
    ui.update_icon.remove_css_class("ok");
    ui.update_icon.remove_css_class("warn");
    if !class.is_empty() {
        ui.update_icon.add_css_class(class);
    }
    ui.update_download.set_visible(matches!(m.update, UpdateState::Available(_)));
    ui.check_now.set_sensitive(!checking);
    let last = m
        .update_checked_at
        .and_then(|t| glib::DateTime::from_unix_local(t).ok())
        .and_then(|d| d.format("%e %b %Y, %H:%M").ok())
        .map(|d| format!("Last checked: {}", d.trim()))
        .unwrap_or_else(|| "Not checked yet.".into());
    ui.last_checked.set_text(&last);

    let banner = m.update_banner().map(UpdateState::headline);
    for b in &ui.update_banners {
        b.root.set_visible(banner.is_some());
        b.text.set_text(banner.as_deref().unwrap_or(""));
    }
}

/// What's live, in numbers big enough to read from across the room.
fn render_live(ui: &mut Ui, m: &Model, model: &Shared) {
    let role = m.role();
    let viewing = matches!(role, Some(SessionRole::Viewing { .. }));
    let subtitle = match &role {
        Some(SessionRole::Viewing { .. }) => {
            let mut parts = Vec::new();
            if !m.viewer_route.is_empty() {
                parts.push(m.viewer_route.clone());
            }
            parts.push(if m.controlling { "You're in control".into() } else { "Encrypted end to end".into() });
            parts.join(" · ")
        }
        Some(SessionRole::Sharing { .. }) => {
            let controllers: Vec<String> =
                m.host_sessions.iter().filter(|s| s.input).map(|s| m.label(&s.name)).collect();
            if m.host_sessions.iter().any(|s| s.extend) {
                "Arrange it in your desktop's display settings".into()
            } else if !controllers.is_empty() {
                format!("{} can use your mouse and keyboard", controllers.join(", "))
            } else {
                "View only: it can't use your mouse or keyboard".into()
            }
        }
        None => String::new(),
    };
    let key = format!("{role:?}{subtitle}");
    if changed(ui, "live", key) {
        clear(&ui.live);
        ui.live_stats.clear();
        if let Some(role) = &role {
            let card = hbox(28);
            card.add_css_class("live");
            let left = vbox(6);
            left.set_hexpand(true);
            left.set_valign(gtk::Align::Center);
            let top = hbox(8);
            top.append(&live_dot());
            top.append(&plain(&format!("LIVE · {}", role.headline().to_uppercase()), "live-eyebrow"));
            left.append(&top);
            let (flow, stop) = match role {
                SessionRole::Sharing { with, extend } => (
                    if *extend {
                        format!("This computer → {} (its extra display)", m.label(with))
                    } else {
                        format!("This computer → {}", m.label(with))
                    },
                    "Stop Sharing",
                ),
                SessionRole::Viewing { from, extend } => (
                    if *extend {
                        format!("{} → this computer (an extra display)", m.label(from))
                    } else {
                        format!("{} → this computer", m.label(from))
                    },
                    "Disconnect",
                ),
            };
            left.append(&label(&flow, "live-flow"));
            left.append(&label(&subtitle, "dim"));
            card.append(&left);
            // The viewer measures the picture: delay, smoothness and size.
            if viewing {
                for (unit, name) in [("ms", "Delay"), ("fps", "Smoothness"), ("", "Picture")] {
                    let stat = vbox(0);
                    stat.set_valign(gtk::Align::Center);
                    let line = hbox(3);
                    let value = plain("–", "stat-value");
                    line.append(&value);
                    if !unit.is_empty() {
                        let u = plain(unit, "stat-unit");
                        u.set_valign(gtk::Align::End);
                        u.set_margin_bottom(8);
                        line.append(&u);
                    }
                    stat.append(&line);
                    stat.append(&plain(name, "stat-label"));
                    card.append(&stat);
                    ui.live_stats.push(value);
                }
            }
            let b = gtk::Button::with_label(stop);
            b.set_valign(gtk::Align::Center);
            b.add_css_class("destructive-action");
            let (app, model) = (app_of(ui), model.clone());
            b.connect_clicked(move |_| {
                tracing::info!(target: "annex::session", "ui: {} pressed", if viewing { "Disconnect" } else { "Stop Sharing" });
                act(&app, &model, |m| if viewing { m.disconnect() } else { m.set_hosting(false) });
            });
            card.append(&b);
            ui.live.append(&card);
        }
    }
    if let [delay, fps, size] = ui.live_stats.as_slice() {
        match &m.latency {
            Some(l) => {
                delay.set_text(&format!("{:.0}", l.p50_ms));
                fps.set_text(&format!("{:.0}", l.fps));
            }
            None => {
                delay.set_text("–");
                fps.set_text("–");
            }
        }
        size.set_text(&m.stream_size.map(|(w, h)| format!("{w}×{h}")).unwrap_or_else(|| "–".into()));
    }

    let same = m.same_name_peer();
    if changed(ui, "same", format!("{same:?}")) {
        clear(&ui.same_name);
        if let Some(name) = same {
            let (notice, g) = section(None, None);
            let r = row(
                &format!(
                    "Another computer is also called “{name}”. Rename this one in Settings so you can tell them apart."
                ),
                None,
            );
            let go = gtk::Button::with_label("Open Settings");
            go.set_valign(gtk::Align::Center);
            let stack = ui.stack.clone();
            go.connect_clicked(move |_| stack.set_visible_child_name("settings"));
            r.append(&go);
            add_row(&g, &r);
            ui.same_name.append(&notice);
        }
    }

    ui.status.set_text(m.viewer_status.as_deref().unwrap_or(""));
    ui.status.set_visible(m.viewer_status.is_some() && role.is_none());
    ui.cancel_connect.set_visible(m.viewer.is_some() && m.viewer_extends.is_none());
}

fn render_nearby(ui: &mut Ui, m: &Model, model: &Shared) {
    let peers = m.peers();
    let live = m.role().is_some();
    ui.waiting.set_visible(peers.is_empty() && !live);
    ui.nearby_section.set_visible(!peers.is_empty());
    let key = format!("{peers:?}{}{}{}", m.trusted.len(), m.viewer.is_some(), m.settings.extend_by_default);
    if !changed(ui, "nearby", key) {
        return;
    }
    clear(&ui.nearby);
    for p in peers {
        let r = hbox(12);
        r.add_css_class("row");
        let dot = hbox(0);
        dot.add_css_class("peer-dot");
        if !p.sharing {
            dot.add_css_class("off");
        }
        dot.set_valign(gtk::Align::Center);
        r.append(&dot);
        let text = vbox(2);
        text.set_hexpand(true);
        let name = m.label(&p.name);
        let title = label(&name, "row-title");
        title.add_css_class("strong");
        text.append(&title);
        let mut sub = Vec::new();
        if m.has_this_name(&p.name) {
            sub.push("Other computer".to_string());
        }
        sub.push(if p.sharing { "Sharing its screen".into() } else { "Annex is open, not sharing".into() });
        if m.is_paired(&p) {
            sub.push("Paired".into());
        }
        if p.mesh {
            sub.push("Private network".into());
        }
        text.append(&label(&sub.join(" · "), "subtitle"));
        r.append(&text);
        if p.sharing {
            let show = gtk::Button::with_label("Show Its Screen");
            let mut buttons = vec![(show, false)];
            if p.extend == Some(true) {
                let ext = gtk::Button::with_label("Use as Display");
                // The default action first, in white.
                if m.settings.extend_by_default {
                    buttons.insert(0, (ext, true));
                } else {
                    buttons.push((ext, true));
                }
            }
            for (i, (b, extend)) in buttons.into_iter().enumerate() {
                b.set_valign(gtk::Align::Center);
                if i == 0 {
                    b.add_css_class("suggested-action");
                }
                let (app, model, peer) = (app_of(ui), model.clone(), p.clone());
                b.connect_clicked(move |b| {
                    tracing::info!(target: "annex::viewer", peer = peer.name, extend, "ui: connect pressed in Nearby");
                    let display = crate::viewer::display_info(Some(b));
                    act(&app, &model, |m| m.connect_peer(&peer, extend, display));
                });
                r.append(&b);
            }
        }
        add_row(&ui.nearby, &r);
    }
}

fn render_share(ui: &mut Ui, m: &Model, model: &Shared) {
    if ui.share_switch.is_active() != m.is_hosting() {
        ui.share_switch.set_active(m.is_hosting());
    }
    ui.share_title.set_text(if m.is_hosting() { "Sharing is on" } else { "Sharing is off" });
    ui.share_note.set_text(&if m.is_hosting() {
        format!(
            "Computers nearby see “{}”. The first time, they type a PIN shown here. {}",
            m.device_name(),
            crate::host::extend_note()
        )
    } else {
        "Nothing on this computer is shown to anyone.".into()
    });

    let key = format!("{:?}", m.host_sessions);
    if changed(ui, "sessions", key) {
        clear(&ui.sessions);
        ui.sessions_section.set_visible(!m.host_sessions.is_empty());
        for s in &m.host_sessions {
            let what = format!(
                "{} · {}",
                if s.extend { "Using this computer as a display" } else { "Viewing this screen" },
                if s.input { "Can use the mouse and keyboard" } else { "Can only watch" }
            );
            let r = row(&m.label(&s.name), Some(what.as_str()));
            let dot = live_dot();
            r.prepend(&dot);
            add_row(&ui.sessions, &r);
        }
    }

    render_local(ui, m);

    let key = format!("{:?}{}", m.internet, m.is_hosting());
    if changed(ui, "code", key) {
        clear(&ui.code);
        if !m.is_hosting() {
            ui.code.append(&label("Turn on sharing to get a code.", "dim"));
            return;
        }
        let app = app_of(ui);
        let button = |text: &str, f: fn(&mut Model)| {
            let b = gtk::Button::with_label(text);
            b.set_halign(gtk::Align::Start);
            let (app, model) = (app.clone(), model.clone());
            b.connect_clicked(move |_| act(&app, &model, f));
            b
        };
        match &m.internet {
            None => {
                ui.code
                    .append(&label("A code lets one computer anywhere connect. It works once, for 10 minutes.", "dim"));
                let b = button("Share with a Code", |m| m.share_code());
                b.add_css_class("suggested-action");
                ui.code.append(&b);
            }
            Some(InternetShare::Starting) => {
                let r = hbox(8);
                let sp = gtk::Spinner::new();
                sp.set_spinning(true);
                r.append(&sp);
                r.append(&label("Getting a code…", "dim"));
                ui.code.append(&r);
            }
            Some(InternetShare::Code { code, expires }) => {
                let r = hbox(16);
                let text = vbox(6);
                text.set_hexpand(true);
                let c = label(code, "code");
                c.set_selectable(true);
                text.append(&c);
                let left = label(&code_time_left(*expires), "dim");
                text.append(&left);
                r.append(&text);
                let buttons = vbox(8);
                buttons.set_valign(gtk::Align::Center);
                buttons.append(&copy_button(code, "the code"));
                let stop = button("Stop", |m| m.stop_code());
                stop.set_halign(gtk::Align::Fill);
                stop.add_css_class("destructive-action");
                buttons.append(&stop);
                r.append(&buttons);
                ui.code.append(&r);
                // The countdown, each second, until the cell is replaced.
                let (weak, expires) = (left.downgrade(), *expires);
                glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
                    let Some(left) = weak.upgrade() else { return glib::ControlFlow::Break };
                    let text = code_time_left(expires);
                    let expired = text == "Expired.";
                    left.set_text(&text);
                    if expired { glib::ControlFlow::Break } else { glib::ControlFlow::Continue }
                });
            }
            Some(InternetShare::Joining(name)) => {
                ui.code.append(&label(&format!("{} is connecting…", m.label(name)), ""))
            }
            Some(InternetShare::Relay { name, relayed, limited }) => {
                let text = match (relayed, limited) {
                    (true, true) => {
                        format!("{} is connecting through Annex's relay (limited time and quality).", m.label(name))
                    }
                    (true, false) => format!("{} is connecting through your relay.", m.label(name)),
                    (false, _) => format!("{} couldn't connect directly.", m.label(name)),
                };
                ui.code.append(&label(&text, ""));
            }
            Some(InternetShare::Ended(why)) => {
                if let Some(why) = why {
                    ui.code.append(&label(why, ""));
                }
                ui.code.append(&button("Share with a New Code", |m| m.share_code()));
            }
        }
    }
}

/// While sharing: pick this computer's name over there, or type one of its addresses.
fn render_local(ui: &mut Ui, m: &Model) {
    let hosting = m.is_hosting();
    ui.local_section.set_visible(hosting);
    let identity = format!("Identity code: {}", m.fingerprint_display());
    ui.local_footer.set_visible(hosting && m.settings.show_technical_details);
    ui.local_footer.set_text(&identity);
    if !hosting {
        ui.last.remove("local");
        return;
    }
    let addresses = crate::model::local_addresses();
    let port = m.host_port.unwrap_or(annex_session::DEFAULT_PORT);
    let name = m.device_name();
    let key = format!("{addresses:?}{port}{name}");
    if !changed(ui, "local", key) {
        return;
    }
    clear(&ui.local);
    add_row(
        &ui.local,
        &row(
            &format!("Pick “{name}” in Annex on the other computer."),
            Some("The first time, type the PIN this computer shows. Not listed? Connect by one of these addresses."),
        ),
    );
    if addresses.is_empty() {
        add_row(
            &ui.local,
            &row("This computer isn't on a network.", Some("Connect it to Wi-Fi, Ethernet or a cable.")),
        );
    }
    for a in &addresses {
        let text = if port == annex_session::DEFAULT_PORT { a.ip.to_string() } else { format!("{}:{port}", a.ip) };
        let r = hbox(10);
        r.add_css_class("row");
        let ip = plain(&text, "mono");
        ip.add_css_class("row-title");
        ip.set_selectable(true);
        ip.set_valign(gtk::Align::Center);
        r.append(&ip);
        let kind = plain(a.link.label(), "subtitle");
        kind.set_valign(gtk::Align::Center);
        r.append(&kind);
        r.append(&spacer());
        let copy = copy_button(&text, "an address");
        copy.set_valign(gtk::Align::Center);
        r.append(&copy);
        add_row(&ui.local, &r);
    }
}

fn render_devices(ui: &mut Ui, m: &Model, model: &Shared) {
    let details = m.settings.show_technical_details;
    ui.own_identity.set_text(&format!("This computer's identity code: {}", m.fingerprint_display()));
    ui.own_identity.set_visible(details);
    let key = format!(
        "{:?} {} {details}",
        m.trusted.iter().map(|t| (&t.fingerprint, &t.name, t.allow_input, t.last_seen)).collect::<Vec<_>>(),
        m.settings.headless
    );
    if !changed(ui, "devices", key) {
        return;
    }
    clear(&ui.devices);
    if m.trusted.is_empty() {
        let (s, g) = section(None, None);
        add_row(&g, &row("No paired devices", Some("Pair by connecting from another computer, or to one.")));
        ui.devices.append(&s);
        return;
    }
    for t in &m.trusted {
        let (s, g) = section(None, None);
        let name = m.label(&t.name);
        let last_seen = (t.last_seen > 0)
            .then(|| glib::DateTime::from_unix_local(t.last_seen as i64).ok())
            .flatten()
            .and_then(|d| d.format("%e %b %Y").ok())
            .map(|d| format!("Last connected {}", d.trim()))
            .unwrap_or_else(|| "Paired".into());
        let remove = gtk::Button::with_label("Remove…");
        remove.add_css_class("destructive-action");
        {
            let (app, model, name, fp) = (app_of(ui), model.clone(), name.clone(), t.fingerprint.clone());
            remove.connect_clicked(move |_| confirm_forget(&app, &model, &name, &fp));
        }
        let head = row_with(&name, Some(last_seen.as_str()), &remove);
        add_row(&g, &head);

        // Headless lets every paired device control this computer.
        let input = gtk::Switch::new();
        input.set_active(t.allow_input || m.settings.headless);
        input.set_sensitive(!m.settings.headless);
        {
            let (app, model, fp) = (app_of(ui), model.clone(), t.fingerprint.clone());
            input.connect_state_set(move |_, on| {
                let fp = fp.clone();
                act(&app, &model, |m| m.set_allow_input(&fp, on));
                glib::Propagation::Proceed
            });
        }
        add_row(
            &g,
            &row_with(
                "Can use this computer's mouse and keyboard",
                m.settings.headless.then_some("Allowed while No one at this screen is on."),
                &input,
            ),
        );
        // Sound both ways, one device at a time (on until turned off).
        let sound = gtk::Switch::new();
        sound.set_active(t.allow_sound);
        {
            let (app, model, fp) = (app_of(ui), model.clone(), t.fingerprint.clone());
            sound.connect_state_set(move |_, on| {
                let fp = fp.clone();
                act(&app, &model, |m| m.set_allow_sound(&fp, on));
                glib::Propagation::Proceed
            });
        }
        add_row(
            &g,
            &row_with(
                "Can hear this computer's sound",
                (!m.settings.share_sound).then_some("This computer's sound is off in Settings."),
                &sound,
            ),
        );
        let mic = gtk::Switch::new();
        mic.set_active(t.allow_mic);
        {
            let (app, model, fp) = (app_of(ui), model.clone(), t.fingerprint.clone());
            mic.connect_state_set(move |_, on| {
                let fp = fp.clone();
                act(&app, &model, |m| m.set_allow_mic(&fp, on));
                glib::Propagation::Proceed
            });
        }
        add_row(&g, &row_with("Its microphone plays here", None, &mic));
        if details {
            let id = cell(2);
            id.append(&label("Identity code", "row-title"));
            let fp = label(&crate::model::display_fingerprint(&t.fingerprint), "subtitle");
            fp.add_css_class("mono");
            fp.set_selectable(true);
            id.append(&fp);
            add_row(&g, &id);
        }
        ui.devices.append(&s);
    }
}

// ---- Dialogs -------------------------------------------------------------------------------

fn dialog(app: &gtk::Application, title: &str) -> (gtk::Window, gtk::Box) {
    let w = gtk::Window::builder().application(app).title(title).modal(true).resizable(false).build();
    w.add_css_class("annex");
    if let Some(parent) = with_ui(|ui| ui.window.clone()).filter(|p| p.is_visible()) {
        w.set_transient_for(Some(&parent));
    }
    let b = vbox(12);
    b.add_css_class("page");
    b.add_css_class("annex-content");
    b.set_width_request(380);
    w.set_child(Some(&b));
    (w, b)
}

/// Forget a paired device, after asking.
fn confirm_forget(app: &gtk::Application, model: &Shared, name: &str, fingerprint: &str) {
    tracing::info!(target: "annex::pairing", fingerprint, "paired devices: Remove pressed; asking");
    let (w, b) = dialog(app, "Remove Device");
    b.append(&label(&format!("Remove {name}?"), "dialog-title"));
    b.append(&label("It will need a PIN to connect again.", "dim"));
    let r = hbox(8);
    r.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let remove = gtk::Button::with_label("Remove");
    remove.add_css_class("destructive-action");
    r.append(&cancel);
    r.append(&remove);
    b.append(&r);
    let w2 = w.clone();
    cancel.connect_clicked(move |_| w2.destroy());
    let (app, model, fp, w2) = (app.clone(), model.clone(), fingerprint.to_string(), w.clone());
    remove.connect_clicked(move |_| {
        let fp = fp.clone();
        act(&app, &model, |m| m.forget(&fp));
        w2.destroy();
    });
    w.present();
    cancel.grab_focus();
}

/// The release notes: each release newest first, its items under New, Improved and Fixed.
pub fn show_whats_new(app: &gtk::Application, notes: &[Release]) {
    let Some(newest) = notes.first() else { return };
    let (w, b) = dialog(app, "What's New");
    b.set_width_request(540);
    b.set_spacing(16);
    b.append(&hero(
        &format!("What's New in Annex {}", newest.version),
        if notes.len() > 1 { "Every version, newest first." } else { "What this update adds and fixes, in short." },
    ));
    let body = vbox(18);
    for r in notes {
        if notes.len() > 1 {
            body.append(&label(&format!("Annex {}", r.version), "version"));
        }
        for (kind, heading) in [("new", "New"), ("improved", "Improved"), ("fixed", "Fixed")] {
            let items: Vec<_> = r.items.iter().filter(|i| i.kind == kind).collect();
            if items.is_empty() {
                continue;
            }
            let (s, g) = section(Some(heading), None);
            for i in items {
                let item = cell(2);
                let title = label(&i.title, "row-title");
                title.add_css_class("strong");
                title.set_max_width_chars(60);
                item.append(&title);
                let detail = label(&i.detail, "subtitle");
                detail.set_max_width_chars(60);
                item.append(&detail);
                add_row(&g, &item);
            }
            body.append(&s);
        }
    }
    let scroll = gtk::ScrolledWindow::builder()
        .child(&body)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(440)
        .build();
    b.append(&scroll);
    let ok = gtk::Button::with_label("Got It");
    ok.add_css_class("suggested-action");
    ok.set_halign(gtk::Align::End);
    b.append(&ok);
    let w2 = w.clone();
    ok.connect_clicked(move |_| w2.close());
    let keys = gtk::EventControllerKey::new();
    let w2 = w.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            w2.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    w.add_controller(keys);
    w.connect_close_request(|_| {
        tracing::info!(target: "annex::update", "update: what's new seen");
        glib::Propagation::Proceed
    });
    w.present();
    ok.grab_focus();
}

fn ask_pin(app: &gtk::Application, model: &Shared, request_id: u64, peer: &str, fingerprint: &str) {
    let (w, b) = dialog(app, "Pair with Annex");
    b.append(&label(&format!("Type the PIN shown on {peer}."), "role"));
    b.append(&label(&format!("Its identity: {fingerprint}"), "dim"));
    let entry = gtk::Entry::builder()
        .placeholder_text("6-digit PIN")
        .max_length(6)
        .input_purpose(gtk::InputPurpose::Digits)
        .build();
    b.append(&entry);
    let row = hbox(8);
    row.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("Pair");
    ok.add_css_class("suggested-action");
    row.append(&cancel);
    row.append(&ok);
    b.append(&row);
    tracing::info!(target: "annex::pairing", request_id, peer, "pairing: asking for the PIN shown on the other computer");
    let submit = {
        let (model, entry, w) = (model.clone(), entry.clone(), w.clone());
        move || {
            model.borrow().submit_pin(request_id, entry.text().trim());
            w.set_sensitive(false);
        }
    };
    let s = submit.clone();
    ok.connect_clicked(move |_| s());
    entry.connect_activate(move |_| submit());
    {
        let (model, w2) = (model.clone(), w.clone());
        cancel.connect_clicked(move |_| {
            model.borrow().cancel_request(request_id);
            w2.destroy();
        });
    }
    with_ui(|ui| ui.pins.insert(request_id, w.clone()));
    w.present();
}

fn show_pin(app: &gtk::Application, model: &Shared, request_id: u64, pin: &str, peer: &str, fingerprint: &str) {
    // Never the PIN itself.
    tracing::info!(target: "annex::pairing", request_id, peer, "pairing: showing a PIN");
    let (w, b) = dialog(app, "Pair with Annex");
    b.append(&label(&format!("{peer} wants to connect. Type this PIN there:"), "role"));
    let p = label(pin, "pin");
    p.set_xalign(0.5);
    b.append(&p);
    b.append(&label(&format!("Its identity: {fingerprint}. Only type the PIN on a computer you know."), "dim"));
    let cancel = gtk::Button::with_label("Don't Allow");
    cancel.set_halign(gtk::Align::End);
    {
        let (model, w2) = (model.clone(), w.clone());
        cancel.connect_clicked(move |_| {
            model.borrow().cancel_request(request_id);
            w2.destroy();
        });
    }
    b.append(&cancel);
    with_ui(|ui| ui.pins.insert(request_id, w.clone()));
    w.present();
}

fn ask_input(app: &gtk::Application, model: &Shared, peer: &str, fingerprint: &str) {
    tracing::info!(target: "annex::permissions", peer, fingerprint, "sharing: asking to allow control");
    let (w, b) = dialog(app, "Allow control?");
    b.append(&label(&format!("{peer} wants to use this computer's mouse and keyboard."), "role"));
    b.append(&label("You can change this later in Paired Devices.", "dim"));
    let row = hbox(8);
    row.set_halign(gtk::Align::End);
    let no = gtk::Button::with_label("Don't Allow");
    let yes = gtk::Button::with_label("Allow");
    yes.add_css_class("suggested-action");
    row.append(&no);
    row.append(&yes);
    b.append(&row);
    let w2 = w.clone();
    no.connect_clicked(move |_| {
        tracing::info!(target: "annex::permissions", "sharing: control not allowed");
        w2.destroy()
    });
    let (app, model, fp, w2) = (app.clone(), model.clone(), fingerprint.to_string(), w.clone());
    yes.connect_clicked(move |_| {
        tracing::info!(target: "annex::permissions", "sharing: control allowed");
        let fp = fp.clone();
        act(&app, &model, |m| m.set_allow_input(&fp, true));
        w2.destroy();
    });
    w.present();
}
