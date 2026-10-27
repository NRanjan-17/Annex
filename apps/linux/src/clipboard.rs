//! The clipboard between the two computers: what's copied here goes to the other one, and what
//! it copied is put here.
//!
//! On Wayland, GTK sees the clipboard only while one of Annex's windows is in front, so
//! `wl-paste --watch` and `wl-copy` (wl-clipboard) are used when they're installed. Otherwise GTK,
//! which works fully on X11.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use crate::model::Msg;

pub const TEXT: &str = "text/plain;charset=utf-8";
pub const PNG: &str = "image/png";

/// KeePassXC and KDE mark passwords with this; they never leave this computer.
const SECRET: &str = "x-kde-passwordManagerHint";

pub struct Clipboard {
    backend: Backend,
    /// What was last seen on the clipboard, so writing what the other computer copied doesn't
    /// send it straight back.
    seen: Arc<Mutex<u64>>,
    tx: async_channel::Sender<Msg>,
}

enum Backend {
    /// The watcher, and whether Annex is stopping it.
    WlClipboard(Child, Arc<AtomicBool>),
    Gtk(gdk::Clipboard, Option<glib::SignalHandlerId>),
}

fn digest(data: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

pub fn wl_clipboard() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        && Command::new("wl-paste").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

/// What's on the clipboard now, through wl-paste.
fn wl_read() -> Option<(String, Vec<u8>)> {
    let types = match Command::new("wl-paste").arg("--list-types").stderr(Stdio::null()).output() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("clipboard: wl-paste didn't run: {e}");
            return None;
        }
    };
    let types = String::from_utf8_lossy(&types.stdout);
    let has = |t: &str| types.lines().any(|l| l.trim() == t);
    if has(SECRET) {
        tracing::debug!("clipboard: copied here; a password, kept on this computer");
        return None;
    }
    // A copied file also offers its icon; send its name, not the icon.
    let (mime, arg) = if has(PNG) && !has("text/uri-list") {
        (PNG, PNG)
    } else if types.lines().any(|l| l.starts_with("text/plain") || l == "UTF8_STRING" || l == "STRING") {
        (TEXT, "text")
    } else {
        tracing::debug!(types = types.lines().count(), "clipboard: copied here; not text or a picture, not shared");
        return None;
    };
    let out = match Command::new("wl-paste").args(["--no-newline", "--type", arg]).stderr(Stdio::null()).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("clipboard: wl-paste didn't run: {e}");
            return None;
        }
    };
    if !out.status.success() || out.stdout.is_empty() {
        tracing::debug!(mime, status = %out.status, "clipboard: wl-paste gave nothing");
        return None;
    }
    Some((mime.to_string(), out.stdout))
}

/// Reads the GTK clipboard and sends what's on it: `Msg::ClipboardNow` if asked for, else `Msg::Copied`.
fn gtk_read(b: &gdk::Clipboard, tx: &async_channel::Sender<Msg>, current: bool) {
    let formats = b.formats();
    if formats.contain_mime_type(SECRET) {
        tracing::debug!("clipboard: copied here; a password, kept on this computer");
        return;
    }
    let (b, tx) = (b.clone(), tx.clone());
    let image = formats.contain_mime_type(PNG) && !formats.contain_mime_type("text/uri-list");
    glib::spawn_future_local(async move {
        let item = if image {
            b.read_texture_future().await.ok().flatten().map(|t| (PNG, t.save_to_png_bytes().to_vec()))
        } else {
            b.read_text_future().await.ok().flatten().map(|s| (TEXT, s.as_bytes().to_vec()))
        };
        let Some((mime, data)) = item.filter(|(_, d)| !d.is_empty()) else {
            tracing::debug!(image, "clipboard: GTK read nothing usable");
            return;
        };
        let mime = mime.to_string();
        let _ = tx.send(if current { Msg::ClipboardNow { mime, data } } else { Msg::Copied { mime, data } }).await;
    });
}

impl Clipboard {
    /// Through wl-clipboard when it's installed and works here, else GTK.
    pub fn start(tx: async_channel::Sender<Msg>) -> Option<Clipboard> {
        if wl_clipboard()
            && let Some(c) = Self::start_wl(tx.clone())
        {
            return Some(c);
        }
        Self::start_gtk(tx)
    }

    fn start_wl(tx: async_channel::Sender<Msg>) -> Option<Clipboard> {
        let seen = Arc::new(Mutex::new(0));
        let stopping = Arc::new(AtomicBool::new(false));
        // Prints a line each time something is copied (and once for what's there now).
        let mut child = match Command::new("wl-paste")
            .args(["--watch", "echo", ""])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("clipboard: wl-paste --watch didn't start: {e}");
                return None;
            }
        };
        let out = child.stdout.take()?;
        let (seen2, stopping2, tx2) = (seen.clone(), stopping.clone(), tx.clone());
        std::thread::Builder::new()
            .name("annex-clipboard".into())
            .spawn(move || {
                for _ in BufReader::new(out).lines().map_while(Result::ok) {
                    let Some((mime, data)) = wl_read() else { continue };
                    let d = digest(&data);
                    if std::mem::replace(&mut *seen2.lock().unwrap(), d) == d {
                        tracing::trace!("clipboard: what the other computer copied, not sent back");
                        continue;
                    }
                    if tx2.send_blocking(Msg::Copied { mime, data }).is_err() {
                        return;
                    }
                }
                // GNOME has no clipboard access for other programs, so wl-paste stops at once there.
                if !stopping2.load(Ordering::Relaxed) {
                    tracing::info!("clipboard: wl-paste --watch stopped by itself");
                    let _ = tx2.send_blocking(Msg::ClipboardWatchEnded);
                }
            })
            .ok()?;
        tracing::info!("clipboard: through wl-clipboard");
        Some(Clipboard { backend: Backend::WlClipboard(child, stopping), seen, tx })
    }

    pub fn start_gtk(tx: async_channel::Sender<Msg>) -> Option<Clipboard> {
        let Some(display) = gdk::Display::default() else {
            tracing::warn!("clipboard: no display for GTK's clipboard");
            return None;
        };
        let board = display.clipboard();
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            tracing::info!("clipboard: through GTK; it's shared only while Annex's window is in front");
        } else {
            tracing::info!("clipboard: through GTK (X11)");
        }
        let tx2 = tx.clone();
        let id = board.connect_changed(move |b| {
            // Set by Annex itself: what the other computer copied.
            if !b.is_local() {
                gtk_read(b, &tx2, false);
            } else {
                tracing::trace!("clipboard: set by Annex itself, not sent back");
            }
        });
        Some(Clipboard { backend: Backend::Gtk(board, Some(id)), seen: Arc::new(Mutex::new(0)), tx })
    }

    /// Reads what's on the clipboard now; it arrives as `Msg::ClipboardNow`.
    pub fn read_now(&self) {
        match &self.backend {
            Backend::WlClipboard(..) => {
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    if let Some((mime, data)) = wl_read() {
                        let _ = tx.send_blocking(Msg::ClipboardNow { mime, data });
                    }
                });
            }
            Backend::Gtk(board, _) => gtk_read(board, &self.tx, true),
        }
    }

    /// Puts what the other computer copied on this computer's clipboard.
    pub fn apply(&self, mime: &str, data: &[u8]) {
        let text = mime.starts_with("text/plain");
        if !text && mime != PNG {
            tracing::debug!(mime, "clipboard: received a kind Annex doesn't put on the clipboard");
            return;
        }
        *self.seen.lock().unwrap() = digest(data);
        match &self.backend {
            Backend::WlClipboard(..) => {
                let mut cmd = Command::new("wl-copy");
                if !text {
                    cmd.args(["--type", PNG]);
                }
                // wl-copy stays in the background to serve the clipboard; this waits only for it to start.
                let mut child = match cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("clipboard: wl-copy didn't run: {e}");
                        return;
                    }
                };
                if let Some(mut stdin) = child.stdin.take()
                    && let Err(e) = stdin.write_all(data)
                {
                    tracing::warn!("clipboard: wl-copy didn't take it: {e}");
                }
                match child.wait() {
                    Ok(s) if !s.success() => tracing::warn!("clipboard: wl-copy failed ({s})"),
                    Err(e) => tracing::warn!("clipboard: wl-copy: {e}"),
                    Ok(_) => tracing::debug!(mime, bytes = data.len(), "clipboard: received and put here (wl-copy)"),
                }
            }
            Backend::Gtk(board, _) if text => {
                board.set_text(&String::from_utf8_lossy(data));
                tracing::debug!(mime, bytes = data.len(), "clipboard: received and put here (GTK)");
            }
            Backend::Gtk(board, _) => match gdk::Texture::from_bytes(&glib::Bytes::from(data)) {
                Ok(t) => {
                    board.set_texture(&t);
                    tracing::debug!(mime, bytes = data.len(), "clipboard: received and put here (GTK)");
                }
                Err(e) => tracing::warn!(bytes = data.len(), "clipboard: the received picture didn't load: {e}"),
            },
        }
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        match &mut self.backend {
            Backend::WlClipboard(child, stopping) => {
                stopping.store(true, Ordering::Relaxed);
                let _ = child.kill();
                let _ = child.wait();
            }
            Backend::Gtk(board, id) => {
                if let Some(id) = id.take() {
                    board.disconnect(id);
                }
            }
        }
    }
}
