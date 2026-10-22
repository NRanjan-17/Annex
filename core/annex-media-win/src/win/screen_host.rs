//! Windows host pipeline: capture a monitor, convert to NV12 at stream size on the GPU, encode
//! H.264 (hardware if available), hand access units to the caller.

use std::sync::{Arc, Mutex};

use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;

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
}
unsafe impl Send for Inner {}

pub struct HostPipeline {
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

fn create_device() -> Result<ID3D11Device, MediaError> {
    unsafe {
        let mut device = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
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
        let device = create_device()?;
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
        }));
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
                        encode(&mut g, &nv12, ts)
                    });
                    if let Err(e) = result {
                        tracing::warn!("frame dropped: {e}");
                    }
                }),
            )?
        };
        tracing::info!(hardware, width = cfg.width, height = cfg.height, "Windows host pipeline started");
        Ok(HostPipeline { inner, base_bitrate: cfg.bitrate, clock, hardware, _capture: SendCapture(capture) })
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

/// Stream size for a monitor: its aspect ratio, at most `max_w`×`max_h`, even dimensions.
pub fn stream_size(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let r = crate::geometry::fit_rect(width, height, max_w.min(width), max_h.min(height));
    ((r.w as u32).max(2) & !1, (r.h as u32).max(2) & !1)
}
