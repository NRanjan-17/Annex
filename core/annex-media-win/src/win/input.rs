//! Host-side input injection with SendInput. Windows refuses injected input into elevated
//! (administrator) windows and the secure desktop (UAC, lock screen) unless the injecting
//! process is elevated too; those events are silently dropped.

use annex_proto::{InputEvent, MouseButton, keys};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::MediaError;
use super::display::Monitor;
use crate::geometry::{Rect, denormalize_point};

/// One wheel notch (120) scrolls about 40 pixels in most apps.
const PIXELS_PER_NOTCH: i32 = 40;

/// Scroll remainders smaller than a wheel unit, carried to the next event.
#[derive(Default)]
pub struct Injector {
    wheel_rest: (i32, i32),
}

fn monitor_rect(m: &Monitor) -> Option<Rect> {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: plain Win32 query on a monitor handle.
    if !unsafe { GetMonitorInfoW(m.handle, &mut mi) }.as_bool() {
        return None;
    }
    let r = mi.rcMonitor;
    Some(Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top })
}

fn send(inputs: &[INPUT]) -> Result<(), MediaError> {
    // SAFETY: the slice holds fully initialized INPUT structures.
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        // Typically UIPI: the foreground window belongs to an elevated process.
        Err(MediaError::Other("SendInput was blocked".into()))
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn key(scancode: u16, down: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if scancode & 0xFF00 == 0xE000 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    // Pause has no single-scancode form; send its virtual key instead.
    let (vk, scan, flags) = if scancode == 0x45 {
        (VK_PAUSE, 0, if down { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP })
    } else {
        (VIRTUAL_KEY(0), scancode & 0xFF, flags)
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

impl Injector {
    /// Injects one event; pointer positions land on `monitor`.
    pub fn inject(&mut self, monitor: &Monitor, e: InputEvent) -> Result<(), MediaError> {
        match e {
            InputEvent::MouseMove { x, y } => {
                let r = monitor_rect(monitor).ok_or_else(|| MediaError::Other("monitor is gone".into()))?;
                let (px, py) = denormalize_point(x, y, r);
                // Absolute coordinates over the whole virtual desktop, scaled to 0..=65535.
                // SAFETY: GetSystemMetrics has no preconditions.
                let (vx, vy, vw, vh) = unsafe {
                    (
                        GetSystemMetrics(SM_XVIRTUALSCREEN),
                        GetSystemMetrics(SM_YVIRTUALSCREEN),
                        GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
                    )
                };
                let ax = ((px - vx) as i64 * 65535 / (vw - 1) as i64) as i32;
                let ay = ((py - vy) as i64 * 65535 / (vh - 1) as i64) as i32;
                send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, ax, ay, 0)])
            }
            InputEvent::MouseButton { button, down } => {
                let flags = match (button, down) {
                    (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                    (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
                    (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                    (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                    (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                };
                send(&[mouse(flags, 0, 0, 0)])
            }
            InputEvent::Scroll { dx, dy } => {
                // Wheel units: 120 per notch. Positive WHEEL scrolls up (content down), as our dy;
                // positive HWHEEL scrolls right (content left), the opposite of our dx.
                let units = |px: i32, rest: &mut i32| {
                    let total = px * 120 + *rest;
                    let out = total / PIXELS_PER_NOTCH;
                    *rest = total % PIXELS_PER_NOTCH;
                    out
                };
                let v = units(dy, &mut self.wheel_rest.1);
                let h = units(-dx, &mut self.wheel_rest.0);
                let mut inputs = Vec::with_capacity(2);
                if v != 0 {
                    inputs.push(mouse(MOUSEEVENTF_WHEEL, 0, 0, v));
                }
                if h != 0 {
                    inputs.push(mouse(MOUSEEVENTF_HWHEEL, 0, 0, h));
                }
                if inputs.is_empty() { Ok(()) } else { send(&inputs) }
            }
            InputEvent::Key { usage, down } => match keys::windows_from_hid(usage) {
                Some(sc) => send(&[key(sc, down)]),
                None => Ok(()),
            },
        }
    }
}
