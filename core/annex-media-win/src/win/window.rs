//! Full-screen borderless viewer window with its own thread: message pump, decode, present.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

use super::MediaError;
use super::decoder::H264Decoder;
use super::renderer::Renderer;

pub struct WindowHooks {
    /// A frame reached the swap chain.
    pub on_presented: Box<dyn Fn(u32) + Send + Sync>,
    /// The decoder lost sync and needs an IDR frame.
    pub on_need_keyframe: Box<dyn Fn() + Send + Sync>,
    /// The user closed the window (Esc / Alt+F4).
    pub on_closed: Box<dyn Fn() + Send + Sync>,
}

enum Msg {
    Frame { data: Vec<u8>, frame_id: u32, keyframe: bool },
    Close,
}

pub struct VideoWindow {
    tx: Mutex<Sender<Msg>>,
    closed: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

thread_local! {
    static DESTROYED: Cell<bool> = const { Cell::new(false) };
    static RESIZED: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
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
            WS_POPUP | WS_VISIBLE,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let _ = SetForegroundWindow(hwnd);
        Ok(hwnd)
    }
}

fn run(rx: Receiver<Msg>, hooks: WindowHooks, hwnd: HWND, mut renderer: Renderer) {
    let mut decoder = match H264Decoder::new() {
        Ok(d) => Some(d),
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
        let thread = std::thread::Builder::new()
            .name("annex-viewer-window".into())
            .spawn(move || {
                let setup = create_window(&title).and_then(|hwnd| Renderer::new(hwnd).map(|r| (hwnd, r)));
                match setup {
                    Ok((hwnd, renderer)) => {
                        let _ = init_tx.send(Ok(()));
                        run(rx, hooks, hwnd, renderer);
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e.to_string()));
                    }
                }
            })
            .map_err(|e| MediaError::Other(e.to_string()))?;
        match init_rx.recv() {
            Ok(Ok(())) => {
                Ok(VideoWindow { tx: Mutex::new(tx), closed: AtomicBool::new(false), thread: Mutex::new(Some(thread)) })
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(MediaError::Other(e))
            }
            Err(_) => Err(MediaError::Other("viewer window thread died".into())),
        }
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
