//! The viewer's mouse and keyboard on this computer: XTest on X11, the RemoteDesktop portal on
//! GNOME and KDE, and wlroots' virtual pointer and keyboard on labwc and sway.

use std::io::Write;
use std::os::fd::AsFd;

use annex_media::geometry::{Rect, denormalize_point};
use annex_session::{InputEvent, MouseButton, keys};
use wayland_client::Proxy as _;
use wayland_client::protocol::{wl_output, wl_pointer};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;

use crate::portal::PortalInput;
use crate::wl::Wayland;

/// Linux button codes (linux/input-event-codes.h).
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// One wheel click is about this many pixels of scrolling.
const WHEEL_PX: i32 = 40;

fn evdev_button(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => BTN_LEFT,
        MouseButton::Right => BTN_RIGHT,
        MouseButton::Middle => BTN_MIDDLE,
    }
}

pub enum Injector {
    X11(Box<X11>),
    Wlr(Box<Wlr>),
    Portal(Portal),
}

impl Injector {
    pub fn inject(&mut self, e: InputEvent) {
        let r = match self {
            Injector::X11(x) => x.inject(e),
            Injector::Wlr(w) => w.inject(e),
            Injector::Portal(p) => {
                p.inject(e);
                Ok(())
            }
        };
        if let Err(err) = r {
            failed(&err);
        }
    }
}

/// An input event that didn't reach the desktop: a warning when the reason is new, then trace
/// while it repeats (it can fail for every key or move). Never says which key.
pub fn failed(why: &str) {
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if *last == why {
        tracing::trace!("input not injected: {why}");
    } else {
        tracing::warn!("input not injected: {why}");
        *last = why.to_string();
    }
}

// ---- X11 ---------------------------------------------------------------------------------

pub struct X11 {
    conn: x11rb::rust_connection::RustConnection,
    root: xproto::Window,
    /// The shared part of the screen, in root-window pixels.
    area: Rect,
    scroll: (i32, i32),
}

impl X11 {
    pub fn new(area: Option<Rect>) -> Result<X11, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("no X display: {e}"))?;
        let s = &conn.setup().roots[screen];
        let root = s.root;
        let area = area.unwrap_or(Rect { x: 0, y: 0, w: s.width_in_pixels as i32, h: s.height_in_pixels as i32 });
        conn.xtest_get_version(2, 2).map_err(|e| e.to_string())?.reply().map_err(|_| "XTest isn't available")?;
        tracing::debug!(?area, "input: XTest ready");
        Ok(X11 { conn, root, area, scroll: (0, 0) })
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<(), String> {
        self.conn.xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, self.root, x, y, 0).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn inject(&mut self, e: InputEvent) -> Result<(), String> {
        match e {
            InputEvent::MouseMove { x, y } => {
                let (x, y) = denormalize_point(x, y, self.area);
                self.fake(xproto::MOTION_NOTIFY_EVENT, 0, x as i16, y as i16)?;
            }
            InputEvent::MouseDelta { dx, dy } => {
                self.conn
                    .warp_pointer(x11rb::NONE, x11rb::NONE, 0, 0, 0, 0, dx as i16, dy as i16)
                    .map_err(|e| e.to_string())?;
            }
            InputEvent::MouseButton { button, down } => {
                let b = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 3,
                };
                self.fake(if down { xproto::BUTTON_PRESS_EVENT } else { xproto::BUTTON_RELEASE_EVENT }, b, 0, 0)?;
            }
            InputEvent::Scroll { dx, dy } => {
                // X11 scrolls in clicks: buttons 4/5 (up/down) and 6/7 (left/right).
                self.scroll.0 += dx;
                self.scroll.1 += dy;
                for (axis, plus, minus) in [(1, 4u8, 5u8), (0, 7, 6)] {
                    let acc = if axis == 1 { &mut self.scroll.1 } else { &mut self.scroll.0 };
                    while acc.abs() >= WHEEL_PX {
                        let b = if *acc > 0 { plus } else { minus };
                        *acc -= WHEEL_PX * acc.signum();
                        self.conn
                            .xtest_fake_input(xproto::BUTTON_PRESS_EVENT, b, 0, self.root, 0, 0, 0)
                            .map_err(|e| e.to_string())?;
                        self.conn
                            .xtest_fake_input(xproto::BUTTON_RELEASE_EVENT, b, 0, self.root, 0, 0, 0)
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
            InputEvent::Key { usage, down } => {
                let code = keys::linux_from_hid(usage).ok_or("unknown key")?;
                // X keycodes are evdev codes + 8.
                self.fake(
                    if down { xproto::KEY_PRESS_EVENT } else { xproto::KEY_RELEASE_EVENT },
                    (code + 8) as u8,
                    0,
                    0,
                )?;
            }
        }
        self.conn.flush().map_err(|e| e.to_string())
    }
}

// ---- wlroots -----------------------------------------------------------------------------

pub struct Wlr {
    wl: Wayland,
    pointer: ZwlrVirtualPointerV1,
    keyboard: Option<ZwpVirtualKeyboardV1>,
    mods: Mods,
    start: std::time::Instant,
}

/// Modifier state for the virtual keyboard. wlroots doesn't work it out from the keys a
/// virtual keyboard presses, so Shift+A and labwc's key bindings need it sent.
#[derive(Default)]
struct Mods {
    /// The keymap's mask for each modifier key's usage.
    masks: Vec<(u16, u32)>,
    caps_mask: u32,
    down: Vec<u16>,
    held: u32,
    locked: u32,
}

impl Mods {
    /// Updates the state for a key; true when the modifiers changed.
    fn key(&mut self, usage: u16, down: bool) -> bool {
        const CAPS_LOCK: u16 = 0x39;
        if usage == CAPS_LOCK {
            if down {
                self.locked ^= self.caps_mask;
            }
            return down;
        }
        if !self.masks.iter().any(|(u, _)| *u == usage) {
            return false;
        }
        self.down.retain(|u| *u != usage);
        if down {
            self.down.push(usage);
        }
        let before = self.held;
        self.held = self.masks.iter().filter(|(u, _)| self.down.contains(u)).fold(0, |m, (_, mask)| m | mask);
        self.held != before
    }
}

impl Wlr {
    /// `output`: the output the stream shows (absolute positions land on it); None for the
    /// whole desktop.
    pub fn new(output: Option<&str>) -> Result<Wlr, String> {
        let wl = Wayland::connect()?;
        let qh = wl.qh();
        let s = &wl.state;
        let pm = s.pointer_manager.as_ref().ok_or("this desktop has no virtual pointer")?;
        let out: Option<wl_output::WlOutput> = output.and_then(|n| wl.output(n)).map(|o| o.output.clone());
        let pointer = if out.is_some() && pm.version() >= 2 {
            pm.create_virtual_pointer_with_output(s.seat.as_ref(), out.as_ref(), &qh, ())
        } else {
            pm.create_virtual_pointer(s.seat.as_ref(), &qh, ())
        };
        let mut mods = Mods::default();
        let keyboard = match (&s.keyboard_manager, &s.seat) {
            (Some(km), Some(seat)) => {
                let kb = km.create_virtual_keyboard(seat, &qh, ());
                match keymap() {
                    Ok((file, size, m)) => {
                        kb.keymap(
                            wayland_client::protocol::wl_keyboard::KeymapFormat::XkbV1.into(),
                            file.as_fd(),
                            size,
                        );
                        mods = m;
                        Some(kb)
                    }
                    Err(e) => {
                        tracing::warn!("no keymap for the virtual keyboard: {e}");
                        None
                    }
                }
            }
            _ => None,
        };
        wl.conn.flush().map_err(|e| e.to_string())?;
        tracing::debug!(output, keyboard = keyboard.is_some(), "input: virtual pointer ready");
        Ok(Wlr { wl, pointer, keyboard, mods, start: std::time::Instant::now() })
    }

    fn time(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    fn inject(&mut self, e: InputEvent) -> Result<(), String> {
        let t = self.time();
        match e {
            InputEvent::MouseMove { x, y } => {
                self.pointer.motion_absolute(t, x.into(), y.into(), u16::MAX.into(), u16::MAX.into());
                self.pointer.frame();
            }
            InputEvent::MouseDelta { dx, dy } => {
                self.pointer.motion(t, dx.into(), dy.into());
                self.pointer.frame();
            }
            InputEvent::MouseButton { button, down } => {
                let state = if down { wl_pointer::ButtonState::Pressed } else { wl_pointer::ButtonState::Released };
                self.pointer.button(t, evdev_button(button), state);
                self.pointer.frame();
            }
            InputEvent::Scroll { dx, dy } => {
                // Wayland counts 15 per wheel click, positive scrolling down.
                self.pointer.axis_source(wl_pointer::AxisSource::Wheel);
                if dy != 0 {
                    self.pointer.axis(t, wl_pointer::Axis::VerticalScroll, -f64::from(dy) * 15.0 / f64::from(WHEEL_PX));
                }
                if dx != 0 {
                    self.pointer.axis(
                        t,
                        wl_pointer::Axis::HorizontalScroll,
                        -f64::from(dx) * 15.0 / f64::from(WHEEL_PX),
                    );
                }
                self.pointer.frame();
            }
            InputEvent::Key { usage, down } => {
                let kb = self.keyboard.as_ref().ok_or("this desktop has no virtual keyboard")?;
                let code = keys::linux_from_hid(usage).ok_or("unknown key")?;
                // Modifiers go down before the key and come up after it.
                let changed = self.mods.key(usage, down);
                if changed && down {
                    kb.modifiers(self.mods.held, 0, self.mods.locked, 0);
                }
                kb.key(t, code.into(), if down { 1 } else { 0 });
                if changed && !down {
                    kb.modifiers(self.mods.held, 0, self.mods.locked, 0);
                }
            }
        }
        self.wl.conn.flush().map_err(|e| e.to_string())
    }

    /// Presses a key chord (labwc runs its virtual-output actions from key bindings).
    pub fn chord(&mut self, usages: &[u16]) -> Result<(), String> {
        for &u in usages {
            self.inject(InputEvent::Key { usage: u, down: true })?;
        }
        for &u in usages.iter().rev() {
            self.inject(InputEvent::Key { usage: u, down: false })?;
        }
        let _ = self.wl.roundtrip();
        Ok(())
    }
}

/// The desktop's keyboard layout (from `XKB_DEFAULT_*`, else the system default), as text in a
/// file the compositor can read, and its modifier masks.
fn keymap() -> Result<(std::fs::File, u32, Mods), String> {
    use xkbcommon::xkb;
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let km = xkb::Keymap::new_from_names(&ctx, "", "", "", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
        .ok_or("couldn't compile the keyboard layout")?;
    let mask = |name: &str| match km.mod_get_index(name) {
        xkb::MOD_INVALID => 0,
        i => 1u32 << i,
    };
    let (shift, ctrl, alt, logo) =
        (mask(xkb::MOD_NAME_SHIFT), mask(xkb::MOD_NAME_CTRL), mask(xkb::MOD_NAME_ALT), mask(xkb::MOD_NAME_LOGO));
    // Left and right Ctrl, Shift, Alt, Super (USB HID usages E0–E7).
    let masks = vec![
        (0xE0, ctrl),
        (0xE1, shift),
        (0xE2, alt),
        (0xE3, logo),
        (0xE4, ctrl),
        (0xE5, shift),
        (0xE6, alt),
        (0xE7, logo),
    ];
    let mods = Mods { masks, caps_mask: mask(xkb::MOD_NAME_CAPS), ..Default::default() };
    let mut text = km.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1).into_bytes();
    text.push(0);
    let mut f = crate::wl::shm_file(text.len() as u64).map_err(|e| e.to_string())?;
    f.write_all(&text).map_err(|e| e.to_string())?;
    Ok((f, text.len() as u32, mods))
}

// ---- Portal (GNOME, KDE) -----------------------------------------------------------------

pub struct Portal {
    tx: tokio::sync::mpsc::UnboundedSender<PortalInput>,
    /// The stream's size in the portal's (logical) coordinates.
    size: (i32, i32),
}

impl Portal {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<PortalInput>, size: (i32, i32)) -> Portal {
        Portal { tx, size }
    }

    fn inject(&self, e: InputEvent) {
        let msg = match e {
            InputEvent::MouseMove { x, y } => {
                let (x, y) = denormalize_point(x, y, Rect { x: 0, y: 0, w: self.size.0, h: self.size.1 });
                PortalInput::Motion { x: x.into(), y: y.into() }
            }
            InputEvent::MouseDelta { dx, dy } => PortalInput::Delta { dx: dx.into(), dy: dy.into() },
            InputEvent::MouseButton { button, down } => PortalInput::Button { code: evdev_button(button) as i32, down },
            InputEvent::Scroll { dx, dy } => PortalInput::Scroll { dx: -f64::from(dx), dy: -f64::from(dy) },
            InputEvent::Key { usage, down } => match keys::linux_from_hid(usage) {
                Some(code) => PortalInput::Key { code: code.into(), down },
                None => return,
            },
        };
        let _ = self.tx.send(msg);
    }
}
