//! C ABI for the native apps. The matching header is `include/annex.h`; keep them in sync
//! (the `header_declares_every_export` test checks the function names).
//!
//! Conventions:
//! - Functions returning `int32_t` use `ANNEX_OK` (0) for success and negative values for
//!   errors; `annex_last_error()` describes the most recent error on the calling thread.
//! - Strings returned as `char *` are owned by the caller and freed with `annex_string_free`.
//! - Callbacks may run on any thread. They must not call the `*_free` function of the object
//!   that is calling them. After a `*_free` function returns, its callbacks are never called again.
//! - Events are UTF-8 JSON objects with a `type` field, documented in `docs/PROTOCOL.md`.

#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use annex_discovery::{Advertisement, Browser, Discovery};
use annex_session::{
    Codec, Core, DisplayInfo, Event, EventSink, FrameOut, FrameSink, HostConfig, HostHandle, StreamConfig,
    SubmitResult, TrustPolicy, ViewerConfig, ViewerHandle,
};

pub const ANNEX_OK: i32 = 0;
pub const ANNEX_ERR_INVALID_ARGUMENT: i32 = -1;
pub const ANNEX_ERR_FAILED: i32 = -2;
pub const ANNEX_ERR_PANIC: i32 = -3;

pub const ANNEX_SUBMIT_SENT: i32 = 0;
pub const ANNEX_SUBMIT_NO_SESSION: i32 = 1;
pub const ANNEX_SUBMIT_DROPPED: i32 = 2;

/// Passed as `capture_ts_local_us` when the clock offset is not known yet.
pub const ANNEX_TS_UNKNOWN: i64 = i64::MIN;

pub type AnnexEventCallback = Option<unsafe extern "C" fn(ctx: *mut c_void, event_json: *const c_char)>;
pub type AnnexFrameCallback = Option<
    unsafe extern "C" fn(
        ctx: *mut c_void,
        data: *const u8,
        len: usize,
        frame_id: u32,
        capture_ts_local_us: i64,
        keyframe: bool,
    ),
>;

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(msg: impl Into<String>) {
    let s = CString::new(msg.into().replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with(|e| *e.borrow_mut() = s);
}

/// Runs `f`, converting errors and panics into return codes.
fn guard(f: impl FnOnce() -> Result<i32, (i32, String)>) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err((code, msg))) => {
            set_error(msg);
            code
        }
        Err(_) => {
            set_error("internal panic");
            ANNEX_ERR_PANIC
        }
    }
}

fn invalid(msg: &str) -> (i32, String) {
    (ANNEX_ERR_INVALID_ARGUMENT, msg.to_string())
}

fn failed(e: impl std::fmt::Display) -> (i32, String) {
    (ANNEX_ERR_FAILED, e.to_string())
}

unsafe fn str_arg<'a>(p: *const c_char, name: &str) -> Result<&'a str, (i32, String)> {
    if p.is_null() {
        return Err(invalid(&format!("{name} is null")));
    }
    // SAFETY: caller guarantees a valid NUL-terminated string.
    unsafe { CStr::from_ptr(p) }.to_str().map_err(|_| invalid(&format!("{name} is not UTF-8")))
}

unsafe fn opt_str_arg<'a>(p: *const c_char, name: &str) -> Result<Option<&'a str>, (i32, String)> {
    if p.is_null() { Ok(None) } else { unsafe { str_arg(p, name) }.map(Some) }
}

fn out_string(s: &str) -> *mut c_char {
    CString::new(s.replace('\0', " ")).unwrap_or_default().into_raw()
}

fn parse_codec(s: &str) -> Result<Codec, (i32, String)> {
    match s {
        "h264" => Ok(Codec::H264),
        "hevc" => Ok(Codec::Hevc),
        "raw" => Ok(Codec::Raw),
        _ => Err(invalid("codec must be h264, hevc or raw")),
    }
}

/// A raw context pointer the native side promised is safe to use from any thread.
#[derive(Clone, Copy)]
struct Ctx(*mut c_void);
unsafe impl Send for Ctx {}
unsafe impl Sync for Ctx {}

/// Callback registration that can be revoked; invocations hold the lock, so once `clear`
/// returns no callback is running or will run.
struct Callbacks {
    inner: Mutex<Option<(AnnexEventCallback, AnnexFrameCallback, Ctx)>>,
}

impl Callbacks {
    fn new(ev: AnnexEventCallback, fr: AnnexFrameCallback, ctx: *mut c_void) -> Arc<Self> {
        Arc::new(Callbacks { inner: Mutex::new(Some((ev, fr, Ctx(ctx)))) })
    }

    fn clear(&self) {
        *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn event_json(&self, json: &str) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((Some(cb), _, ctx)) = *g {
            let c = CString::new(json).unwrap_or_default();
            // SAFETY: native callback contract.
            unsafe { cb(ctx.0, c.as_ptr()) };
        }
    }

    fn event_sink(self: &Arc<Self>) -> EventSink {
        let me = self.clone();
        Arc::new(move |e: Event| {
            if let Ok(json) = serde_json::to_string(&e) {
                me.event_json(&json);
            }
        })
    }

    fn frame_sink(self: &Arc<Self>) -> FrameSink {
        let me = self.clone();
        Arc::new(move |f: FrameOut<'_>| {
            let g = me.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, Some(cb), ctx)) = *g {
                // SAFETY: native callback contract; `data` is valid for the call.
                unsafe {
                    cb(
                        ctx.0,
                        f.data.as_ptr(),
                        f.data.len(),
                        f.frame_id,
                        f.capture_ts_local_us.unwrap_or(ANNEX_TS_UNKNOWN),
                        f.keyframe,
                    )
                };
            }
        })
    }
}

pub struct AnnexCore {
    rt: tokio::runtime::Runtime,
    core: Arc<Core>,
    discovery: Mutex<Option<Arc<Discovery>>>,
}

impl AnnexCore {
    fn discovery(&self) -> Result<Arc<Discovery>, (i32, String)> {
        let mut d = self.discovery.lock().unwrap();
        if let Some(d) = d.as_ref() {
            return Ok(d.clone());
        }
        let new = Arc::new(Discovery::new(self.core.fingerprint()).map_err(failed)?);
        *d = Some(new.clone());
        Ok(new)
    }
}

pub struct AnnexHost {
    handle: Arc<HostHandle>,
    callbacks: Arc<Callbacks>,
    _advert: Option<Advertisement>,
    core: *const AnnexCore,
    /// Windows: the capture/encode pipeline owned by the core (annex_host_start_screen).
    #[cfg(windows)]
    screen: Option<Arc<win_host::ScreenState>>,
}

pub struct AnnexViewer {
    handle: Arc<ViewerHandle>,
    callbacks: Arc<Callbacks>,
    core: *const AnnexCore,
    #[cfg(windows)]
    window: Option<Arc<annex_media_win::VideoWindow>>,
}

pub struct AnnexDiscovery {
    _browser: Browser,
    callbacks: Arc<Callbacks>,
}

fn new_core(core: Arc<Core>, out: *mut *mut AnnexCore) -> Result<i32, (i32, String)> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("annex-core")
        .enable_all()
        .build()
        .map_err(failed)?;
    let boxed = Box::new(AnnexCore { rt, core, discovery: Mutex::new(None) });
    // SAFETY: checked non-null by callers.
    unsafe { *out = Box::into_raw(boxed) };
    Ok(ANNEX_OK)
}

// ---------------------------------------------------------------------------------------
// Library / core
// ---------------------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn annex_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// Message for the last error on this thread. Valid until the next failing call on this thread.
#[unsafe(no_mangle)]
pub extern "C" fn annex_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_string_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: produced by CString::into_raw in this library.
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Microseconds on the core's monotonic clock; use it for `capture_ts_us`.
#[unsafe(no_mangle)]
pub extern "C" fn annex_now_us() -> i64 {
    annex_session::clock::now_us()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_core_new(
    data_dir: *const c_char,
    device_name: *const c_char,
    out: *mut *mut AnnexCore,
) -> i32 {
    guard(|| {
        if out.is_null() {
            return Err(invalid("out is null"));
        }
        let dir = PathBuf::from(unsafe { str_arg(data_dir, "data_dir") }?);
        let name = unsafe { str_arg(device_name, "device_name") }?;
        let core = Core::open(&dir, name, TrustPolicy::Pair).map_err(failed)?;
        new_core(core, out)
    })
}

/// In-memory identity and trust store (tests). Unknown peers still require PIN pairing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_core_new_ephemeral(device_name: *const c_char, out: *mut *mut AnnexCore) -> i32 {
    guard(|| {
        if out.is_null() {
            return Err(invalid("out is null"));
        }
        let name = unsafe { str_arg(device_name, "device_name") }?;
        new_core(Core::ephemeral(name, TrustPolicy::Pair).map_err(failed)?, out)
    })
}

/// Frees the core. All hosts, viewers and discovery handles created from it must be freed first.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_core_free(core: *mut AnnexCore) {
    if !core.is_null() {
        // SAFETY: produced by annex_core_new*.
        let c = unsafe { Box::from_raw(core) };
        c.rt.shutdown_timeout(std::time::Duration::from_secs(2));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_core_fingerprint(core: *const AnnexCore) -> *mut c_char {
    match unsafe { core.as_ref() } {
        Some(c) => out_string(c.core.fingerprint()),
        None => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_core_device_name(core: *const AnnexCore) -> *mut c_char {
    match unsafe { core.as_ref() } {
        Some(c) => out_string(c.core.device_name()),
        None => std::ptr::null_mut(),
    }
}

/// `AB12 CD34 ...` form of a fingerprint, for display.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_display_fingerprint(fingerprint: *const c_char) -> *mut c_char {
    match unsafe { str_arg(fingerprint, "fingerprint") } {
        Ok(fp) => out_string(&annex_crypto::display_fingerprint(fp)),
        Err(_) => std::ptr::null_mut(),
    }
}

/// JSON array of `{fingerprint, name, added_at, last_seen}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_core_trusted_peers_json(core: *const AnnexCore) -> *mut c_char {
    match unsafe { core.as_ref() } {
        Some(c) => out_string(&serde_json::to_string(&c.core.trusted_peers()).unwrap_or_else(|_| "[]".into())),
        None => std::ptr::null_mut(),
    }
}

/// Revokes a trusted peer. Returns 1 if removed, 0 if it wasn't trusted, negative on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_core_untrust(core: *const AnnexCore, fingerprint: *const c_char) -> i32 {
    guard(|| {
        let c = unsafe { core.as_ref() }.ok_or_else(|| invalid("core is null"))?;
        let fp = unsafe { str_arg(fingerprint, "fingerprint") }?;
        Ok(c.core.untrust(fp).map_err(failed)? as i32)
    })
}

/// Answers a `pairing_pin_required` event with the PIN the user typed.
/// Returns 1 if the request was pending, 0 if unknown/expired.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_core_submit_pin(core: *const AnnexCore, request_id: u64, pin: *const c_char) -> i32 {
    guard(|| {
        let c = unsafe { core.as_ref() }.ok_or_else(|| invalid("core is null"))?;
        let pin = unsafe { str_arg(pin, "pin") }?;
        Ok(c.core.submit_pin(request_id, pin) as i32)
    })
}

/// Cancels a pending pairing (`pairing_pin` on the host, `pairing_pin_required` on the viewer).
/// Returns 1 if the request was pending.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_core_cancel_request(core: *const AnnexCore, request_id: u64) -> i32 {
    guard(|| {
        let c = unsafe { core.as_ref() }.ok_or_else(|| invalid("core is null"))?;
        Ok(c.core.cancel_request(request_id) as i32)
    })
}

// ---------------------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------------------

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn annex_host_start(
    core: *const AnnexCore,
    port: u16,
    codec: *const c_char,
    width: u32,
    height: u32,
    fps: u32,
    advertise: bool,
    on_event: AnnexEventCallback,
    ctx: *mut c_void,
    out: *mut *mut AnnexHost,
) -> i32 {
    guard(|| {
        let c = unsafe { core.as_ref() }.ok_or_else(|| invalid("core is null"))?;
        if out.is_null() {
            return Err(invalid("out is null"));
        }
        let codec = parse_codec(unsafe { str_arg(codec, "codec") }?)?;
        if width == 0 || height == 0 || fps == 0 {
            return Err(invalid("width, height and fps must be non-zero"));
        }
        let callbacks = Callbacks::new(on_event, None, ctx);
        let cfg = HostConfig { port, stream: StreamConfig { codec, width, height, fps } };
        let handle =
            c.rt.block_on(annex_session::start_host(c.core.clone(), cfg, callbacks.event_sink())).map_err(failed)?;
        let advert = if advertise {
            Some(c.discovery()?.advertise_host(c.core.device_name(), handle.port()).map_err(failed)?)
        } else {
            None
        };
        let host = Box::new(AnnexHost {
            handle: Arc::new(handle),
            callbacks,
            _advert: advert,
            core,
            #[cfg(windows)]
            screen: None,
        });
        unsafe { *out = Box::into_raw(host) };
        Ok(ANNEX_OK)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_host_port(host: *const AnnexHost) -> u16 {
    unsafe { host.as_ref() }.map(|h| h.handle.port()).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_host_set_stream(
    host: *const AnnexHost,
    codec: *const c_char,
    width: u32,
    height: u32,
    fps: u32,
) -> i32 {
    guard(|| {
        let h = unsafe { host.as_ref() }.ok_or_else(|| invalid("host is null"))?;
        let codec = parse_codec(unsafe { str_arg(codec, "codec") }?)?;
        if width == 0 || height == 0 || fps == 0 {
            return Err(invalid("width, height and fps must be non-zero"));
        }
        h.handle.set_stream(StreamConfig { codec, width, height, fps });
        Ok(ANNEX_OK)
    })
}

/// Queues one encoded access unit (Annex B for H.264/HEVC). Never blocks.
/// Returns `ANNEX_SUBMIT_*` or a negative error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_host_submit_frame(
    host: *const AnnexHost,
    data: *const u8,
    len: usize,
    capture_ts_us: i64,
    keyframe: bool,
) -> i32 {
    guard(|| {
        let h = unsafe { host.as_ref() }.ok_or_else(|| invalid("host is null"))?;
        if data.is_null() || len == 0 {
            return Err(invalid("empty frame"));
        }
        // SAFETY: caller guarantees `data` points to `len` readable bytes.
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        Ok(match h.handle.submit_frame(bytes, capture_ts_us, keyframe) {
            SubmitResult::Sent => ANNEX_SUBMIT_SENT,
            SubmitResult::NoSession => ANNEX_SUBMIT_NO_SESSION,
            SubmitResult::Dropped => ANNEX_SUBMIT_DROPPED,
        })
    })
}

/// Stops hosting (ends any session, stops advertising) and frees the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_host_free(host: *mut AnnexHost) {
    if host.is_null() {
        return;
    }
    let h = unsafe { Box::from_raw(host) };
    #[cfg(windows)]
    if let Some(s) = &h.screen {
        s.stop();
    }
    if let Some(c) = unsafe { h.core.as_ref() } {
        c.rt.block_on(h.handle.stop());
    }
    h.callbacks.clear();
}

/// Windows: JSON array of monitors `[{index, name, width, height, refresh_hz, primary}]`,
/// primary first.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub extern "C" fn annex_win_monitors_json() -> *mut c_char {
    let list: Vec<serde_json::Value> = annex_media_win::monitors()
        .iter()
        .enumerate()
        .map(|(i, m)| {
            serde_json::json!({
                "index": i, "name": m.name, "width": m.width, "height": m.height,
                "refresh_hz": m.refresh_hz, "primary": m.primary,
            })
        })
        .collect();
    out_string(&serde_json::Value::Array(list).to_string())
}

/// Windows: host a monitor. The core captures (Windows.Graphics.Capture), encodes (hardware
/// H.264 when available) and streams while a viewer is connected. Events are the same as
/// annex_host_start's.
#[cfg(windows)]
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn annex_host_start_screen(
    core: *const AnnexCore,
    monitor_index: u32,
    port: u16,
    advertise: bool,
    prefer_hardware: bool,
    on_event: AnnexEventCallback,
    ctx: *mut c_void,
    out: *mut *mut AnnexHost,
) -> i32 {
    guard(|| {
        let c = unsafe { core.as_ref() }.ok_or_else(|| invalid("core is null"))?;
        if out.is_null() {
            return Err(invalid("out is null"));
        }
        let monitor = annex_media_win::monitors()
            .into_iter()
            .nth(monitor_index as usize)
            .ok_or_else(|| invalid("no such monitor"))?;
        let callbacks = Callbacks::new(on_event, None, ctx);
        let (host, screen) =
            win_host::start(c, monitor, port, prefer_hardware, callbacks.event_sink()).map_err(failed)?;
        let advert = if advertise {
            Some(c.discovery()?.advertise_host(c.core.device_name(), host.port()).map_err(failed)?)
        } else {
            None
        };
        let h = Box::new(AnnexHost { handle: host, callbacks, _advert: advert, core, screen: Some(screen) });
        unsafe { *out = Box::into_raw(h) };
        Ok(ANNEX_OK)
    })
}

#[cfg(windows)]
mod win_host {
    use std::sync::{Arc, Mutex, OnceLock, Weak};

    use annex_media_win::{HostPipeline, HostPipelineConfig, Monitor, stream_size};
    use annex_session::{Codec, Event, EventSink, HostConfig, HostHandle, StreamConfig};

    use crate::AnnexCore;

    /// Pipeline lifecycle follows the session: started on `session_started`, dropped on
    /// `session_ended`, asked for an IDR on `keyframe_requested`.
    pub struct ScreenState {
        cfg: HostPipelineConfig,
        host: OnceLock<Weak<HostHandle>>,
        pipeline: Mutex<Option<HostPipeline>>,
    }

    impl ScreenState {
        fn start_pipeline(self: &Arc<Self>) {
            let Some(host) = self.host.get().and_then(|w| w.upgrade()) else { return };
            let clock: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(annex_session::clock::now_us);
            let output = Arc::new(move |f: annex_media_win::EncodedFrame| {
                host.submit_frame(&f.data, f.capture_us, f.keyframe);
            });
            match HostPipeline::start(self.cfg.clone(), clock, output) {
                Ok(p) => *self.pipeline.lock().unwrap() = Some(p),
                Err(e) => tracing::error!("screen capture failed to start: {e}"),
            }
        }

        pub fn stop(&self) {
            self.pipeline.lock().unwrap().take();
        }

        fn on_event(self: &Arc<Self>, e: &Event) {
            match e {
                Event::SessionStarted { .. } => {
                    // Capture setup touches COM/WinRT; keep it off the async runtime threads.
                    let me = self.clone();
                    std::thread::spawn(move || me.start_pipeline());
                }
                Event::SessionEnded { .. } => self.stop(),
                Event::KeyframeRequested => {
                    if let Some(p) = self.pipeline.lock().unwrap().as_ref() {
                        p.request_keyframe();
                    }
                }
                _ => {}
            }
        }
    }

    pub fn start(
        c: &AnnexCore,
        monitor: Monitor,
        port: u16,
        prefer_hardware: bool,
        app_events: EventSink,
    ) -> Result<(Arc<HostHandle>, Arc<ScreenState>), annex_session::SessionError> {
        // Hardware encoders handle 1440p60 comfortably; the software fallback is sized for 1080p.
        let (max_w, max_h) = if prefer_hardware { (2560, 1600) } else { (1920, 1200) };
        let (width, height) = stream_size(monitor.width, monitor.height, max_w, max_h);
        let fps = monitor.refresh_hz.clamp(30, 60);
        let bitrate = (width * height * fps / 8).clamp(4_000_000, 40_000_000);
        let state = Arc::new(ScreenState {
            cfg: HostPipelineConfig { monitor, width, height, fps, bitrate, prefer_hardware },
            host: OnceLock::new(),
            pipeline: Mutex::new(None),
        });
        let events: EventSink = {
            let state = state.clone();
            Arc::new(move |e: Event| {
                state.on_event(&e);
                app_events(e);
            })
        };
        let cfg = HostConfig { port, stream: StreamConfig { codec: Codec::H264, width, height, fps } };
        let handle = Arc::new(c.rt.block_on(annex_session::start_host(c.core.clone(), cfg, events))?);
        let _ = state.host.set(Arc::downgrade(&handle));
        Ok((handle, state))
    }
}

// ---------------------------------------------------------------------------------------
// Viewer
// ---------------------------------------------------------------------------------------

unsafe fn viewer_config(
    address: *const c_char,
    expected_fingerprint: *const c_char,
    display_json: *const c_char,
) -> Result<ViewerConfig, (i32, String)> {
    let address = unsafe { str_arg(address, "address") }?.to_string();
    let expected = unsafe { opt_str_arg(expected_fingerprint, "expected_fingerprint") }?
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase());
    let display = match unsafe { opt_str_arg(display_json, "display_json") }? {
        Some(j) => serde_json::from_str::<DisplayInfo>(j).map_err(|e| invalid(&format!("display_json: {e}")))?,
        None => DisplayInfo {
            width_px: 1920,
            height_px: 1080,
            width_mm: 0,
            height_mm: 0,
            refresh_hz: 60,
            scale_percent: 100,
        },
    };
    Ok(ViewerConfig {
        address,
        display,
        codecs: vec![Codec::H264],
        expected_fingerprint: expected,
        latency_csv: std::env::var_os("ANNEX_LATENCY_CSV").map(PathBuf::from),
    })
}

/// Connects to a host in the background and delivers frames to `on_frame`.
/// `expected_fingerprint` (nullable) pins the host certificate; `display_json` (nullable) is a
/// DisplayInfo object describing the screen the stream will be shown on.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn annex_viewer_connect(
    core: *const AnnexCore,
    address: *const c_char,
    expected_fingerprint: *const c_char,
    display_json: *const c_char,
    on_event: AnnexEventCallback,
    on_frame: AnnexFrameCallback,
    ctx: *mut c_void,
    out: *mut *mut AnnexViewer,
) -> i32 {
    guard(|| {
        let c = unsafe { core.as_ref() }.ok_or_else(|| invalid("core is null"))?;
        if out.is_null() {
            return Err(invalid("out is null"));
        }
        let cfg = unsafe { viewer_config(address, expected_fingerprint, display_json) }?;
        let callbacks = Callbacks::new(on_event, on_frame, ctx);
        let _enter = c.rt.enter();
        let handle = Arc::new(annex_session::connect_viewer(
            c.core.clone(),
            cfg,
            callbacks.event_sink(),
            callbacks.frame_sink(),
        ));
        let v = Box::new(AnnexViewer {
            handle,
            callbacks,
            core,
            #[cfg(windows)]
            window: None,
        });
        unsafe { *out = Box::into_raw(v) };
        Ok(ANNEX_OK)
    })
}

/// Windows only: connects and shows the stream in a full-screen window rendered by the core
/// (Media Foundation decode + Direct3D 11). Closing the window (Esc) ends the session.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_viewer_connect_windowed(
    core: *const AnnexCore,
    address: *const c_char,
    expected_fingerprint: *const c_char,
    on_event: AnnexEventCallback,
    ctx: *mut c_void,
    out: *mut *mut AnnexViewer,
) -> i32 {
    guard(|| {
        let c = unsafe { core.as_ref() }.ok_or_else(|| invalid("core is null"))?;
        if out.is_null() {
            return Err(invalid("out is null"));
        }
        let mut cfg = unsafe { viewer_config(address, expected_fingerprint, std::ptr::null()) }?;
        cfg.display = annex_media_win::primary_display_info();
        let callbacks = Callbacks::new(on_event, None, ctx);
        let events = callbacks.event_sink();

        // The window needs the viewer handle (keyframe requests, presented reports) and the
        // viewer needs the window (frames); tie them together through a late-bound slot.
        let slot: Arc<Mutex<Option<std::sync::Weak<ViewerHandle>>>> = Arc::new(Mutex::new(None));
        let hooks = annex_media_win::WindowHooks {
            on_presented: {
                let slot = slot.clone();
                Box::new(move |id| with_viewer(&slot, |v| v.frame_presented(id)))
            },
            on_need_keyframe: {
                let slot = slot.clone();
                Box::new(move || with_viewer(&slot, |v| v.request_keyframe()))
            },
            on_closed: {
                let slot = slot.clone();
                let rt = c.rt.handle().clone();
                Box::new(move || {
                    if let Some(v) = slot.lock().unwrap().as_ref().and_then(|w| w.upgrade()) {
                        rt.spawn(async move { v.disconnect().await });
                    }
                })
            },
        };
        let window = Arc::new(annex_media_win::VideoWindow::open("Annex", hooks).map_err(failed)?);
        let frames: FrameSink = {
            let window = window.clone();
            Arc::new(move |f: FrameOut<'_>| window.submit(f.data, f.frame_id, f.keyframe))
        };
        let events: EventSink = {
            let window = window.clone();
            Arc::new(move |e: Event| {
                if matches!(e, Event::SessionEnded { .. }) {
                    window.close();
                }
                events(e)
            })
        };
        let _enter = c.rt.enter();
        let handle = Arc::new(annex_session::connect_viewer(c.core.clone(), cfg, events, frames));
        *slot.lock().unwrap() = Some(Arc::downgrade(&handle));
        let v = Box::new(AnnexViewer { handle, callbacks, core, window: Some(window) });
        unsafe { *out = Box::into_raw(v) };
        Ok(ANNEX_OK)
    })
}

#[cfg(windows)]
fn with_viewer(slot: &Mutex<Option<std::sync::Weak<ViewerHandle>>>, f: impl FnOnce(&ViewerHandle)) {
    if let Some(v) = slot.lock().unwrap().as_ref().and_then(|w| w.upgrade()) {
        f(&v);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_viewer_request_keyframe(viewer: *const AnnexViewer) {
    if let Some(v) = unsafe { viewer.as_ref() } {
        v.handle.request_keyframe();
    }
}

/// Reports that a frame reached the screen; drives `latency_stats` events.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_viewer_frame_presented(viewer: *const AnnexViewer, frame_id: u32) {
    if let Some(v) = unsafe { viewer.as_ref() } {
        v.handle.frame_presented(frame_id);
    }
}

/// Disconnects (if connected) and frees the viewer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_viewer_free(viewer: *mut AnnexViewer) {
    if viewer.is_null() {
        return;
    }
    let v = unsafe { Box::from_raw(viewer) };
    if let Some(c) = unsafe { v.core.as_ref() } {
        c.rt.block_on(v.handle.disconnect());
    }
    #[cfg(windows)]
    if let Some(w) = &v.window {
        w.close();
    }
    v.callbacks.clear();
}

// ---------------------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------------------

/// Browses the LAN for hosts. Events: `peer_found` / `peer_lost`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_discovery_start(
    core: *const AnnexCore,
    on_event: AnnexEventCallback,
    ctx: *mut c_void,
    out: *mut *mut AnnexDiscovery,
) -> i32 {
    guard(|| {
        let c = unsafe { core.as_ref() }.ok_or_else(|| invalid("core is null"))?;
        if out.is_null() {
            return Err(invalid("out is null"));
        }
        let callbacks = Callbacks::new(on_event, None, ctx);
        let cb = callbacks.clone();
        let browser = c
            .discovery()?
            .browse(move |e| {
                if let Ok(json) = serde_json::to_string(&e) {
                    cb.event_json(&json);
                }
            })
            .map_err(failed)?;
        unsafe { *out = Box::into_raw(Box::new(AnnexDiscovery { _browser: browser, callbacks })) };
        Ok(ANNEX_OK)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn annex_discovery_free(d: *mut AnnexDiscovery) {
    if !d.is_null() {
        let d = unsafe { Box::from_raw(d) };
        d.callbacks.clear();
        drop(d);
    }
}
