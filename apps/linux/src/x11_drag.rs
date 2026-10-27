//! The drop island on X11 (Linux Mint, XFCE, MATE): X11 has no layer shell, but it does let Annex
//! see a drag start in any app and place its own window anywhere, so the island glows as soon as
//! files are picked up, like the Mac's notch.
//!
//! A drag starts when an app takes the `XdndSelection`; XFixes reports that. Annex then follows
//! the pointer until the buttons are let go.

use std::cell::Cell;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{self, AtomEnum, ConnectionExt as _, KeyButMask};
use x11rb::rust_connection::RustConnection;

pub enum Drag {
    /// Files were picked up somewhere.
    Started,
    /// The pointer, in X screen pixels.
    Moved(i32, i32),
    Ended,
}

/// Watches for drags on its own thread and reports them to the GTK thread.
pub fn watch(tx: async_channel::Sender<Drag>) {
    std::thread::Builder::new()
        .name("x11 drags".into())
        .spawn(move || {
            if let Err(e) = run(&tx) {
                tracing::warn!("not watching drags: {e}");
            }
        })
        .ok();
}

fn run(tx: &async_channel::Sender<Drag>) -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    conn.xfixes_query_version(5, 0)?.reply()?;
    let atom = |name: &[u8]| conn.intern_atom(false, name).map(|c| c.reply().map(|r| r.atom));
    let selection = atom(b"XdndSelection")??;
    let type_list = atom(b"XdndTypeList")??;
    let uri_list = atom(b"text/uri-list")??;
    conn.xfixes_select_selection_input(root, selection, SelectionEventMask::SET_SELECTION_OWNER)?;
    conn.flush()?;
    loop {
        let Event::XfixesSelectionNotify(e) = conn.wait_for_event()? else { continue };
        if e.owner == x11rb::NONE {
            continue;
        }
        // Give the app a moment to list what it's dragging.
        std::thread::sleep(Duration::from_millis(30));
        if !offers_files(&conn, e.owner, type_list, uri_list) || !buttons_down(&conn, root)?.0 {
            continue;
        }
        if tx.send_blocking(Drag::Started).is_err() {
            return Ok(());
        }
        let mut last = (i32::MIN, i32::MIN);
        loop {
            std::thread::sleep(Duration::from_millis(40));
            let (down, at) = buttons_down(&conn, root)?;
            if !down {
                break;
            }
            if at != last {
                last = at;
                let _ = tx.send_blocking(Drag::Moved(at.0, at.1));
            }
        }
        let _ = tx.send_blocking(Drag::Ended);
        // Owner changes during the drag were the same drag.
        while conn.poll_for_event()?.is_some() {}
    }
}

/// Whether the drag carries files. Apps offering more than three kinds list them on their window;
/// others are let through, since a short list is usually files.
fn offers_files(conn: &RustConnection, owner: u32, type_list: u32, uri_list: u32) -> bool {
    let Ok(Ok(reply)) = conn.get_property(false, owner, type_list, AtomEnum::ATOM, 0, 64).map(|c| c.reply()) else {
        return true;
    };
    match reply.value32() {
        Some(mut atoms) if reply.value_len > 0 => atoms.any(|a| a == uri_list),
        _ => true,
    }
}

fn buttons_down(conn: &RustConnection, root: u32) -> Result<(bool, (i32, i32)), Box<dyn std::error::Error>> {
    let p = conn.query_pointer(root)?.reply()?;
    let down = p.mask.intersects(KeyButMask::BUTTON1 | KeyButMask::BUTTON2 | KeyButMask::BUTTON3);
    Ok((down, (p.root_x.into(), p.root_y.into())))
}

/// Keeps a GTK window at the top centre of the main screen, above everything, outside the window
/// manager (no title bar, no taskbar entry, never focused).
pub struct Placer {
    conn: RustConnection,
    shaped: Cell<[i32; 4]>,
}

impl Placer {
    /// Call on a window that isn't shown yet.
    pub fn new(window: &gtk::Window) -> Option<Placer> {
        let (conn, _) = x11rb::connect(None).ok()?;
        WidgetExt::realize(window);
        let xid = xid(window)?;
        let attrs = xproto::ChangeWindowAttributesAux::new().override_redirect(1);
        conn.change_window_attributes(xid, &attrs).ok()?;
        conn.flush().ok()?;
        Some(Placer { conn, shaped: Cell::new([0; 4]) })
    }

    /// The top centre of the main screen, in X pixels.
    pub fn top_centre(window: &gtk::Window) -> Option<(i32, i32)> {
        let (r, scale) = monitor(window)?;
        Some(((r.x() + r.width() / 2) * scale, r.y() * scale))
    }

    /// Moves the window, `width` GTK pixels wide, to the top centre and above everything.
    pub fn place(&self, window: &gtk::Window, width: i32) {
        let (Some(xid), Some((r, scale))) = (xid(window), monitor(window)) else { return };
        let x = (r.x() + (r.width() - width) / 2) * scale;
        let at = xproto::ConfigureWindowAux::new().x(x).y(r.y() * scale).stack_mode(xproto::StackMode::ABOVE);
        if self.conn.configure_window(xid, &at).is_ok() {
            let _ = self.conn.flush();
        }
    }
}

impl Placer {
    /// Cuts the window to one rectangle (in GTK pixels): it shows and takes clicks only there.
    pub fn shape(&self, window: &gtk::Window, x: i32, y: i32, width: i32, height: i32) {
        let scale = window.scale_factor();
        let r = [x * scale, y * scale, width.max(1) * scale, height.max(1) * scale];
        if r == self.shaped.get() {
            return;
        }
        let Some(xid) = xid(window) else { return };
        self.shaped.set(r);
        let rect = xproto::Rectangle { x: r[0] as i16, y: r[1] as i16, width: r[2] as u16, height: r[3] as u16 };
        for kind in [shape::SK::BOUNDING, shape::SK::INPUT] {
            let _ =
                self.conn.shape_rectangles(shape::SO::SET, kind, xproto::ClipOrdering::UNSORTED, xid, 0, 0, &[rect]);
        }
        let _ = self.conn.flush();
    }
}

fn xid(window: &gtk::Window) -> Option<u32> {
    let surface = window.surface()?;
    Some(surface.downcast_ref::<gdk4_x11::X11Surface>()?.xid() as u32)
}

fn monitor(window: &gtk::Window) -> Option<(gdk::Rectangle, i32)> {
    let display = WidgetExt::display(window);
    let monitor = match display.downcast_ref::<gdk4_x11::X11Display>() {
        Some(x) => x.primary_monitor(),
        None => display.monitors().item(0)?.downcast::<gdk::Monitor>().ok()?,
    };
    Some((monitor.geometry(), window.scale_factor()))
}

/// Runs `f` on the GTK thread for each drag report.
pub fn on_drag(rx: async_channel::Receiver<Drag>, f: impl Fn(Drag) + 'static) {
    glib::spawn_future_local(async move {
        while let Ok(d) = rx.recv().await {
            f(d);
        }
    });
}
