//! H.264 decoding with Microsoft's Media Foundation decoder MFT, and HEVC when Windows has an
//! HEVC decoder (the HEVC Video Extensions, built in on many PCs, a Store download on others).
//!
//! With a D3D11 device the decoder runs on the GPU (DXVA) and returns textures that the
//! renderer feeds straight into the video processor (no CPU copies). Without one, or if the
//! GPU path can't be set up, it decodes in software into system memory.

use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Multithread, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::core::Interface;

use super::MediaError;

/// Where a decoded NV12 picture lives.
pub enum Picture {
    /// System memory (software decode).
    Cpu { data: Vec<u8>, stride: u32 },
    /// A slice of a decoder texture array (hardware decode). `_sample` keeps the decoder from
    /// reusing the surface until we're done with it.
    Gpu { texture: ID3D11Texture2D, subresource: u32, _sample: IMFSample },
}

/// One decoded NV12 picture.
pub struct DecodedFrame {
    pub picture: Picture,
    /// Allocated size (height may be padded, e.g. 1088).
    pub width: u32,
    pub height: u32,
    /// Visible area (display aperture).
    pub visible_width: u32,
    pub visible_height: u32,
}

pub struct H264Decoder {
    mft: IMFTransform,
    /// Present when decoding on the GPU; must outlive the MFT's use of it.
    _manager: Option<IMFDXGIDeviceManager>,
    width: u32,
    height: u32,
    stride: u32,
    visible: (u32, u32),
}

/// The first HEVC decoder Windows lists, activated; None if there's none.
unsafe fn hevc_decoder() -> Result<Option<IMFTransform>, MediaError> {
    unsafe {
        let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_HEVC };
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        let flags = MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER;
        MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, flags, Some(&input), None, &mut activates, &mut count)?;
        let list: Vec<IMFActivate> = if activates.is_null() {
            Vec::new()
        } else {
            let slice = std::slice::from_raw_parts_mut(activates, count as usize);
            let out = slice.iter_mut().filter_map(|a| a.take()).collect();
            CoTaskMemFree(Some(activates.cast()));
            out
        };
        let Some(first) = list.first() else { return Ok(None) };
        Ok(Some(first.ActivateObject().map_err(|e| MediaError::Other(format!("opening the HEVC decoder: {e}")))?))
    }
}

/// Whether this PC can decode HEVC, so a viewer here can offer it: a decoder is installed (the
/// HEVC Video Extensions) and really starts. Some PCs list one that then refuses to start (seen:
/// "The requested attribute was not found", 0xC00D36E6); offering HEVC there left the viewer
/// with no picture, so it's only offered once one has started here.
pub fn hevc_decoder_available() -> bool {
    match H264Decoder::new_for(None, true) {
        Ok(_) => true,
        Err(e) => {
            tracing::info!("HEVC not offered: this PC's HEVC decoder doesn't start ({e}); the picture comes as H.264");
            false
        }
    }
}

/// Names the step that failed, so a decoder that won't start can be told apart in the log.
fn step(what: &'static str) -> impl FnOnce(windows::core::Error) -> MediaError {
    move |e| MediaError::Other(format!("{what}: {e}"))
}

fn unpack_size(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

impl H264Decoder {
    pub fn is_hardware(&self) -> bool {
        self._manager.is_some()
    }

    /// `device` enables GPU decoding (falls back to software if the decoder refuses it).
    pub fn new(device: Option<&ID3D11Device>) -> Result<Self, MediaError> {
        Self::new_for(device, false)
    }

    /// An H.264 or (`hevc`) HEVC decoder.
    pub fn new_for(device: Option<&ID3D11Device>, hevc: bool) -> Result<Self, MediaError> {
        unsafe {
            // S_FALSE (already initialised) is fine; RPC_E_CHANGED_MODE means the thread uses STA,
            // which the MFT also tolerates.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            super::mf_startup()?;
            let mft: IMFTransform = if hevc {
                hevc_decoder()?.ok_or_else(|| MediaError::Other("no HEVC decoder (HEVC Video Extensions)".into()))?
            } else {
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?
            };
            if let Ok(attrs) = mft.GetAttributes() {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, if hevc { &MFVideoFormat_HEVC } else { &MFVideoFormat_H264 })?;
            let manager = device.and_then(|d| match Self::enable_d3d11(&mft, d) {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!("hardware decode unavailable, using software: {e}");
                    None
                }
            });
            mft.SetInputType(0, &input, 0).map_err(step("setting the input type"))?;
            let mut dec = H264Decoder { mft, _manager: manager, width: 0, height: 0, stride: 0, visible: (0, 0) };
            dec.select_output_type()?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(step("starting"))?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(step("starting the stream"))?;
            Ok(dec)
        }
    }

    unsafe fn enable_d3d11(mft: &IMFTransform, device: &ID3D11Device) -> Result<IMFDXGIDeviceManager, MediaError> {
        unsafe {
            let attrs = mft.GetAttributes()?;
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                return Err(MediaError::Other("decoder is not D3D11-aware".into()));
            }
            // The decoder and the renderer share the device across threads.
            let mt: ID3D11Multithread = device.cast()?;
            let _ = mt.SetMultithreadProtected(true);
            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or_else(|| MediaError::Other("no DXGI device manager".into()))?;
            manager.ResetDevice(device, token)?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
            Ok(manager)
        }
    }

    /// Picks NV12 output and records the current geometry.
    fn select_output_type(&mut self) -> Result<(), MediaError> {
        unsafe {
            let mut i = 0;
            loop {
                let t = match self.mft.GetOutputAvailableType(0, i) {
                    Ok(t) => t,
                    Err(_) => return Err(MediaError::Other("decoder offers no NV12 output".into())),
                };
                if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                    self.mft.SetOutputType(0, &t, 0)?;
                    if let Ok(size) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                        let (w, h) = unpack_size(size);
                        self.width = w;
                        self.height = h;
                        self.visible = (w, h);
                    }
                    self.stride = t
                        .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                        .map(|s| s as i32)
                        .unwrap_or(self.width as i32)
                        .unsigned_abs();
                    let mut area = MFVideoArea::default();
                    let mut got = 0u32;
                    if t.GetBlob(
                        &MF_MT_MINIMUM_DISPLAY_APERTURE,
                        std::slice::from_raw_parts_mut(
                            (&mut area as *mut MFVideoArea).cast::<u8>(),
                            std::mem::size_of::<MFVideoArea>(),
                        ),
                        Some(&mut got),
                    )
                    .is_ok()
                        && area.Area.cx > 0
                        && area.Area.cy > 0
                    {
                        self.visible = (area.Area.cx as u32, area.Area.cy as u32);
                    }
                    return Ok(());
                }
                i += 1;
            }
        }
    }

    /// Feeds one Annex B access unit and returns any pictures that came out.
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<DecodedFrame>, MediaError> {
        let mut out = Vec::new();
        unsafe {
            let sample = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            sample.AddBuffer(&buffer)?;
            match self.mft.ProcessInput(0, &sample, 0) {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    self.drain(&mut out)?;
                    self.mft.ProcessInput(0, &sample, 0)?;
                }
                Err(e) => return Err(e.into()),
            }
            self.drain(&mut out)?;
        }
        Ok(out)
    }

    unsafe fn drain(&mut self, out: &mut Vec<DecodedFrame>) -> Result<(), MediaError> {
        loop {
            let info = unsafe { self.mft.GetOutputStreamInfo(0)? };
            let provides = info.dwFlags
                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)
                != 0;
            let sample = if provides {
                None
            } else {
                let s = unsafe { MFCreateSample()? };
                let b = unsafe { MFCreateMemoryBuffer(info.cbSize.max(1))? };
                unsafe { s.AddBuffer(&b)? };
                Some(s)
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let r = unsafe { self.mft.ProcessOutput(0, &mut buffers, &mut status) };
            let sample = unsafe { std::mem::ManuallyDrop::take(&mut buffers[0].pSample) };
            drop(unsafe { std::mem::ManuallyDrop::take(&mut buffers[0].pEvents) });
            match r {
                Ok(()) => {
                    if let Some(s) = sample {
                        out.push(self.read_sample(&s)?);
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.select_output_type()?,
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn read_sample(&self, s: &IMFSample) -> Result<DecodedFrame, MediaError> {
        unsafe {
            let buf = s.GetBufferByIndex(0)?;
            let picture = if let Ok(dxgi) = buf.cast::<IMFDXGIBuffer>() {
                let mut ptr = std::ptr::null_mut();
                dxgi.GetResource(&ID3D11Texture2D::IID, &mut ptr)?;
                let texture = ID3D11Texture2D::from_raw(ptr);
                Picture::Gpu { texture, subresource: dxgi.GetSubresourceIndex()?, _sample: s.clone() }
            } else {
                let buf = s.ConvertToContiguousBuffer()?;
                let mut p = std::ptr::null_mut();
                let mut len = 0u32;
                buf.Lock(&mut p, None, Some(&mut len))?;
                let data = std::slice::from_raw_parts(p, len as usize).to_vec();
                buf.Unlock()?;
                Picture::Cpu { data, stride: if self.stride == 0 { self.width } else { self.stride } }
            };
            Ok(DecodedFrame {
                picture,
                width: self.width,
                height: self.height,
                visible_width: self.visible.0,
                visible_height: self.visible.1,
            })
        }
    }
}

impl Drop for H264Decoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}
