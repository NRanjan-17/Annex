use annex_proto::DisplayInfo;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW, GetDC, GetDeviceCaps, GetMonitorInfoW,
    HDC, HMONITOR, HORZSIZE, LOGPIXELSX, MONITORINFO, MONITORINFOEXW, ReleaseDC, VERTSIZE,
};
use windows::core::{BOOL, PCWSTR};

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

/// A connected monitor.
#[derive(Debug, Clone)]
pub struct Monitor {
    pub handle: HMONITOR,
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub primary: bool,
}

// SAFETY: HMONITOR is a process-wide handle value, not a pointer we dereference.
unsafe impl Send for Monitor {}
unsafe impl Sync for Monitor {}

/// All monitors, primary first. Sizes are physical pixels of the current mode.
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the &mut Vec passed below, alive for the duration of the call.
        let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        list.push(h);
        true.into()
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut handles as *mut _ as isize));
    }
    let mut out: Vec<Monitor> = handles
        .into_iter()
        .filter_map(|h| unsafe {
            let mut mi = MONITORINFOEXW::default();
            mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if !GetMonitorInfoW(h, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
                return None;
            }
            let len = mi.szDevice.iter().position(|c| *c == 0).unwrap_or(mi.szDevice.len());
            let name = String::from_utf16_lossy(&mi.szDevice[..len]);
            let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
            let (width, height, refresh_hz) =
                if EnumDisplaySettingsW(PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() {
                    (dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency.max(1))
                } else {
                    let r = mi.monitorInfo.rcMonitor;
                    ((r.right - r.left) as u32, (r.bottom - r.top) as u32, 60)
                };
            Some(Monitor { handle: h, name, width, height, refresh_hz, primary: mi.monitorInfo.dwFlags & 1 != 0 })
        })
        .collect();
    out.sort_by_key(|m| !m.primary);
    out
}
