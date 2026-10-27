//! The drop island: a black shape hanging from the top centre of the screen, like the Mac's notch.
//! While another computer is connected, dragging files to the top edge pops it open, and dropping
//! sends them; files going and coming show in it. Needs the layer shell ([`crate::layer`]) or X11
//! ([`crate::x11_drag`]); without them (GNOME), files are dropped on the viewer window and
//! arrivals show as notifications.
//!
//! Wayland doesn't let an app watch the pointer, so there a 3-pixel strip at the top centre waits
//! for the drag; on X11 the island glows as soon as files are picked up.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::model::Msg;

const CSS: &str = "
window.annex-island-window { background: transparent; }
.annex-island-window.solid .annex-island { margin: 0; box-shadow: none; }
.annex-island {
  background: black; border-radius: 0 0 20px 20px; border: 0 solid transparent;
  min-width: 300px; min-height: 3px; margin: 0; opacity: 0.01;
  transition: min-width 260ms ease-out, min-height 260ms ease-out, margin 260ms ease-out,
    opacity 200ms ease-out, box-shadow 260ms ease-out;
}
.annex-island.glowing, .annex-island.open, .annex-island.transfers {
  opacity: 1; margin: 0 26px 26px 26px;
  transition: min-width 420ms cubic-bezier(0.34, 1.56, 0.64, 1), min-height 420ms cubic-bezier(0.34, 1.56, 0.64, 1),
    margin 300ms ease-out, opacity 150ms ease-out, box-shadow 300ms ease-out;
}
.annex-island.glowing { min-width: 200px; min-height: 34px; }
.annex-island.open { min-width: 380px; min-height: 150px; }
.annex-island.transfers { min-width: 380px; }
.annex-island.glowing, .annex-island.open {
  border-width: 0 2px 2px 2px; animation: annex-flow 2.4s linear infinite;
  box-shadow: 0 0 8px rgba(255, 255, 255, 0.9), 0 0 22px rgba(255, 255, 255, 0.5);
}
@keyframes annex-flow {
  0% { border-color: #9e9e9e #ffffff #d6d6d6 #ffffff; }
  25% { border-color: #ffffff #9e9e9e #ffffff #d6d6d6; }
  50% { border-color: #d6d6d6 #ffffff #9e9e9e #ffffff; }
  75% { border-color: #ffffff #d6d6d6 #ffffff #9e9e9e; }
  100% { border-color: #9e9e9e #ffffff #d6d6d6 #ffffff; }
}
.annex-island label, .annex-island image { color: white; }
.annex-island .title { font-weight: 600; }
.annex-island .dim { color: rgba(255, 255, 255, 0.6); font-size: 0.9em; }
.annex-island .badge { font-weight: 700; min-width: 28px; }
.annex-island .badge.done { color: #3dd185; }
.annex-island progressbar trough { background: rgba(255, 255, 255, 0.15); min-height: 4px; border-radius: 2px; }
.annex-island progressbar progress { background: #3dd185; min-height: 4px; border-radius: 2px; }
.annex-island button { background: rgba(255, 255, 255, 0.15); color: white; border: none; box-shadow: none;
  border-radius: 12px; padding: 2px 12px; min-height: 0; }
.annex-island button:hover { background: rgba(255, 255, 255, 0.25); }
.annex-island button.round { padding: 0; min-width: 24px; min-height: 24px; border-radius: 12px; }
.annex-island button.round image { -gtk-icon-size: 12px; }
";

/// X11: the window, big enough for the island at its largest (four transfers) and its glow.
const WINDOW_WIDTH: i32 = 440;
const WINDOW_HEIGHT: i32 = 300;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Only the strip that waits for a drag.
    Hidden,
    /// A drag reached the top: a small glowing pill, about to open.
    Glowing,
    Open,
    Transfers,
}

/// One file going or coming. `viewer`: in the session this computer views (else one it shares);
/// the two number their sessions apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TransferKey {
    pub viewer: bool,
    pub session: u64,
    pub id: u64,
    pub incoming: bool,
}

struct Item {
    key: TransferKey,
    name: String,
    size: u64,
    done: u64,
    ok: Option<bool>,
    path: Option<String>,
    error: Option<String>,
    /// Sending is paused (outgoing only).
    paused: bool,
    /// Unsent (outgoing) or stopped (incoming) from the island.
    cancelled: bool,
}

/// What a row shows that changes with progress, to update it without rebuilding the row (its
/// buttons would lose a click in progress).
struct RowParts {
    key: TransferKey,
    status: gtk::Label,
    bar: Option<gtk::ProgressBar>,
}

#[derive(Default)]
struct State {
    peer: String,
    can_send: bool,
    items: Vec<Item>,
    hovering: bool,
    /// Bumped by every change, so a pending hide only runs if nothing happened since.
    generation: u64,
}

struct Inner {
    window: gtk::Window,
    island: gtk::Box,
    content: gtk::Box,
    phase: std::cell::Cell<Phase>,
    state: RefCell<State>,
    rows: RefCell<Vec<RowParts>>,
    tx: async_channel::Sender<Msg>,
    /// X11 only: places the window, since there's no layer shell.
    placer: Option<crate::x11_drag::Placer>,
}

pub struct DropTray(Rc<Inner>);

impl DropTray {
    /// None where neither the layer shell nor X11 is available.
    pub fn new(tx: async_channel::Sender<Msg>) -> Option<DropTray> {
        let shell = crate::layer::get();
        let x11 = shell.is_none() && crate::desktop::session() == crate::desktop::Session::X11;
        if shell.is_none() && !x11 {
            tracing::info!("drop: no island on this desktop (no layer shell, not X11); files go on the viewer window");
            return None;
        }
        let Some(display) = gdk::Display::default() else {
            tracing::warn!("drop: no display for the island");
            return None;
        };
        let css = gtk::CssProvider::new();
        css.load_from_data(CSS);
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

        let window = gtk::Window::builder().decorated(false).resizable(false).title("Annex").build();
        window.add_css_class("annex-island-window");
        if !display.is_composited() {
            window.add_css_class("solid");
        }
        let composited = display.is_composited();
        let placer = match shell {
            Some(shell) => {
                tracing::info!(composited, "drop: island through the layer shell");
                shell.top_centre(&window);
                None
            }
            None => match crate::x11_drag::Placer::new(&window) {
                Some(p) => {
                    tracing::info!(composited, "drop: island as an X11 window");
                    Some(p)
                }
                None => {
                    tracing::warn!("drop: couldn't set up the X11 window for the island; no island");
                    return None;
                }
            },
        };
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_halign(gtk::Align::Center);
        let island = gtk::Box::new(gtk::Orientation::Vertical, 0);
        island.add_css_class("annex-island");
        island.set_halign(gtk::Align::Center);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(14);
        content.set_margin_bottom(14);
        content.set_margin_start(18);
        content.set_margin_end(18);
        island.append(&content);
        root.append(&island);
        if placer.is_some() {
            // X11: a window of fixed size, cut to the island's shape as it grows and shrinks.
            root.set_size_request(WINDOW_WIDTH, WINDOW_HEIGHT);
        }
        window.set_child(Some(&root));

        let inner = Rc::new(Inner {
            window,
            island,
            content,
            phase: std::cell::Cell::new(Phase::Hidden),
            state: RefCell::new(State::default()),
            rows: RefCell::new(Vec::new()),
            tx,
            placer,
        });
        if inner.placer.is_some() {
            let i = inner.clone();
            inner.island.add_tick_callback(move |island, _| {
                i.reshape(island);
                glib::ControlFlow::Continue
            });
            let (drags, rx) = async_channel::unbounded();
            crate::x11_drag::watch(drags);
            let i = inner.clone();
            crate::x11_drag::on_drag(rx, move |d| i.dragged(d));
        }

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        {
            let i = inner.clone();
            drop.connect_enter(move |_, _, _| {
                match i.phase.get() {
                    Phase::Hidden => {
                        i.set_phase(Phase::Glowing, "a drag reached the strip");
                        i.open_later(300);
                    }
                    Phase::Glowing => i.set_phase(Phase::Open, "a drag came onto the pill"),
                    _ => {}
                }
                gdk::DragAction::COPY
            });
        }
        {
            let i = inner.clone();
            drop.connect_leave(move |_| {
                // On X11 the drag watcher decides.
                if i.placer.is_none() && matches!(i.phase.get(), Phase::Glowing | Phase::Open) {
                    tracing::debug!("drop: the drag left the island");
                    i.hide_later(700);
                }
            });
        }
        {
            let i = inner.clone();
            drop.connect_drop(move |_, value, _, _| {
                let Ok(list) = value.get::<gdk::FileList>() else { return false };
                let files: Vec<PathBuf> =
                    list.files().iter().filter_map(|f| f.path()).flat_map(|p| files_in(&p)).collect();
                if files.is_empty() {
                    i.set_phase(Phase::Hidden, "dropped, but no files in it");
                    return false;
                }
                i.set_phase(Phase::Transfers, "files dropped on the island");
                let _ = i.tx.send_blocking(Msg::DropFiles(files));
                true
            });
        }
        root.add_controller(drop);

        let hover = gtk::EventControllerMotion::new();
        {
            let i = inner.clone();
            hover.connect_enter(move |_, _, _| i.state.borrow_mut().hovering = true);
        }
        {
            let i = inner.clone();
            hover.connect_leave(move |_| {
                i.state.borrow_mut().hovering = false;
                if i.phase.get() == Phase::Transfers && i.all_done() {
                    tracing::trace!("drop: the pointer left the finished transfers");
                    i.hide_later(2000);
                }
            });
        }
        root.add_controller(hover);
        Some(DropTray(inner))
    }

    /// Files can go to `peer` now; None: nothing connected with control.
    pub fn set_target(&self, peer: Option<String>) {
        let i = &self.0;
        let can = peer.is_some();
        {
            let mut s = i.state.borrow_mut();
            if let Some(p) = peer {
                s.peer = p;
            }
            if s.can_send == can {
                return;
            }
            s.can_send = can;
            tracing::info!(
                can_send = can,
                peer = s.peer,
                "drop: files can {}be sent",
                if can { "" } else { "no longer " }
            );
        }
        if !can && matches!(i.phase.get(), Phase::Glowing | Phase::Open) {
            i.set_phase(Phase::Hidden, "nothing connected with control");
        }
        i.update_visible();
    }

    pub fn started(&self, key: TransferKey, name: String, size: u64) {
        let i = &self.0;
        {
            let mut s = i.state.borrow_mut();
            s.items.retain(|x| x.key != key);
            s.items.push(Item {
                key,
                name,
                size,
                done: 0,
                ok: None,
                path: None,
                error: None,
                paused: false,
                cancelled: false,
            });
            while s.items.len() > 4 {
                let Some(old) = s.items.iter().position(|x| x.ok.is_some()) else { break };
                s.items.remove(old);
            }
        }
        i.set_phase(Phase::Transfers, "a transfer started");
    }

    pub fn progress(&self, key: TransferKey, done: u64) {
        let i = &self.0;
        if let Some(x) = i.state.borrow_mut().items.iter_mut().find(|x| x.key == key) {
            x.done = done;
        }
        if i.phase.get() == Phase::Transfers {
            i.refresh(key);
        }
    }

    pub fn ended(&self, key: TransferKey, ok: bool, path: Option<String>, error: Option<String>) {
        let i = &self.0;
        {
            let mut s = i.state.borrow_mut();
            let Some(x) = s.items.iter_mut().find(|x| x.key == key) else {
                tracing::debug!(?key, "drop: a transfer ended that isn't in the island");
                return;
            };
            // Unsent or stopped here: it already shows so, whatever the core says.
            if !x.cancelled {
                x.ok = Some(ok);
                x.error = error;
                if ok {
                    x.done = x.size;
                }
            }
            x.path = path;
            x.paused = false;
        }
        i.render();
        if i.all_done() {
            i.hide_later(5000);
        }
    }

    /// The core took a pause or resume (see [`Msg::PauseTransfer`]).
    pub fn set_paused(&self, key: TransferKey, paused: bool) {
        let i = &self.0;
        {
            let mut s = i.state.borrow_mut();
            let Some(x) = s.items.iter_mut().find(|x| x.key == key && x.ok.is_none()) else { return };
            x.paused = paused;
            // A paused file keeps the island open: a pending hide doesn't run.
            s.generation += 1;
        }
        i.render();
    }

    /// Unsent or stopped: shows so now, without waiting for the core's end of it.
    pub fn cancelled(&self, key: TransferKey) {
        let i = &self.0;
        {
            let mut s = i.state.borrow_mut();
            let Some(x) = s.items.iter_mut().find(|x| x.key == key && x.ok.is_none()) else { return };
            x.cancelled = true;
            x.paused = false;
            x.ok = Some(false);
        }
        i.render();
        if i.all_done() {
            i.hide_later(3000);
        }
    }
}

impl Inner {
    fn all_done(&self) -> bool {
        self.state.borrow().items.iter().all(|x| x.ok.is_some())
    }

    /// `why`: what moved it, for the log.
    fn set_phase(self: &Rc<Self>, phase: Phase, why: &str) {
        if phase != self.phase.get() {
            tracing::debug!(from = ?self.phase.get(), to = ?phase, "drop: {why}");
        }
        self.state.borrow_mut().generation += 1;
        self.phase.set(phase);
        for c in ["glowing", "open", "transfers"] {
            self.island.remove_css_class(c);
        }
        match phase {
            Phase::Hidden => {
                self.state.borrow_mut().items.retain(|x| x.ok.is_none());
            }
            Phase::Glowing => self.island.add_css_class("glowing"),
            Phase::Open => self.island.add_css_class("open"),
            Phase::Transfers => self.island.add_css_class("transfers"),
        }
        self.render();
        self.update_visible();
    }

    /// The strip stays while files can be sent (Wayland only; X11 sees drags without it); the
    /// island shows while it has something to say.
    fn update_visible(&self) {
        let strip = self.placer.is_none() && self.state.borrow().can_send;
        let show = strip || self.phase.get() != Phase::Hidden;
        if show != self.window.is_visible() {
            tracing::trace!(show, strip, "drop: island window");
            self.window.set_visible(show);
            if show {
                self.place();
            }
        }
    }

    /// X11: only the island (and its glow, where windows can be see-through) shows and takes clicks.
    fn reshape(&self, island: &gtk::Box) {
        let (Some(p), Some(b)) = (&self.placer, island.compute_bounds(&self.window)) else { return };
        let pad = if self.window.has_css_class("solid") { 0.0 } else { 26.0 };
        p.shape(
            &self.window,
            (b.x() - pad) as i32,
            b.y() as i32,
            (b.width() + 2.0 * pad) as i32,
            (b.height() + pad) as i32,
            // Without see-through windows, the shape is what rounds the corners.
            if pad > 0.0 { 0 } else { 20 },
        );
    }

    fn place(&self) {
        if let Some(p) = &self.placer {
            p.place(&self.window, WINDOW_WIDTH);
        }
    }

    /// X11: glow when files are picked up, open near the top, go when they're let go.
    fn dragged(self: &Rc<Self>, d: crate::x11_drag::Drag) {
        use crate::x11_drag::{Drag, Placer};
        match d {
            Drag::Started => {
                let can = self.state.borrow().can_send;
                if can && self.phase.get() == Phase::Hidden {
                    self.set_phase(Phase::Glowing, "files picked up (XdndSelection)");
                } else {
                    tracing::trace!(can_send = can, phase = ?self.phase.get(), "drop: files picked up; island stays");
                }
            }
            Drag::Moved(x, y) => {
                let phase = self.phase.get();
                if !matches!(phase, Phase::Glowing | Phase::Open) {
                    return;
                }
                let Some((cx, cy)) = Placer::top_centre(&self.window) else { return };
                let scale = f64::from(self.window.scale_factor());
                let distance = f64::from(x - cx).hypot(f64::from(y - cy));
                let near = distance < 160.0 * scale;
                if near && phase == Phase::Glowing {
                    self.set_phase(Phase::Open, &format!("the drag came near the top ({distance:.0} px)"));
                } else if !near && phase == Phase::Open {
                    self.set_phase(Phase::Glowing, &format!("the drag moved away ({distance:.0} px)"));
                }
            }
            // A drop on the island arrives just after the button goes up.
            Drag::Ended => {
                if matches!(self.phase.get(), Phase::Glowing | Phase::Open) {
                    tracing::debug!("drop: the drag ended");
                    self.hide_later(350);
                }
            }
        }
    }

    /// Opens the pill if the drag is still on it after `ms`.
    fn open_later(self: &Rc<Self>, ms: u64) {
        let generation = self.state.borrow().generation;
        let me = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
            if me.state.borrow().generation == generation && me.phase.get() == Phase::Glowing {
                me.set_phase(Phase::Open, &format!("the drag stayed on the pill for {ms} ms"));
            }
        });
    }

    fn hide_later(self: &Rc<Self>, ms: u64) {
        let generation = self.state.borrow().generation;
        let me = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
            let s = me.state.borrow();
            if s.generation != generation || s.hovering || s.items.iter().any(|x| x.paused) {
                return;
            }
            drop(s);
            me.set_phase(Phase::Hidden, &format!("nothing happened for {ms} ms"));
        });
    }

    /// Updates one row's status and bar in place; rebuilds all of them if it isn't shown yet.
    fn refresh(&self, key: TransferKey) {
        let shown = {
            let s = self.state.borrow();
            let rows = self.rows.borrow();
            match (s.items.iter().find(|x| x.key == key), rows.iter().find(|r| r.key == key)) {
                (Some(x), Some(r)) => {
                    r.status.set_text(&status_text(x, &s.peer));
                    if let Some(bar) = &r.bar {
                        bar.set_fraction(fraction(x));
                    }
                    true
                }
                _ => false,
            }
        };
        if !shown {
            self.render();
        }
    }

    fn render(&self) {
        while let Some(c) = self.content.first_child() {
            self.content.remove(&c);
        }
        self.rows.borrow_mut().clear();
        // Hidden: only the strip, without the content's padding.
        self.content.set_visible(!matches!(self.phase.get(), Phase::Hidden | Phase::Glowing));
        let s = self.state.borrow();
        match self.phase.get() {
            Phase::Hidden | Phase::Glowing => {}
            Phase::Open => {
                let icon = gtk::Image::from_icon_name("document-save-symbolic");
                icon.set_pixel_size(28);
                self.content.append(&icon);
                let title = gtk::Label::new(Some(&format!("Drop to send to {}", s.peer)));
                title.add_css_class("title");
                self.content.append(&title);
                let note = gtk::Label::new(Some("Same quality, byte for byte"));
                note.add_css_class("dim");
                self.content.append(&note);
                self.content.set_valign(gtk::Align::Center);
            }
            Phase::Transfers => {
                let mut rows = self.rows.borrow_mut();
                for x in &s.items {
                    let (row, parts) = row(x, &s.peer, &self.tx);
                    self.content.append(&row);
                    rows.push(parts);
                }
            }
        }
    }
}

fn fraction(x: &Item) -> f64 {
    if x.size == 0 { 1.0 } else { (x.done as f64 / x.size as f64).min(1.0) }
}

fn status_text(x: &Item, peer: &str) -> String {
    let incoming = x.key.incoming;
    let total = glib::format_size(x.size);
    match x.ok {
        Some(true) if incoming => format!("Received from {peer} · {total}"),
        Some(true) => format!("Sent to {peer} · {total}"),
        Some(false) if x.cancelled => (if incoming { "Stopped" } else { "Unsent" }).into(),
        Some(false) => x.error.clone().unwrap_or_else(|| "Didn't arrive".into()),
        None if x.paused => format!("Paused · {} of {total}", glib::format_size(x.done)),
        None => format!("{} {} of {total}", if incoming { "Receiving" } else { "Sending" }, glib::format_size(x.done)),
    }
}

/// A small round icon button, styled like "Show".
fn round_button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("round");
    b.set_tooltip_text(Some(tip));
    b.set_valign(gtk::Align::Center);
    b
}

fn row(x: &Item, peer: &str, tx: &async_channel::Sender<Msg>) -> (gtk::Box, RowParts) {
    let incoming = x.key.incoming;
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let badge = gtk::Label::new(Some(match x.ok {
        Some(true) => "✓",
        Some(false) => "✕",
        None if x.paused => "\u{23F8}\u{FE0E}",
        None if incoming => "↓",
        None => "↑",
    }));
    badge.add_css_class("badge");
    if x.ok == Some(true) {
        badge.add_css_class("done");
    }
    row.append(&badge);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    let name = gtk::Label::new(Some(&x.name));
    name.add_css_class("title");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    text.append(&name);
    let status = gtk::Label::new(Some(&status_text(x, peer)));
    status.add_css_class("dim");
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&status);
    let bar = x.ok.is_none().then(|| {
        let bar = gtk::ProgressBar::new();
        bar.set_fraction(fraction(x));
        // Paused: dimmed, like the Mac's ring.
        bar.set_opacity(if x.paused { 0.35 } else { 1.0 });
        text.append(&bar);
        bar
    });
    row.append(&text);
    if x.ok.is_none() {
        let key = x.key;
        if !incoming {
            let pause = !x.paused;
            let b = if pause {
                round_button("media-playback-pause-symbolic", "Pause")
            } else {
                round_button("media-playback-start-symbolic", "Resume")
            };
            let tx = tx.clone();
            b.connect_clicked(move |_| {
                tracing::info!(?key, "drop: {}", if pause { "pause" } else { "resume" });
                let _ = tx.send_blocking(Msg::PauseTransfer(key, pause));
            });
            row.append(&b);
        }
        let b = round_button("window-close-symbolic", if incoming { "Stop" } else { "Unsend" });
        let tx = tx.clone();
        b.connect_clicked(move |_| {
            tracing::info!(?key, "drop: {}", if incoming { "stop" } else { "unsend" });
            let _ = tx.send_blocking(Msg::CancelTransfer(key));
        });
        row.append(&b);
    }
    if let (true, Some(true), Some(path)) = (incoming, x.ok, &x.path) {
        let show = gtk::Button::with_label("Show");
        show.set_valign(gtk::Align::Center);
        let folder = Path::new(path).parent().map(gio::File::for_path);
        show.connect_clicked(move |_| {
            if let Some(f) = &folder {
                tracing::info!(folder = %f.uri(), "drop: Show pressed");
                if let Err(e) = gio::AppInfo::launch_default_for_uri(&f.uri(), None::<&gio::AppLaunchContext>) {
                    tracing::warn!("drop: couldn't open the folder: {e}");
                }
            }
        });
        row.append(&show);
    }
    (row, RowParts { key: x.key, status, bar })
}

/// A file, or every file inside a folder (hidden ones skipped).
pub fn files_in(path: &Path) -> Vec<PathBuf> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %path.display(), "drop: can't read what was dropped: {e}");
            return Vec::new();
        }
    };
    if meta.is_file() {
        return vec![path.to_path_buf()];
    }
    let mut out = Vec::new();
    if meta.is_dir()
        && let Ok(entries) = std::fs::read_dir(path)
    {
        for e in entries.flatten() {
            if !e.file_name().to_string_lossy().starts_with('.') {
                out.extend(files_in(&e.path()));
            }
        }
    }
    out
}
