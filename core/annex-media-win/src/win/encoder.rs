//! H.264 encoding with Media Foundation.
//!
//! Prefers a hardware encoder MFT (NVENC / Quick Sync / AMF expose themselves as async MFTs)
//! fed with D3D11 textures; falls back to Microsoft's software encoder (sync MFT, system
//! memory). Both are configured for low latency: no B-frames, keyframes on demand.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use windows::Win32::Foundation::VARIANT_TRUE;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Multithread, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4};
use windows::core::Interface;

use super::MediaError;
use crate::annexb::with_parameter_sets;

/// One encoded access unit (Annex B; keyframes carry SPS/PPS).
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub capture_us: i64,
}

pub type OutputFn = Arc<dyn Fn(EncodedFrame) + Send + Sync>;

fn var_u32(v: u32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { ulVal: v },
            }),
        },
    }
}

fn var_true() -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_BOOL,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { boolVal: VARIANT_TRUE },
            }),
        },
    }
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

#[derive(Debug, Clone, Copy)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

unsafe fn configure(mft: &IMFTransform, cfg: EncoderConfig) -> Result<(), MediaError> {
    unsafe {
        let out = MFCreateMediaType()?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        out.SetUINT32(&MF_MT_AVG_BITRATE, cfg.bitrate)?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, pack(cfg.width, cfg.height))?;
        out.SetUINT64(&MF_MT_FRAME_RATE, pack(cfg.fps, 1))?;
        out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
        mft.SetOutputType(0, &out, 0)?;

        let input = MFCreateMediaType()?;
        input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        input.SetUINT64(&MF_MT_FRAME_SIZE, pack(cfg.width, cfg.height))?;
        input.SetUINT64(&MF_MT_FRAME_RATE, pack(cfg.fps, 1))?;
        input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        mft.SetInputType(0, &input, 0)?;

        if let Ok(api) = mft.cast::<ICodecAPI>() {
            // Not every encoder supports every knob; each is best-effort.
            let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &var_true());
            let _ = api.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &var_u32(0));
            let _ = api.SetValue(&CODECAPI_AVEncMPVGOPSize, &var_u32(cfg.fps * 30));
            if api
                .SetValue(
                    &CODECAPI_AVEncCommonRateControlMode,
                    &var_u32(eAVEncCommonRateControlMode_LowDelayVBR.0 as u32),
                )
                .is_err()
            {
                let _ = api
                    .SetValue(&CODECAPI_AVEncCommonRateControlMode, &var_u32(eAVEncCommonRateControlMode_CBR.0 as u32));
            }
            let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &var_u32(cfg.bitrate));
        }
        Ok(())
    }
}

/// SPS/PPS (Annex B) the encoder reports in its output type, if any.
unsafe fn sequence_header(mft: &IMFTransform) -> Vec<u8> {
    unsafe {
        let Ok(t) = mft.GetOutputCurrentType(0) else { return Vec::new() };
        let Ok(size) = t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) else { return Vec::new() };
        let mut buf = vec![0u8; size as usize];
        if t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut buf, None).is_err() {
            return Vec::new();
        }
        buf
    }
}

unsafe fn sample_to_frame(sample: &IMFSample, seq: &[u8]) -> Result<EncodedFrame, MediaError> {
    unsafe {
        let buf = sample.ConvertToContiguousBuffer()?;
        let mut p = std::ptr::null_mut();
        let mut len = 0u32;
        buf.Lock(&mut p, None, Some(&mut len))?;
        let data = std::slice::from_raw_parts(p, len as usize).to_vec();
        buf.Unlock()?;
        let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
        let capture_us = sample.GetSampleTime().unwrap_or(0) / 10;
        let data = if keyframe { with_parameter_sets(data, seq) } else { data };
        Ok(EncodedFrame { data, keyframe, capture_us })
    }
}

/// Software encoder: Microsoft H.264 encoder MFT, synchronous, system-memory NV12 input.
pub struct SoftwareEncoder {
    mft: IMFTransform,
    cfg: EncoderConfig,
    force_key: AtomicBool,
    output: OutputFn,
}

impl SoftwareEncoder {
    pub fn new(cfg: EncoderConfig, output: OutputFn) -> Result<Self, MediaError> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)?;
            let mft: IMFTransform = CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER)?;
            configure(&mft, cfg)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(SoftwareEncoder { mft, cfg, force_key: AtomicBool::new(true), output })
        }
    }

    pub fn request_keyframe(&self) {
        self.force_key.store(true, Ordering::Release);
    }

    /// `nv12` is a tightly packed NV12 image (`width * height * 3 / 2` bytes).
    pub fn encode(&self, nv12: &[u8], capture_us: i64) -> Result<(), MediaError> {
        let expected = (self.cfg.width * self.cfg.height * 3 / 2) as usize;
        if nv12.len() < expected {
            return Err(MediaError::Other(format!("NV12 buffer too small: {} < {expected}", nv12.len())));
        }
        unsafe {
            if self.force_key.swap(false, Ordering::AcqRel)
                && let Ok(api) = self.mft.cast::<ICodecAPI>()
            {
                let _ = api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &var_u32(1));
            }
            let buffer = MFCreateMemoryBuffer(expected as u32)?;
            let mut p = std::ptr::null_mut();
            buffer.Lock(&mut p, None, None)?;
            std::ptr::copy_nonoverlapping(nv12.as_ptr(), p, expected);
            buffer.Unlock()?;
            buffer.SetCurrentLength(expected as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(capture_us * 10)?;
            sample.SetSampleDuration(10_000_000 / self.cfg.fps as i64)?;
            self.mft.ProcessInput(0, &sample, 0)?;
            self.drain()
        }
    }

    unsafe fn drain(&self) -> Result<(), MediaError> {
        let seq = unsafe { sequence_header(&self.mft) };
        loop {
            let info = unsafe { self.mft.GetOutputStreamInfo(0)? };
            let provides = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
            let sample = if provides {
                None
            } else {
                unsafe {
                    let s = MFCreateSample()?;
                    s.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1 << 20))?)?;
                    Some(s)
                }
            };
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let r = unsafe { self.mft.ProcessOutput(0, &mut out, &mut status) };
            let sample = unsafe { std::mem::ManuallyDrop::take(&mut out[0].pSample) };
            drop(unsafe { std::mem::ManuallyDrop::take(&mut out[0].pEvents) });
            match r {
                Ok(()) => {
                    if let Some(s) = sample {
                        (self.output)(unsafe { sample_to_frame(&s, &seq)? });
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => unsafe { configure(&self.mft, self.cfg)? },
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Drop for SoftwareEncoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = MFShutdown();
        }
    }
}

/// Latest frame waiting for the hardware encoder (newer frames replace older ones).
struct Slot {
    frame: Mutex<Option<(ID3D11Texture2D, i64)>>,
    ready: Condvar,
    stop: AtomicBool,
}

/// Hardware encoder: async MFT with D3D11 texture input, driven by its own event thread.
pub struct HardwareEncoder {
    mft: IMFTransform,
    slot: Arc<Slot>,
    force_key: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    _manager: IMFDXGIDeviceManager,
}

// SAFETY: the MFT and manager are free-threaded COM objects; access is synchronised by MF.
unsafe impl Send for HardwareEncoder {}
unsafe impl Sync for HardwareEncoder {}

impl HardwareEncoder {
    pub fn new(device: &ID3D11Device, cfg: EncoderConfig, output: OutputFn) -> Result<Self, MediaError> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)?;
            let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
            let output_t = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
            let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output_t),
                &mut activates,
                &mut count,
            )?;
            let list = std::slice::from_raw_parts_mut(activates, count as usize);
            let first = list.iter().flatten().next().cloned();
            for a in list.iter_mut() {
                drop(a.take());
            }
            CoTaskMemFree(Some(activates.cast()));
            let activate = first.ok_or_else(|| MediaError::Other("no hardware H.264 encoder".into()))?;
            let mft: IMFTransform = activate.ActivateObject()?;
            let attrs = mft.GetAttributes()?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);

            let mt: ID3D11Multithread = device.cast()?;
            let _ = mt.SetMultithreadProtected(true);
            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or_else(|| MediaError::Other("no DXGI device manager".into()))?;
            manager.ResetDevice(device, token)?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            configure(&mft, cfg)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

            let slot = Arc::new(Slot { frame: Mutex::new(None), ready: Condvar::new(), stop: AtomicBool::new(false) });
            let force_key = Arc::new(AtomicBool::new(true));
            let thread = {
                let (mft, slot, force_key) = (SendMft(mft.clone()), slot.clone(), force_key.clone());
                std::thread::Builder::new()
                    .name("annex-hw-encoder".into())
                    .spawn(move || {
                        let mft = mft;
                        if let Err(e) = event_loop(&mft.0, &slot, &force_key, cfg, &output)
                            && !slot.stop.load(Ordering::Acquire)
                        {
                            tracing::error!("hardware encoder stopped: {e}");
                        }
                    })
                    .map_err(|e| MediaError::Other(e.to_string()))?
            };
            Ok(HardwareEncoder { mft, slot, force_key, thread: Some(thread), _manager: manager })
        }
    }

    pub fn request_keyframe(&self) {
        self.force_key.store(true, Ordering::Release);
    }

    /// Queues an NV12 texture (stream size). If the encoder is busy, the previous queued frame
    /// is replaced; the texture must not be written again until a later frame replaces it.
    pub fn submit(&self, nv12: &ID3D11Texture2D, capture_us: i64) {
        *self.slot.frame.lock().unwrap() = Some((nv12.clone(), capture_us));
        self.slot.ready.notify_one();
    }
}

struct SendMft(IMFTransform);
// SAFETY: see HardwareEncoder.
unsafe impl Send for SendMft {}

fn event_loop(
    mft: &IMFTransform,
    slot: &Slot,
    force_key: &AtomicBool,
    cfg: EncoderConfig,
    output: &OutputFn,
) -> Result<(), MediaError> {
    unsafe {
        let events: IMFMediaEventGenerator = mft.cast()?;
        loop {
            let event = events.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0))?;
            let kind = MF_EVENT_TYPE(event.GetType()? as i32);
            if kind == METransformNeedInput {
                {
                    let (texture, ts) = {
                        let mut g = slot.frame.lock().unwrap();
                        loop {
                            if slot.stop.load(Ordering::Acquire) {
                                return Ok(());
                            }
                            if let Some(f) = g.take() {
                                break f;
                            }
                            g = slot.ready.wait(g).unwrap();
                        }
                    };
                    if force_key.swap(false, Ordering::AcqRel)
                        && let Ok(api) = mft.cast::<ICodecAPI>()
                    {
                        let _ = api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &var_u32(1));
                    }
                    let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &texture, 0, false)?;
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&buffer)?;
                    sample.SetSampleTime(ts * 10)?;
                    sample.SetSampleDuration(10_000_000 / cfg.fps as i64)?;
                    mft.ProcessInput(0, &sample, 0)?;
                }
            } else if kind == METransformHaveOutput {
                {
                    let mut out = [MFT_OUTPUT_DATA_BUFFER {
                        dwStreamID: 0,
                        pSample: std::mem::ManuallyDrop::new(None),
                        dwStatus: 0,
                        pEvents: std::mem::ManuallyDrop::new(None),
                    }];
                    let mut status = 0;
                    let r = mft.ProcessOutput(0, &mut out, &mut status);
                    let sample = std::mem::ManuallyDrop::take(&mut out[0].pSample);
                    drop(std::mem::ManuallyDrop::take(&mut out[0].pEvents));
                    match r {
                        Ok(()) => {
                            if let Some(s) = sample {
                                output(sample_to_frame(&s, &sequence_header(mft))?);
                            }
                        }
                        Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => configure(mft, cfg)?,
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
    }
}

impl Drop for HardwareEncoder {
    fn drop(&mut self) {
        self.slot.stop.store(true, Ordering::Release);
        self.slot.ready.notify_all();
        unsafe {
            // Unblocks GetEvent in the event thread.
            if let Ok(s) = self.mft.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = MFShutdown();
        }
    }
}
