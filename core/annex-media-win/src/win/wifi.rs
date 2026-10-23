//! Wi-Fi tuning while streaming. Windows periodically scans for networks in the background, which
//! stalls traffic for tens of milliseconds, and power-saves the radio between packets. "Media
//! streaming mode" plus disabling background scans removes both (the same trick game-streaming
//! clients use). Reference-counted across sessions and restored when the last one ends.

use std::sync::Mutex;

use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows::Win32::NetworkManagement::WiFi::{
    WLAN_INTERFACE_INFO, WLAN_INTERFACE_INFO_LIST, WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle,
    WlanSetInterface, wlan_intf_opcode_background_scan_enabled, wlan_intf_opcode_media_streaming_mode,
};

static ACTIVE: Mutex<u32> = Mutex::new(0);

/// More Wi-Fi adapters than any PC has; a larger count would be a corrupt list.
const MAX_INTERFACES: usize = 64;

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
        let mut raw: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        let status = WlanEnumInterfaces(handle, None, &mut raw);
        // Only read the list Windows handed back on success, and never trust an absurd count.
        if let (true, Some(list)) = (status == ERROR_SUCCESS.0, std::ptr::NonNull::new(raw)) {
            // InterfaceInfo is a variable-length array: dwNumberOfItems entries follow the header
            // in the block Windows allocated (the binding declares it with one element).
            let count = (list.as_ref().dwNumberOfItems as usize).min(MAX_INTERFACES);
            let first = std::ptr::addr_of!((*list.as_ptr()).InterfaceInfo).cast::<WLAN_INTERFACE_INFO>();
            for i in 0..count {
                let item = first.add(i).read();
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
            WlanFreeMemory(list.as_ptr().cast());
        }
        let _ = WlanCloseHandle(handle, None);
    }
}
