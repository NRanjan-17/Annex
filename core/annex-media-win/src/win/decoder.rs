//! H.264 decoding with Microsoft's Media Foundation decoder MFT (software path in M1;
//! the D3D11-accelerated path arrives in M2).

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};

use super::MediaError;

/// One decoded NV12 picture in system memory.
pub struct DecodedFrame {
    pub data: Vec<u8>,
    /// Allocated size (height may be padded, e.g. 1088).
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// Visible area (display aperture).
    pub visible_width: u32,
    pub visible_height: u32,
}

pub struct H264Decoder {
    mft: IMFTransform,
    width: u32,
    height: u32,
    stride: u32,
    visible: (u32, u32),
}

fn unpack_size(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

impl H264Decoder {
    pub fn new() -> Result<Self, MediaError> {
        unsafe {
            // S_FALSE (already initialised) is fine; RPC_E_CHANGED_MODE means the thread uses STA,
            // which the MFT also tolerates.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)?;
            let mft: IMFTransform = CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;
            if let Ok(attrs) = mft.GetAttributes() {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            mft.SetInputType(0, &input, 0)?;
            let mut dec = H264Decoder { mft, width: 0, height: 0, stride: 0, visible: (0, 0) };
            dec.select_output_type()?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(dec)
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
            let buf = s.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buf.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buf.Unlock()?;
            let stride = if self.stride == 0 { self.width } else { self.stride };
            Ok(DecodedFrame {
                data,
                width: self.width,
                height: self.height,
                stride,
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
            let _ = MFShutdown();
        }
    }
}
