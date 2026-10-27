//! Windows host pipeline: capture a monitor, convert to NV12 at stream size on the GPU, encode
//! H.264 (hardware if available), hand access units to the caller.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIAdapter, IDXGIFactory4};

use super::MediaError;
use super::capture::ScreenCapture;
use super::convert::Converter;
use super::display::Monitor;
use super::encoder::{EncoderConfig, HardwareEncoder, OutputFn, SoftwareEncoder};

pub use super::encoder::EncodedFrame;

#[derive(Debug, Clone)]
pub struct HostPipelineConfig {
    pub monitor: Monitor,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub prefer_hardware: bool,
    /// Graphics card to capture and encode on (adapter LUID); None = Windows' default.
    pub gpu: Option<u64>,
    /// Encode at most this many frames a second (adaptive bitrate's smaller picture); None:
    /// every frame Windows captures.
    pub max_fps: Option<u32>,
}

/// A graphics card the user can pick for sharing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    pub name: String,
    /// Adapter LUID, packed as (HighPart << 32) | LowPart.
    pub luid: u64,
}

/// Hardware graphics cards, in Windows' order (the first is the default).
pub fn gpus() -> Vec<Gpu> {
    let mut out: Vec<Gpu> = Vec::new();
    // Windows can list one physical GPU several times under different LUIDs (virtual display
    // drivers such as Annex's or Duet's appear as extra adapters reusing the GPU they render on).
    // Keep the first entry per piece of hardware: it's the real adapter.
    let mut seen: Vec<(u32, u32, u32, u32)> = Vec::new();
    // SAFETY: plain DXGI enumeration.
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory4>() else { return out };
        for i in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(i) else { break };
            let Ok(desc) = adapter.GetDesc1() else { continue };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let len = desc.Description.iter().position(|c| *c == 0).unwrap_or(desc.Description.len());
            let luid = ((desc.AdapterLuid.HighPart as u32 as u64) << 32) | desc.AdapterLuid.LowPart as u64;
            let hardware = (desc.VendorId, desc.DeviceId, desc.SubSysId, desc.Revision);
            if !seen.contains(&hardware) && !out.iter().any(|g| g.luid == luid) {
                seen.push(hardware);
                out.push(Gpu { name: String::from_utf16_lossy(&desc.Description[..len]), luid });
            }
        }
    }
    out
}

enum Encoder {
    Hardware(HardwareEncoder),
    Software { enc: SoftwareEncoder, scratch: Vec<u8> },
}

// SAFETY: the D3D11 device is created multithread-protected and every access to the converter
// and encoder goes through `Inner`'s mutex.
struct Inner {
    converter: Converter,
    encoder: Encoder,
    last: Option<ID3D11Texture2D>,
    /// With `max_fps`: when the last frame was encoded, and whether a newer one was skipped.
    last_encoded_us: i64,
    skipped: bool,
}
unsafe impl Send for Inner {}

/// With `max_fps`, sends a skipped frame once its turn comes if no newer one arrives: Windows
/// captures only when the screen changes, so the last change could otherwise never be shown.
struct Pacer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Pacer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub struct HostPipeline {
    /// First: stopped (and joined) before the rest goes.
    _pacer: Option<Pacer>,
    inner: Arc<Mutex<Inner>>,
    base_bitrate: u32,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    hardware: bool,
    _capture: SendCapture,
}

struct SendCapture(#[allow(dead_code)] ScreenCapture);
// SAFETY: WinRT capture objects are agile.
unsafe impl Send for SendCapture {}
unsafe impl Sync for SendCapture {}

fn create_device(gpu: Option<u64>) -> Result<ID3D11Device, MediaError> {
    unsafe {
        // The chosen graphics card, if it's still there; otherwise Windows' default.
        let adapter: Option<IDXGIAdapter> = gpu.and_then(|luid| {
            let factory = CreateDXGIFactory1::<IDXGIFactory4>().ok()?;
            let luid = LUID { LowPart: luid as u32, HighPart: (luid >> 32) as i32 };
            factory.EnumAdapterByLuid::<IDXGIAdapter>(luid).ok()
        });
        let mut device = None;
        D3D11CreateDevice(
            adapter.as_ref(),
            if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE },
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
        let device: ID3D11Device = device.ok_or_else(|| MediaError::Other("no D3D11 device".into()))?;
        let mt: ID3D11Multithread = windows::core::Interface::cast(&device)?;
        let _ = mt.SetMultithreadProtected(true);
        Ok(device)
    }
}

fn encode(inner: &mut Inner, nv12: &ID3D11Texture2D, ts: i64) -> Result<(), MediaError> {
    match &mut inner.encoder {
        Encoder::Hardware(h) => {
            h.submit(nv12, ts);
            Ok(())
        }
        Encoder::Software { enc, scratch } => {
            inner.converter.read_back(nv12, scratch)?;
            enc.encode(scratch, ts)
        }
    }
}

impl HostPipeline {
    /// `clock` supplies capture timestamps (µs, the core's monotonic clock).
    pub fn start(
        cfg: HostPipelineConfig,
        clock: Arc<dyn Fn() -> i64 + Send + Sync>,
        output: OutputFn,
    ) -> Result<Self, MediaError> {
        let device = create_device(cfg.gpu)?;
        let ecfg = EncoderConfig { width: cfg.width, height: cfg.height, fps: cfg.fps, bitrate: cfg.bitrate };
        let encoder = if cfg.prefer_hardware {
            match HardwareEncoder::new(&device, ecfg, output.clone()) {
                Ok(h) => Encoder::Hardware(h),
                Err(e) => {
                    tracing::warn!("hardware H.264 encoder unavailable, using software: {e}");
                    Encoder::Software { enc: SoftwareEncoder::new(ecfg, output)?, scratch: Vec::new() }
                }
            }
        } else {
            Encoder::Software { enc: SoftwareEncoder::new(ecfg, output)?, scratch: Vec::new() }
        };
        let hardware = matches!(encoder, Encoder::Hardware(_));
        let inner = Arc::new(Mutex::new(Inner {
            converter: Converter::new(&device, cfg.width, cfg.height)?,
            encoder,
            last: None,
            last_encoded_us: 0,
            skipped: false,
        }));
        // A little under the frame time, so a screen refreshing at exactly twice the rate isn't
        // cut to a third by timing jitter. 0: every frame.
        let min_gap_us: i64 = cfg.max_fps.map_or(0, |f| 900_000 / i64::from(f.max(1)));
        let capture = {
            let inner = inner.clone();
            let clock = clock.clone();
            ScreenCapture::start(
                &device,
                cfg.monitor.handle,
                Arc::new(move |bgra| {
                    let ts = clock();
                    let mut g = inner.lock().unwrap();
                    let result = g.converter.convert(bgra).and_then(|nv12| {
                        g.last = Some(nv12.clone());
                        if min_gap_us > 0 && ts - g.last_encoded_us < min_gap_us {
                            // Too soon after the last one: the pacer sends it if nothing newer comes.
                            g.skipped = true;
                            return Ok(());
                        }
                        g.skipped = false;
                        g.last_encoded_us = ts;
                        encode(&mut g, &nv12, ts)
                    });
                    if let Err(e) = result {
                        tracing::warn!("frame dropped: {e}");
                    }
                }),
            )?
        };
        let pacer = if min_gap_us > 0 {
            let stop = Arc::new(AtomicBool::new(false));
            let (inner, clock, stop2) = (inner.clone(), clock.clone(), stop.clone());
            let thread = std::thread::Builder::new()
                .name("annex-frame-pacer".into())
                .spawn(move || {
                    while !stop2.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_micros(min_gap_us as u64));
                        let mut g = inner.lock().unwrap();
                        let ts = clock();
                        if !g.skipped || ts - g.last_encoded_us < min_gap_us {
                            continue;
                        }
                        let Some(last) = g.last.clone() else { continue };
                        g.skipped = false;
                        g.last_encoded_us = ts;
                        if let Err(e) = encode(&mut g, &last, ts) {
                            tracing::warn!("frame dropped: {e}");
                        }
                    }
                })
                .map_err(|e| MediaError::Other(e.to_string()))?;
            Some(Pacer { stop, thread: Some(thread) })
        } else {
            None
        };
        tracing::info!(
            hardware,
            width = cfg.width,
            height = cfg.height,
            max_fps = ?cfg.max_fps,
            "Windows host pipeline started"
        );
        Ok(HostPipeline {
            _pacer: pacer,
            inner,
            base_bitrate: cfg.bitrate,
            clock,
            hardware,
            _capture: SendCapture(capture),
        })
    }

    pub fn is_hardware(&self) -> bool {
        self.hardware
    }

    /// Adaptive bitrate: run the encoder at `percent` % of the configured bitrate.
    pub fn set_bitrate_scale(&self, percent: u32) {
        let bitrate = (self.base_bitrate as u64 * percent as u64 / 100).clamp(500_000, u32::MAX as u64) as u32;
        match &self.inner.lock().unwrap().encoder {
            Encoder::Hardware(h) => h.set_bitrate(bitrate),
            Encoder::Software { enc, .. } => enc.set_bitrate(bitrate),
        }
    }

    /// Forces an IDR and, because a static screen produces no new frames, re-encodes the last one.
    pub fn request_keyframe(&self) {
        let mut g = self.inner.lock().unwrap();
        match &g.encoder {
            Encoder::Hardware(h) => h.request_keyframe(),
            Encoder::Software { enc, .. } => enc.request_keyframe(),
        }
        if let Some(last) = g.last.clone() {
            let ts = (self.clock)();
            if let Err(e) = encode(&mut g, &last, ts) {
                tracing::warn!("keyframe re-encode failed: {e}");
            }
        }
    }
}

pub use crate::geometry::stream_size;
