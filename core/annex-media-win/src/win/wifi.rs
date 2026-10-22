//! Wi-Fi tuning while streaming. Windows periodically scans for networks in the background, which
//! stalls traffic for tens of milliseconds, and power-saves the radio between packets. "Media
//! streaming mode" plus disabling background scans removes both (the same trick game-streaming
//! clients use). Reference-counted across sessions and restored when the last one ends.

use std::sync::Mutex;

use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows::Win32::NetworkManagement::WiFi::{
    WLAN_INTERFACE_INFO_LIST, WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanSetInterface,
    wlan_intf_opcode_background_scan_enabled, wlan_intf_opcode_media_streaming_mode,
};

static ACTIVE: Mutex<u32> = Mutex::new(0);

/// A session started (`true`) or ended (`false`). The first start turns low-latency mode on,
/// the last end turns it off again. No-op without Wi-Fi.
pub fn wifi_low_latency(session_started: bool) {
    let mut n = ACTIVE.lock().unwrap();
    let before = *n;
    *n = if session_started { before + 1 } else { before.saturating_sub(1) };
    match (before, *n) {
        (0, 1) => apply(true),
        (1, 0) => apply(false),
        _ => {}
    }
}

fn apply(low_latency: bool) {
    // SAFETY: plain WLAN API calls; the interface list is freed with WlanFreeMemory.
    unsafe {
        let mut version = 0u32;
        let mut handle = HANDLE::default();
        if WlanOpenHandle(2, None, &mut version, &mut handle) != ERROR_SUCCESS.0 {
            return; // no WLAN service (desktop without Wi-Fi)
        }
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(handle, None, &mut list) == ERROR_SUCCESS.0 && !list.is_null() {
            let count = (*list).dwNumberOfItems as usize;
            let items = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), count);
            for item in items {
                let streaming: u32 = u32::from(low_latency);
                let scanning: u32 = u32::from(!low_latency);
                let set = |opcode, value: &u32| {
                    WlanSetInterface(
                        handle,
                        &item.InterfaceGuid,
                        opcode,
                        std::mem::size_of::<u32>() as u32,
                        (value as *const u32).cast(),
                        None,
                    )
                };
                let a = set(wlan_intf_opcode_media_streaming_mode, &streaming);
                let b = set(wlan_intf_opcode_background_scan_enabled, &scanning);
                tracing::info!(low_latency, media_streaming = a, background_scan = b, "wifi tuning");
            }
            WlanFreeMemory(list.cast());
        }
        let _ = WlanCloseHandle(handle, None);
    }
}
