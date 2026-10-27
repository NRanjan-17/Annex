//! The drop island: a black shape hanging from the top centre of the screen, like the Mac's notch.
//! While another computer is connected, dragging files to the top edge pops it open, and dropping
//! sends them; files going and coming show in it. Needs the layer shell ([`crate::layer`]); without
//! it, files are dropped on the viewer window and arrivals show as notifications.
//!
//! Wayland doesn't let an app watch the pointer, so a 3-pixel strip at the top centre waits for
//! the drag instead of the island glowing as soon as one starts.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::model::Msg;

const CSS: &str = "
window.annex-island-window { background: transparent; }
.annex-island {
  background: black; border-radius: 0 0 20px 20px; border: 0 solid transparent;
  min-width: 300px; min-height: 3px; margin: 0; opacity: 0.01;
  transition: min-width 260ms ease-out, min-height 260ms ease-out, margin 260ms ease-out,
    opacity 200ms ease-out, box-shadow 260ms ease-out;
}
.annex-island.open, .annex-island.transfers {
  opacity: 1; margin: 0 26px 26px 26px;
  transition: min-width 420ms cubic-bezier(0.34, 1.56, 0.64, 1), min-height 420ms cubic-bezier(0.34, 1.56, 0.64, 1),
    margin 300ms ease-out, opacity 150ms ease-out, box-shadow 300ms ease-out;
}
.annex-island.open { min-width: 380px; min-height: 150px; }
.annex-island.transfers { min-width: 380px; }
.annex-island.open {
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
.annex-island label { color: white; }
.annex-island .title { font-weight: 600; }
.annex-island .dim { color: rgba(255, 255, 255, 0.6); font-size: 0.9em; }
.annex-island .badge { font-weight: 700; min-width: 28px; }
.annex-island .badge.done { color: #3dd185; }
.annex-island progressbar trough { background: rgba(255, 255, 255, 0.15); min-height: 4px; border-radius: 2px; }
.annex-island progressbar progress { background: #3dd185; min-height: 4px; border-radius: 2px; }
.annex-island button { background: rgba(255, 255, 255, 0.15); color: white; border: none; box-shadow: none;
  border-radius: 12px; padding: 2px 12px; min-height: 0; }
.annex-island button:hover { background: rgba(255, 255, 255, 0.25); }
";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Only the strip that waits for a drag.
    Hidden,
    Open,
    Transfers,
}

struct Item {
    key: (u64, u64, bool),
    name: String,
    size: u64,
    done: u64,
    ok: Option<bool>,
    path: Option<String>,
    error: Option<String>,
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
    tx: async_channel::Sender<Msg>,
}

pub struct DropTray(Rc<Inner>);

impl DropTray {
    /// None where the layer shell isn't available.
    pub fn new(tx: async_channel::Sender<Msg>) -> Option<DropTray> {
        let shell = crate::layer::get()?;
        let display = gdk::Display::default()?;
        let css = gtk::CssProvider::new();
        css.load_from_data(CSS);
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

        let window = gtk::Window::builder().decorated(false).resizable(false).title("Annex").build();
        window.add_css_class("annex-island-window");
        shell.top_centre(&window);
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
        window.set_child(Some(&root));

        let inner = Rc::new(Inner {
            window,
            island,
            content,
            phase: std::cell::Cell::new(Phase::Hidden),
            state: RefCell::new(State::default()),
            tx,
        });

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        {
            let i = inner.clone();
            drop.connect_enter(move |_, _, _| {
                if i.phase.get() == Phase::Hidden {
                    i.set_phase(Phase::Open);
                }
                gdk::DragAction::COPY
            });
        }
        {
            let i = inner.clone();
            drop.connect_leave(move |_| {
                if i.phase.get() == Phase::Open {
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
                    i.set_phase(Phase::Hidden);
                    return false;
                }
                i.set_phase(Phase::Transfers);
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
        }
        if !can && i.phase.get() == Phase::Open {
            i.set_phase(Phase::Hidden);
        }
        i.update_visible();
    }

    pub fn started(&self, session: u64, id: u64, incoming: bool, name: String, size: u64) {
        let i = &self.0;
        {
            let mut s = i.state.borrow_mut();
            let key = (session, id, incoming);
            s.items.retain(|x| x.key != key);
            s.items.push(Item { key, name, size, done: 0, ok: None, path: None, error: None });
            while s.items.len() > 4 {
                let Some(old) = s.items.iter().position(|x| x.ok.is_some()) else { break };
                s.items.remove(old);
            }
        }
        i.set_phase(Phase::Transfers);
    }

    pub fn progress(&self, session: u64, id: u64, incoming: bool, done: u64) {
        let i = &self.0;
        if let Some(x) = i.state.borrow_mut().items.iter_mut().find(|x| x.key == (session, id, incoming)) {
            x.done = done;
        }
        if i.phase.get() == Phase::Transfers {
            i.render();
        }
    }

    pub fn ended(&self, session: u64, id: u64, incoming: bool, ok: bool, path: Option<String>, error: Option<String>) {
        let i = &self.0;
        {
            let mut s = i.state.borrow_mut();
            let Some(x) = s.items.iter_mut().find(|x| x.key == (session, id, incoming)) else { return };
            x.ok = Some(ok);
            x.path = path;
            x.error = error;
            if ok {
                x.done = x.size;
            }
        }
        i.render();
        if i.all_done() {
            i.hide_later(5000);
        }
    }
}

impl Inner {
    fn all_done(&self) -> bool {
        self.state.borrow().items.iter().all(|x| x.ok.is_some())
    }

    fn set_phase(self: &Rc<Self>, phase: Phase) {
        self.state.borrow_mut().generation += 1;
        self.phase.set(phase);
        for c in ["open", "transfers"] {
            self.island.remove_css_class(c);
        }
        match phase {
            Phase::Hidden => {
                self.state.borrow_mut().items.retain(|x| x.ok.is_none());
            }
            Phase::Open => self.island.add_css_class("open"),
            Phase::Transfers => self.island.add_css_class("transfers"),
        }
        self.render();
        self.update_visible();
    }

    /// The strip stays while files can be sent; the island shows while it has something to say.
    fn update_visible(&self) {
        let show = self.state.borrow().can_send || self.phase.get() != Phase::Hidden;
        if show != self.window.is_visible() {
            self.window.set_visible(show);
        }
    }

    fn hide_later(self: &Rc<Self>, ms: u64) {
        let generation = self.state.borrow().generation;
        let me = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
            let s = me.state.borrow();
            if s.generation != generation || s.hovering {
                return;
            }
            drop(s);
            me.set_phase(Phase::Hidden);
        });
    }

    fn render(&self) {
        while let Some(c) = self.content.first_child() {
            self.content.remove(&c);
        }
        // Hidden: only the strip, without the content's padding.
        self.content.set_visible(self.phase.get() != Phase::Hidden);
        let s = self.state.borrow();
        match self.phase.get() {
            Phase::Hidden => {}
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
                for x in &s.items {
                    self.content.append(&row(x, &s.peer));
                }
            }
        }
    }
}

fn row(x: &Item, peer: &str) -> gtk::Box {
    let incoming = x.key.2;
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let badge = gtk::Label::new(Some(match x.ok {
        Some(true) => "✓",
        Some(false) => "✕",
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
    let total = glib::format_size(x.size);
    let status = match x.ok {
        Some(true) if incoming => format!("Received from {peer} · {total}"),
        Some(true) => format!("Sent to {peer} · {total}"),
        Some(false) => x.error.clone().unwrap_or_else(|| "Didn't arrive".into()),
        None => format!("{} {} of {total}", if incoming { "Receiving" } else { "Sending" }, glib::format_size(x.done)),
    };
    let status = gtk::Label::new(Some(&status));
    status.add_css_class("dim");
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&status);
    if x.ok.is_none() {
        let bar = gtk::ProgressBar::new();
        bar.set_fraction(if x.size == 0 { 1.0 } else { (x.done as f64 / x.size as f64).min(1.0) });
        text.append(&bar);
    }
    row.append(&text);
    if let (true, Some(true), Some(path)) = (incoming, x.ok, &x.path) {
        let show = gtk::Button::with_label("Show");
        show.set_valign(gtk::Align::Center);
        let folder = Path::new(path).parent().map(gio::File::for_path);
        show.connect_clicked(move |_| {
            if let Some(f) = &folder {
                let _ = gio::AppInfo::launch_default_for_uri(&f.uri(), None::<&gio::AppLaunchContext>);
            }
        });
        row.append(&show);
    }
    row
}

/// A file, or every file inside a folder (hidden ones skipped).
pub fn files_in(path: &Path) -> Vec<PathBuf> {
    let Ok(meta) = std::fs::metadata(path) else { return Vec::new() };
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
