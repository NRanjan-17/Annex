//! Control-protocol constants for the AnnexIdd driver. Mirrors
//! `drivers/windows/AnnexIdd/AnnexIddControl.h`; the tests pin codes and sizes.

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MONITORS: u32 = 4;

const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}
const FILE_DEVICE_UNKNOWN: u32 = 0x22;

pub const IOCTL_ADD_MONITOR: u32 = ctl_code(FILE_DEVICE_UNKNOWN, 0x901, 0, 0);
pub const IOCTL_REMOVE_MONITOR: u32 = ctl_code(FILE_DEVICE_UNKNOWN, 0x902, 0, 0);
pub const IOCTL_PING: u32 = ctl_code(FILE_DEVICE_UNKNOWN, 0x903, 0, 0);

/// {6F2B8E14-3C51-4C0E-9D3A-7A1E5B0C4D21}
pub const INTERFACE_GUID: u128 = 0x6f2b8e14_3c51_4c0e_9d3a_7a1e5b0c4d21;

/// Name the driver puts in each monitor's EDID (shown in Settings → Display).
pub const MONITOR_NAME: &str = "Annex Display";

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AddMonitor {
    pub version: u32,
    pub serial: u32,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub width_mm: u32,
    pub height_mm: u32,
    pub reserved: u32,
    pub owner: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AddMonitorResult {
    pub connector_index: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoveMonitor {
    pub version: u32,
    pub connector_index: u32,
    pub owner: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ping {
    pub version: u32,
    pub reserved: u32,
    pub owner: u64,
}

/// Stable 32-bit monitor serial from a viewer's certificate fingerprint (same as macOS).
pub fn serial_for(fingerprint: &str) -> u32 {
    fingerprint.get(..8).and_then(|h| u32::from_str_radix(h, 16).ok()).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_codes_match_ctl_code() {
        assert_eq!(IOCTL_ADD_MONITOR, 0x0022_2404);
        assert_eq!(IOCTL_REMOVE_MONITOR, 0x0022_2408);
        assert_eq!(IOCTL_PING, 0x0022_240C);
    }

    #[test]
    fn struct_layouts_match_the_driver() {
        assert_eq!(std::mem::size_of::<AddMonitor>(), 40);
        assert_eq!(std::mem::size_of::<RemoveMonitor>(), 16);
        assert_eq!(std::mem::size_of::<Ping>(), 16);
        assert_eq!(std::mem::size_of::<AddMonitorResult>(), 4);
    }

    #[test]
    fn serial_matches_macos_rule() {
        assert_eq!(serial_for("deadbeef0011"), 0xDEAD_BEEF);
        assert_eq!(serial_for("zz"), 1);
    }
}
