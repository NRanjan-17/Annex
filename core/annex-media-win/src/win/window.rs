//! Full-screen borderless viewer window with its own thread: message pump, decode, present.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, mpsc};

use annex_proto::{InputEvent, MouseButton};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

use super::MediaError;
use super::decoder::H264Decoder;
use super::renderer::Renderer;
use crate::geometry::normalize_point;

pub struct WindowHooks {
    /// A frame reached the swap chain.
    pub on_presented: Box<dyn Fn(u32) + Send + Sync>,
    /// The decoder lost sync and needs an IDR frame.
    pub on_need_keyframe: Box<dyn Fn() + Send + Sync>,
    /// Mouse and keyboard input over the window, while input is active (see
    /// [`VideoWindow::set_input_active`]). `None`: the window never forwards input.
    pub on_input: Option<Box<dyn Fn(InputEvent) + Send + Sync>>,
    /// The user closed the window (Esc / Alt+F4; Ctrl+Alt+Shift+Q while forwarding input).
    pub on_closed: Box<dyn Fn() + Send + Sync>,
}

/// Window-thread state for input forwarding.
struct InputState {
    sink: Box<dyn Fn(InputEvent) + Send + Sync>,
    active: Arc<AtomicBool>,
    /// Visible size of the last presented frame (for the letterbox mapping).
    video: (u32, u32),
    buttons_down: u32,
}

enum Msg {
    Frame {
        data: Vec<u8>,
        frame_id: u32,
        keyframe: bool,
    },
    /// The session started: show the (until now hidden) window in front.
    Show,
    Close,
}

/// The local pointer hides after this long without moving (the stream shows the remote one).
const CURSOR_IDLE: Duration = Duration::from_secs(2);

pub struct VideoWindow {
    tx: Mutex<Sender<Msg>>,
    input_active: Arc<AtomicBool>,
    closed: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

thread_local! {
    static DESTROYED: Cell<bool> = const { Cell::new(false) };
    static RESIZED: Cell<bool> = const { Cell::new(false) };
    static CURSOR_HIDDEN: Cell<bool> = const { Cell::new(false) };
    static LAST_MOVE: Cell<Option<Instant>> = const { Cell::new(None) };
    static INPUT: RefCell<Option<InputState>> = const { RefCell::new(None) };
}

fn key_held(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    // SAFETY: GetKeyState has no preconditions.
    (unsafe { GetKeyState(vk.0 as i32) }) < 0
}

/// Forwards a window message as input if forwarding is active. Returns true if consumed.
///
/// Windows re-enters the window procedure from inside some calls (SetCapture/ReleaseCapture send
/// WM_CAPTURECHANGED at once), so the input state is never borrowed across one: a re-entrant
/// message finds it busy and gets the default handling, and capture changes happen afterwards.
fn forward_input(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
    INPUT.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else { return false };
        let Some(st) = guard.as_mut() else { return false };
        if !st.active.load(Ordering::Acquire) {
            return false;
        }
        let video = st.video;
        let point = || {
            let (x, y) = ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32);
            let mut rc = RECT::default();
            // SAFETY: our own window.
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            normalize_point(x, y, video.0, video.1, rc.right as u32, rc.bottom as u32)
        };
        // Returns the mouse-capture change to make once the state is released: keep receiving moves
        // and releases while dragging outside the window.
        let button = |b: MouseButton, down: bool, st: &mut InputState| -> Option<bool> {
            let (x, y) = point();
            (st.sink)(InputEvent::MouseMove { x, y });
            (st.sink)(InputEvent::MouseButton { button: b, down });
            if down {
                st.buttons_down += 1;
                Some(true)
            } else {
                st.buttons_down = st.buttons_down.saturating_sub(1);
                (st.buttons_down == 0).then_some(false)
            }
        };
        let mut capture = None;
        match msg {
            WM_MOUSEMOVE => {
                let (x, y) = point();
                (st.sink)(InputEvent::MouseMove { x, y });
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => capture = button(MouseButton::Left, true, st),
            WM_LBUTTONUP => capture = button(MouseButton::Left, false, st),
            WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => capture = button(MouseButton::Right, true, st),
            WM_RBUTTONUP => capture = button(MouseButton::Right, false, st),
            WM_MBUTTONDOWN | WM_MBUTTONDBLCLK => capture = button(MouseButton::Middle, true, st),
            WM_MBUTTONUP => capture = button(MouseButton::Middle, false, st),
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                // 120 per notch ≈ 40 pixels; HWHEEL positive = scroll right (content moves left).
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let px = delta / 3;
                let e = if msg == WM_MOUSEWHEEL {
                    InputEvent::Scroll { dx: 0, dy: px }
                } else {
                    InputEvent::Scroll { dx: -px, dy: 0 }
                };
                (st.sink)(e);
            }
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
                let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                // Ctrl+Alt+Shift+Q leaves the session; every other key goes to the host.
                if down && wparam.0 == 'Q' as usize && key_held(VK_CONTROL) && key_held(VK_MENU) && key_held(VK_SHIFT) {
                    drop(guard);
                    // SAFETY: our own window.
                    let _ = unsafe { DestroyWindow(hwnd) };
                    return true;
                }
                let scan = ((lparam.0 >> 16) & 0xFF) as u16;
                let extended = (lparam.0 >> 24) & 1 == 1;
                let code = if extended { 0xE000 | scan } else { scan };
                if let Some(usage) = annex_proto::keys::hid_from_windows(code) {
                    (st.sink)(InputEvent::Key { usage, down });
                }
            }
            WM_SYSCHAR | WM_CHAR => {}
            _ => return false,
        }
        drop(guard);
        match capture {
            // SAFETY: capturing / releasing our own window's mouse capture.
            Some(true) => unsafe {
                SetCapture(hwnd);
            },
            Some(false) => {
                let _ = unsafe { ReleaseCapture() };
            }
            None => {}
        }
        true
    })
}

/// Forwarding input to the host right now (then the local pointer stays hidden).
fn input_active() -> bool {
    INPUT.with(|i| i.try_borrow().is_ok_and(|st| st.as_ref().is_some_and(|st| st.active.load(Ordering::Acquire))))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // A panic can't unwind out of the window procedure (the process would abort): log it (the panic
    // hook does) and let Windows handle the message instead.
    std::panic::catch_unwind(|| handle_message(hwnd, msg, wparam, lparam))
        // SAFETY: our own window; default handling.
        .unwrap_or_else(|_| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
}

fn handle_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_MOUSEMOVE {
        LAST_MOVE.with(|t| t.set(Some(Instant::now())));
        if !input_active() {
            // Moving the mouse brings the pointer back (WM_SETCURSOR follows and shows it).
            CURSOR_HIDDEN.with(|h| h.set(false));
        }
    }
    if msg == WM_SETCURSOR
        && (lparam.0 & 0xFFFF) as u32 == HTCLIENT
        && (CURSOR_HIDDEN.with(|h| h.get()) || input_active())
    {
        unsafe { SetCursor(None) };
        return LRESULT(1);
    }
    if forward_input(hwnd, msg, wparam, lparam) {
        return LRESULT(0);
    }
    match msg {
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_SIZE => {
            RESIZED.with(|r| r.set(true));
            LRESULT(0)
        }
        WM_DESTROY => {
            DESTROYED.with(|d| d.set(true));
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn create_window(title: &str) -> Result<HWND, MediaError> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("AnnexViewerWindow");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        // Registering twice (second window) fails harmlessly.
        RegisterClassExW(&wc);
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut mi);
        let r = mi.rcMonitor;
        let title = HSTRING::from(title);
        let hwnd = CreateWindowExW(
            WS_EX_APPWINDOW,
            class,
            PCWSTR(title.as_ptr()),
            // Hidden until the session starts, so a PIN prompt in the app isn't covered.
            WS_POPUP,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        Ok(hwnd)
    }
}

fn run(rx: Receiver<Msg>, mut hooks: WindowHooks, hwnd: HWND, mut renderer: Renderer, input_active: Arc<AtomicBool>) {
    if let Some(sink) = hooks.on_input.take() {
        INPUT.with(|i| {
            *i.borrow_mut() = Some(InputState { sink, active: input_active, video: (0, 0), buttons_down: 0 })
        });
    }
    let mut decoder = match H264Decoder::new(Some(renderer.device())) {
        Ok(d) => {
            tracing::info!(hardware = d.is_hardware(), "H.264 decoder ready");
            Some(d)
        }
        Err(e) => {
            tracing::error!("H.264 decoder unavailable: {e}");
            None
        }
    };
    let mut waiting_for_key = true;
    let mut user_closed = false;
    'outer: loop {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if DESTROYED.with(|d| d.get()) {
            user_closed = true;
            break;
        }
        if RESIZED.with(|r| r.replace(false)) {
            renderer.resized();
        }
        hide_idle_cursor(hwnd);
        let first = match rx.recv_timeout(Duration::from_millis(4)) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        // Decode everything that's queued (references matter) but present only the newest.
        let mut latest = None;
        for m in std::iter::once(first).chain(rx.try_iter()) {
            match m {
                Msg::Close => break 'outer,
                Msg::Show => unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOW);
                    let _ = SetForegroundWindow(hwnd);
                },
                Msg::Frame { data, frame_id, keyframe } => {
                    if waiting_for_key && !keyframe {
                        continue;
                    }
                    waiting_for_key = false;
                    let Some(dec) = decoder.as_mut() else { continue };
                    match dec.decode(&data) {
                        Ok(frames) => {
                            if let Some(f) = frames.into_iter().last() {
                                latest = Some((f, frame_id));
                            }
                        }
                        Err(e) => {
                            tracing::warn!("decode failed: {e}");
                            waiting_for_key = true;
                            (hooks.on_need_keyframe)();
                        }
                    }
                }
            }
        }
        if let Some((f, id)) = latest {
            INPUT.with(|i| {
                if let Some(st) = i.borrow_mut().as_mut() {
                    st.video = (f.visible_width.min(f.width), f.visible_height.min(f.height));
                }
            });
            match renderer.present(&f) {
                Ok(()) => (hooks.on_presented)(id),
                Err(e) => tracing::warn!("present failed: {e}"),
            }
        }
    }
    unsafe {
        if !DESTROYED.with(|d| d.get()) {
            let _ = DestroyWindow(hwnd);
        }
    }
    INPUT.with(|i| i.borrow_mut().take());
    drop(renderer);
    drop(decoder);
    if user_closed {
        (hooks.on_closed)();
    }
}

impl VideoWindow {
    /// Creates the window and its render thread. Fails if Direct3D 11 is unavailable.
    pub fn open(title: &str, hooks: WindowHooks) -> Result<Self, MediaError> {
        let (tx, rx) = channel::<Msg>();
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), String>>(1);
        let title = title.to_string();
        let input_active = Arc::new(AtomicBool::new(false));
        let active = input_active.clone();
        let thread = std::thread::Builder::new()
            .name("annex-viewer-window".into())
            .spawn(move || {
                let setup = create_window(&title).and_then(|hwnd| Renderer::new(hwnd).map(|r| (hwnd, r)));
                match setup {
                    Ok((hwnd, renderer)) => {
                        let _ = init_tx.send(Ok(()));
                        run(rx, hooks, hwnd, renderer, active);
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e.to_string()));
                    }
                }
            })
            .map_err(|e| MediaError::Other(e.to_string()))?;
        match init_rx.recv() {
            Ok(Ok(())) => Ok(VideoWindow {
                tx: Mutex::new(tx),
                input_active,
                closed: AtomicBool::new(false),
                thread: Mutex::new(Some(thread)),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(MediaError::Other(e))
            }
            Err(_) => Err(MediaError::Other("viewer window thread died".into())),
        }
    }

    /// Starts or stops forwarding mouse and keyboard (the host allowed or revoked input).
    pub fn set_input_active(&self, active: bool) {
        self.input_active.store(active, Ordering::Release);
    }

    /// Shows the window in front (call when the session has started).
    pub fn show(&self) {
        let _ = self.tx.lock().unwrap().send(Msg::Show);
    }

    pub fn submit(&self, data: &[u8], frame_id: u32, keyframe: bool) {
        let _ = self.tx.lock().unwrap().send(Msg::Frame { data: data.to_vec(), frame_id, keyframe });
    }

    /// Closes the window and waits for its thread (unless called from that thread).
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.tx.lock().unwrap().send(Msg::Close);
        let handle = self.thread.lock().unwrap().take();
        if let Some(h) = handle
            && h.thread().id() != std::thread::current().id()
        {
            let _ = h.join();
        }
    }
}

impl Drop for VideoWindow {
    fn drop(&mut self) {
        self.close();
    }
}

/// Hides the local pointer over the video after it rests (or at once while controlling the host).
fn hide_idle_cursor(hwnd: HWND) {
    if CURSOR_HIDDEN.with(|h| h.get()) {
        return;
    }
    let idle = LAST_MOVE.with(|t| t.get()).is_none_or(|at| at.elapsed() >= CURSOR_IDLE);
    if !(idle || input_active()) {
        return;
    }
    let mut pt = POINT::default();
    // SAFETY: plain Win32 queries on our own window.
    unsafe {
        if GetCursorPos(&mut pt).is_ok() && WindowFromPoint(pt) == hwnd {
            SetCursor(None);
            CURSOR_HIDDEN.with(|h| h.set(true));
        }
    }
}
