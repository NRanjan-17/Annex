use annex_proto::DisplayInfo;
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetDC, GetDeviceCaps, HORZSIZE, LOGPIXELSX, ReleaseDC,
    VERTSIZE,
};

/// Physical properties of the primary display (pixels are physical, not DPI-scaled).
pub fn primary_display_info() -> DisplayInfo {
    let mut info =
        DisplayInfo { width_px: 1920, height_px: 1080, width_mm: 0, height_mm: 0, refresh_hz: 60, scale_percent: 100 };
    unsafe {
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        if EnumDisplaySettingsW(None, ENUM_CURRENT_SETTINGS, &mut dm).as_bool() {
            info.width_px = dm.dmPelsWidth;
            info.height_px = dm.dmPelsHeight;
            if dm.dmDisplayFrequency > 1 {
                info.refresh_hz = dm.dmDisplayFrequency;
            }
        }
        let hdc = GetDC(None);
        if !hdc.is_invalid() {
            info.width_mm = GetDeviceCaps(Some(hdc), HORZSIZE).max(0) as u32;
            info.height_mm = GetDeviceCaps(Some(hdc), VERTSIZE).max(0) as u32;
            let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSX);
            if dpi > 0 {
                info.scale_percent = (dpi as u32 * 100) / 96;
            }
            ReleaseDC(None, hdc);
        }
    }
    info
}
