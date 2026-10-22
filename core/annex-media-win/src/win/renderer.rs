//! Direct3D 11 presenter: uploads NV12 frames and uses the D3D11 video processor for
//! colour conversion and scaling (letterboxed) straight into a flip-model swap chain.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

use super::MediaError;
use super::decoder::{DecodedFrame, Picture};
use crate::geometry::{copy_nv12, fit_rect};

struct Processor {
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    in_size: (u32, u32),
    out_size: (u32, u32),
}

pub struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    swap_chain: IDXGISwapChain1,
    upload: Option<(ID3D11Texture2D, ID3D11Texture2D, u32, u32)>, // (staging, default, w, h)
    processor: Option<Processor>,
    needs_resize: bool,
}

impl Renderer {
    pub fn new(hwnd: HWND) -> Result<Self, MediaError> {
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device: ID3D11Device = device.ok_or_else(|| MediaError::Other("no D3D11 device".into()))?;
            let context = context.ok_or_else(|| MediaError::Other("no D3D11 context".into()))?;
            let video_device: ID3D11VideoDevice = device.cast()?;
            let video_context: ID3D11VideoContext = context.cast()?;

            let dxgi_device: IDXGIDevice = device.cast()?;
            let adapter = dxgi_device.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: 0,
                Height: 0,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: 0,
                ..Default::default()
            };
            let swap_chain = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;
            // We handle Esc/fullscreen ourselves; Alt+Enter exclusive mode would fight the window.
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            Ok(Renderer {
                device,
                context,
                video_device,
                video_context,
                swap_chain,
                upload: None,
                processor: None,
                needs_resize: false,
            })
        }
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn resized(&mut self) {
        self.needs_resize = true;
    }

    fn ensure_upload(&mut self, w: u32, h: u32) -> Result<(), MediaError> {
        if matches!(self.upload, Some((_, _, uw, uh)) if uw == w && uh == h) {
            return Ok(());
        }
        let mut desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
        };
        unsafe {
            let mut staging = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            desc.Usage = D3D11_USAGE_DEFAULT;
            desc.CPUAccessFlags = 0;
            desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
            let mut default = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut default))?;
            self.upload = Some((staging.unwrap(), default.unwrap(), w, h));
        }
        self.processor = None;
        Ok(())
    }

    fn ensure_processor(&mut self, in_size: (u32, u32), out_size: (u32, u32)) -> Result<(), MediaError> {
        if matches!(&self.processor, Some(p) if p.in_size == in_size && p.out_size == out_size) {
            return Ok(());
        }
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            InputWidth: in_size.0,
            InputHeight: in_size.1,
            OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            OutputWidth: out_size.0,
            OutputHeight: out_size.1,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        unsafe {
            let enumerator = self.video_device.CreateVideoProcessorEnumerator(&desc)?;
            let processor = self.video_device.CreateVideoProcessor(&enumerator, 0)?;
            // Streams from Annex hosts are BT.709 limited range; output is full-range RGB.
            let in_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) };
            self.video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &in_cs);
            let out_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
            self.video_context.VideoProcessorSetOutputColorSpace(&processor, &out_cs);
            self.video_context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            let black = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 { RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 } },
            };
            self.video_context.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);
            self.processor = Some(Processor { enumerator, processor, in_size, out_size });
        }
        Ok(())
    }

    /// Uploads, converts and presents one frame.
    pub fn present(&mut self, f: &DecodedFrame) -> Result<(), MediaError> {
        unsafe {
            if self.needs_resize {
                self.processor = None;
                self.swap_chain.ResizeBuffers(0, 0, 0, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
                self.needs_resize = false;
            }
            let (w, h) = (f.width & !1, f.height & !1);
            // Input for the video processor: the decoder's own texture (hardware decode) or our
            // upload texture (software decode).
            let (input, slice) = match &f.picture {
                Picture::Gpu { texture, subresource, .. } => (texture.clone(), *subresource),
                Picture::Cpu { data, stride } => {
                    self.ensure_upload(w, h)?;
                    let (staging, default, _, _) = self.upload.clone().unwrap();
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    self.context.Map(&staging, 0, D3D11_MAP_WRITE, 0, Some(&mut mapped))?;
                    let dst_pitch = mapped.RowPitch as usize;
                    let dst_len = dst_pitch * h as usize * 3 / 2;
                    let dst = std::slice::from_raw_parts_mut(mapped.pData.cast::<u8>(), dst_len);
                    let src_pitch = *stride as usize;
                    let ok = copy_nv12(
                        data,
                        src_pitch,
                        src_pitch * f.height as usize,
                        dst,
                        dst_pitch,
                        dst_pitch * h as usize,
                        w as usize,
                        h as usize,
                    );
                    self.context.Unmap(&staging, 0);
                    if !ok {
                        return Err(MediaError::Other("decoded frame smaller than expected".into()));
                    }
                    self.context.CopyResource(&default, &staging);
                    (default, 0)
                }
            };

            let back: ID3D11Texture2D = self.swap_chain.GetBuffer(0)?;
            let mut bdesc = D3D11_TEXTURE2D_DESC::default();
            back.GetDesc(&mut bdesc);
            self.ensure_processor((w, h), (bdesc.Width, bdesc.Height))?;
            let p = self.processor.as_ref().unwrap();

            let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: slice },
                },
            };
            let mut in_view = None;
            self.video_device.CreateVideoProcessorInputView(&input, &p.enumerator, &in_desc, Some(&mut in_view))?;
            let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut out_view = None;
            self.video_device.CreateVideoProcessorOutputView(&back, &p.enumerator, &out_desc, Some(&mut out_view))?;

            let vis = (f.visible_width.min(w), f.visible_height.min(h));
            let src = RECT { left: 0, top: 0, right: vis.0 as i32, bottom: vis.1 as i32 };
            let r = fit_rect(vis.0, vis.1, bdesc.Width, bdesc.Height);
            let dst = RECT { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h };
            let full = RECT { left: 0, top: 0, right: bdesc.Width as i32, bottom: bdesc.Height as i32 };
            self.video_context.VideoProcessorSetStreamSourceRect(&p.processor, 0, true, Some(&src));
            self.video_context.VideoProcessorSetStreamDestRect(&p.processor, 0, true, Some(&dst));
            self.video_context.VideoProcessorSetOutputTargetRect(&p.processor, true, Some(&full));

            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(in_view),
                ..Default::default()
            };
            let streams = [stream];
            let blt = self.video_context.VideoProcessorBlt(&p.processor, out_view.as_ref(), 0, &streams);
            let [mut stream] = streams;
            std::mem::ManuallyDrop::drop(&mut stream.pInputSurface);
            blt?;
            // Sync interval 0: present as soon as possible; DWM composes it on the next vblank.
            self.swap_chain.Present(0, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }
}
