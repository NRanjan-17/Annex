//! Screen capture with Windows.Graphics.Capture (Windows 10 1903+). Frames arrive on a capture
//! thread as BGRA D3D11 textures.

use std::sync::Arc;

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::core::{IInspectable, Interface};

use super::MediaError;

pub type FrameFn = Arc<dyn Fn(&ID3D11Texture2D) + Send + Sync>;

/// The WinRT Direct3D device wrapper is agile (usable from any thread).
struct AgileDevice(IDirect3DDevice);
// SAFETY: IDirect3DDevice is an agile WinRT object.
unsafe impl Send for AgileDevice {}
unsafe impl Sync for AgileDevice {}

impl AgileDevice {
    // A method (not field access) so closures capture the whole Send wrapper.
    fn get(&self) -> &IDirect3DDevice {
        &self.0
    }
}

pub struct ScreenCapture {
    _item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
}

impl ScreenCapture {
    pub fn start(device: &ID3D11Device, monitor: HMONITOR, on_frame: FrameFn) -> Result<Self, MediaError> {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
            let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = interop.CreateForMonitor(monitor)?;
            let dxgi: IDXGIDevice = device.cast()?;
            let winrt_device: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            let size = item.Size()?;
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &winrt_device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                2,
                size,
            )?;
            let last_size = std::sync::Mutex::new(size);
            let device_for_recreate = AgileDevice(winrt_device.clone());
            pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |pool, _| {
                let Some(pool) = pool.as_ref() else { return Ok(()) };
                let frame = pool.TryGetNextFrame()?;
                let content: SizeInt32 = frame.ContentSize()?;
                {
                    // Resolution changed (or display rotated): recreate the pool at the new size.
                    let mut last = last_size.lock().unwrap();
                    if content.Width != last.Width || content.Height != last.Height {
                        *last = content;
                        pool.Recreate(
                            device_for_recreate.get(),
                            DirectXPixelFormat::B8G8R8A8UIntNormalized,
                            2,
                            content,
                        )?;
                        return Ok(());
                    }
                }
                let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
                let texture: ID3D11Texture2D = access.GetInterface()?;
                on_frame(&texture);
                Ok(())
            }))?;
            let session = pool.CreateCaptureSession(&item)?;
            // Available on Windows 11 / 10 20348+; older builds keep the yellow border.
            let _ = session.SetIsBorderRequired(false);
            let _ = session.SetIsCursorCaptureEnabled(true);
            session.StartCapture()?;
            Ok(ScreenCapture { _item: item, pool, session })
        }
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
