//! GPU colour conversion + scaling: captured BGRA → NV12 at stream size (D3D11 video processor),
//! with optional read-back to system memory for the software encoder.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::Interface;

use super::MediaError;
use crate::geometry::fit_rect;

/// Ring of output textures so the async encoder can still be reading one while we write the next.
const RING: usize = 4;

pub struct Converter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    out_size: (u32, u32),
    outputs: Vec<ID3D11Texture2D>,
    next: usize,
    staging: Option<ID3D11Texture2D>,
    processor: Option<(ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor, (u32, u32))>,
}

impl Converter {
    pub fn new(device: &ID3D11Device, width: u32, height: u32) -> Result<Self, MediaError> {
        unsafe {
            let context = device.GetImmediateContext()?;
            let mut outputs = Vec::with_capacity(RING);
            for _ in 0..RING {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut t = None;
                device.CreateTexture2D(&desc, None, Some(&mut t))?;
                outputs.push(t.unwrap());
            }
            Ok(Converter {
                device: device.clone(),
                video_device: device.cast()?,
                video_context: context.cast()?,
                context,
                out_size: (width, height),
                outputs,
                next: 0,
                staging: None,
                processor: None,
            })
        }
    }

    fn ensure_processor(&mut self, in_size: (u32, u32)) -> Result<(), MediaError> {
        if matches!(&self.processor, Some((_, _, s)) if *s == in_size) {
            return Ok(());
        }
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            InputWidth: in_size.0,
            InputHeight: in_size.1,
            OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            OutputWidth: self.out_size.0,
            OutputHeight: self.out_size.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        unsafe {
            let e = self.video_device.CreateVideoProcessorEnumerator(&desc)?;
            let p = self.video_device.CreateVideoProcessor(&e, 0)?;
            // Input: full-range RGB. Output: BT.709 limited-range YCbCr (what decoders expect).
            self.video_context.VideoProcessorSetStreamColorSpace(
                &p,
                0,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 },
            );
            self.video_context.VideoProcessorSetOutputColorSpace(
                &p,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) },
            );
            self.video_context.VideoProcessorSetStreamFrameFormat(&p, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            self.processor = Some((e, p, in_size));
        }
        Ok(())
    }

    /// Converts a captured BGRA texture; returns the NV12 output texture (valid until RING more
    /// conversions have happened).
    pub fn convert(&mut self, bgra: &ID3D11Texture2D) -> Result<ID3D11Texture2D, MediaError> {
        unsafe {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            bgra.GetDesc(&mut d);
            self.ensure_processor((d.Width, d.Height))?;
            let (e, p, _) = self.processor.as_ref().unwrap();
            let out = self.outputs[self.next].clone();
            self.next = (self.next + 1) % RING;

            let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                },
            };
            let mut in_view = None;
            self.video_device.CreateVideoProcessorInputView(bgra, e, &in_desc, Some(&mut in_view))?;
            let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut out_view = None;
            self.video_device.CreateVideoProcessorOutputView(&out, e, &out_desc, Some(&mut out_view))?;

            let r = fit_rect(d.Width, d.Height, self.out_size.0, self.out_size.1);
            let src = RECT { left: 0, top: 0, right: d.Width as i32, bottom: d.Height as i32 };
            let dst = RECT { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h };
            self.video_context.VideoProcessorSetStreamSourceRect(p, 0, true, Some(&src));
            self.video_context.VideoProcessorSetStreamDestRect(p, 0, true, Some(&dst));
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(in_view),
                ..Default::default()
            };
            let streams = [stream];
            let r = self.video_context.VideoProcessorBlt(p, out_view.as_ref(), 0, &streams);
            let [mut stream] = streams;
            std::mem::ManuallyDrop::drop(&mut stream.pInputSurface);
            r?;
            Ok(out)
        }
    }

    /// Copies an NV12 texture to a tightly packed system-memory buffer (software encoder path).
    pub fn read_back(&mut self, nv12: &ID3D11Texture2D, out: &mut Vec<u8>) -> Result<(), MediaError> {
        let (w, h) = self.out_size;
        unsafe {
            if self.staging.is_none() {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: w,
                    Height: h,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };
                let mut t = None;
                self.device.CreateTexture2D(&desc, None, Some(&mut t))?;
                self.staging = t;
            }
            let staging = self.staging.as_ref().unwrap();
            self.context.CopyResource(staging, nv12);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let pitch = m.RowPitch as usize;
            let src = std::slice::from_raw_parts(m.pData.cast::<u8>(), pitch * h as usize * 3 / 2);
            out.resize((w * h * 3 / 2) as usize, 0);
            let ok = crate::geometry::copy_nv12(
                src,
                pitch,
                pitch * h as usize,
                out,
                w as usize,
                (w * h) as usize,
                w as usize,
                h as usize,
            );
            self.context.Unmap(staging, 0);
            if !ok {
                return Err(MediaError::Other("NV12 read-back size mismatch".into()));
            }
        }
        Ok(())
    }
}
