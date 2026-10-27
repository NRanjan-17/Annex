//! Keyboard codes. Keys travel as USB HID usages (keyboard page 0x07), which name physical key
//! positions independently of layout and platform. Each side converts from and to its native
//! codes: macOS virtual key codes (`kVK_*`), Windows set-1 scancodes, where extended keys
//! carry `0xE0` in the high byte (`0xE048` = Up), and Linux evdev codes (`KEY_*`; X11 and GTK
//! keycodes are these plus 8).
//!
//! Sending positions rather than characters means the host's keyboard layout decides what a
//! key types, just like a USB keyboard plugged into it.

/// `(HID usage, macOS kVK code, Windows scancode, Linux evdev code)`. `NONE` marks keys a platform
/// doesn't have.
const NONE: u16 = u16::MAX;

#[rustfmt::skip]
const TABLE: &[(u16, u16, u16, u16)] = &[
    // Letters
    (0x04, 0x00, 0x1E, 30), (0x05, 0x0B, 0x30, 48), (0x06, 0x08, 0x2E, 46), (0x07, 0x02, 0x20, 32), // A B C D
    (0x08, 0x0E, 0x12, 18), (0x09, 0x03, 0x21, 33), (0x0A, 0x05, 0x22, 34), (0x0B, 0x04, 0x23, 35), // E F G H
    (0x0C, 0x22, 0x17, 23), (0x0D, 0x26, 0x24, 36), (0x0E, 0x28, 0x25, 37), (0x0F, 0x25, 0x26, 38), // I J K L
    (0x10, 0x2E, 0x32, 50), (0x11, 0x2D, 0x31, 49), (0x12, 0x1F, 0x18, 24), (0x13, 0x23, 0x19, 25), // M N O P
    (0x14, 0x0C, 0x10, 16), (0x15, 0x0F, 0x13, 19), (0x16, 0x01, 0x1F, 31), (0x17, 0x11, 0x14, 20), // Q R S T
    (0x18, 0x20, 0x16, 22), (0x19, 0x09, 0x2F, 47), (0x1A, 0x0D, 0x11, 17), (0x1B, 0x07, 0x2D, 45), // U V W X
    (0x1C, 0x10, 0x15, 21), (0x1D, 0x06, 0x2C, 44),                                         // Y Z
    // Digit row 1..9, 0
    (0x1E, 0x12, 0x02, 2), (0x1F, 0x13, 0x03, 3), (0x20, 0x14, 0x04, 4), (0x21, 0x15, 0x05, 5),
    (0x22, 0x17, 0x06, 6), (0x23, 0x16, 0x07, 7), (0x24, 0x1A, 0x08, 8), (0x25, 0x1C, 0x09, 9),
    (0x26, 0x19, 0x0A, 10), (0x27, 0x1D, 0x0B, 11),
    (0x28, 0x24, 0x1C, 28),   // Enter
    (0x29, 0x35, 0x01, 1),   // Escape
    (0x2A, 0x33, 0x0E, 14),   // Backspace
    (0x2B, 0x30, 0x0F, 15),   // Tab
    (0x2C, 0x31, 0x39, 57),   // Space
    (0x2D, 0x1B, 0x0C, 12),   // - _
    (0x2E, 0x18, 0x0D, 13),   // = +
    (0x2F, 0x21, 0x1A, 26),   // [ {
    (0x30, 0x1E, 0x1B, 27),   // ] }
    (0x31, 0x2A, 0x2B, 43),   // \ |
    (0x33, 0x29, 0x27, 39),   // ; :
    (0x34, 0x27, 0x28, 40),   // ' "
    (0x35, 0x32, 0x29, 41),   // ` ~
    (0x36, 0x2B, 0x33, 51),   // , <
    (0x37, 0x2F, 0x34, 52),   // . >
    (0x38, 0x2C, 0x35, 53),   // / ?
    (0x39, 0x39, 0x3A, 58),   // Caps Lock
    // F1..F12
    (0x3A, 0x7A, 0x3B, 59), (0x3B, 0x78, 0x3C, 60), (0x3C, 0x63, 0x3D, 61), (0x3D, 0x76, 0x3E, 62),
    (0x3E, 0x60, 0x3F, 63), (0x3F, 0x61, 0x40, 64), (0x40, 0x62, 0x41, 65), (0x41, 0x64, 0x42, 66),
    (0x42, 0x65, 0x43, 67), (0x43, 0x6D, 0x44, 68), (0x44, 0x67, 0x57, 87), (0x45, 0x6F, 0x58, 88),
    (0x46, NONE, 0xE037, 99), // Print Screen
    (0x47, NONE, 0x46, 70),   // Scroll Lock
    (0x48, NONE, 0x45, 119),   // Pause
    (0x49, 0x72, 0xE052, 110), // Insert (Help on Mac keyboards)
    (0x4A, 0x73, 0xE047, 102), // Home
    (0x4B, 0x74, 0xE049, 104), // Page Up
    (0x4C, 0x75, 0xE053, 111), // Delete (forward)
    (0x4D, 0x77, 0xE04F, 107), // End
    (0x4E, 0x79, 0xE051, 109), // Page Down
    (0x4F, 0x7C, 0xE04D, 106), // Right
    (0x50, 0x7B, 0xE04B, 105), // Left
    (0x51, 0x7D, 0xE050, 108), // Down
    (0x52, 0x7E, 0xE048, 103), // Up
    (0x53, 0x47, 0xE045, 69), // Num Lock (Clear on Mac keypads)
    (0x54, 0x4B, 0xE035, 98), // Keypad /
    (0x55, 0x43, 0x37, 55),   // Keypad *
    (0x56, 0x4E, 0x4A, 74),   // Keypad -
    (0x57, 0x45, 0x4E, 78),   // Keypad +
    (0x58, 0x4C, 0xE01C, 96), // Keypad Enter
    // Keypad 1..9, 0, .
    (0x59, 0x53, 0x4F, 79), (0x5A, 0x54, 0x50, 80), (0x5B, 0x55, 0x51, 81), (0x5C, 0x56, 0x4B, 75),
    (0x5D, 0x57, 0x4C, 76), (0x5E, 0x58, 0x4D, 77), (0x5F, 0x59, 0x47, 71), (0x60, 0x5B, 0x48, 72),
    (0x61, 0x5C, 0x49, 73), (0x62, 0x52, 0x52, 82), (0x63, 0x41, 0x53, 83),
    (0x64, 0x0A, 0x56, 86),   // ISO key next to left Shift (§ on Mac ISO keyboards)
    (0x65, 0x6E, 0xE05D, 127), // Menu / Application
    (0x67, 0x51, 0x59, 117),   // Keypad =
    // F13..F20
    (0x68, 0x69, 0x64, 183), (0x69, 0x6B, 0x65, 184), (0x6A, 0x71, 0x66, 185), (0x6B, 0x6A, 0x67, 186),
    (0x6C, 0x40, 0x68, 187), (0x6D, 0x4F, 0x69, 188), (0x6E, 0x50, 0x6A, 189), (0x6F, 0x5A, 0x6B, 190),
    (0x7F, 0x4A, 0xE020, 113), // Mute
    (0x80, 0x48, 0xE030, 115), // Volume Up
    (0x81, 0x49, 0xE02E, 114), // Volume Down
    (0x85, 0x5F, 0x7E, 95),   // Keypad , (JIS)
    (0x87, 0x5E, 0x73, 89),   // International1: JIS ろ / _
    (0x89, 0x5D, 0x7D, 124),   // International3: JIS ¥
    (0x90, 0x68, 0x72, 122),   // Lang1: Kana / Hangul
    (0x91, 0x66, 0x71, 123),   // Lang2: Eisu / Hanja
    // Modifiers
    (0xE0, 0x3B, 0x1D, 29),   // Left Control
    (0xE1, 0x38, 0x2A, 42),   // Left Shift
    (0xE2, 0x3A, 0x38, 56),   // Left Alt / Option
    (0xE3, 0x37, 0xE05B, 125), // Left GUI: Command / Windows
    (0xE4, 0x3E, 0xE01D, 97), // Right Control
    (0xE5, 0x3C, 0x36, 54),   // Right Shift
    (0xE6, 0x3D, 0xE038, 100), // Right Alt / Option (AltGr)
    (0xE7, 0x36, 0xE05C, 126), // Right GUI
];

fn find(f: impl Fn(&(u16, u16, u16, u16)) -> bool) -> Option<&'static (u16, u16, u16, u16)> {
    TABLE.iter().find(|e| f(e))
}

/// HID usage for a macOS virtual key code (`NSEvent.keyCode`).
pub fn hid_from_mac(code: u16) -> Option<u16> {
    find(|e| e.1 == code && code != NONE).map(|e| e.0)
}

/// macOS virtual key code for a HID usage.
pub fn mac_from_hid(usage: u16) -> Option<u16> {
    find(|e| e.0 == usage).map(|e| e.1).filter(|&c| c != NONE)
}

/// HID usage for a Windows scancode (`0xE0xx` for extended keys).
pub fn hid_from_windows(scancode: u16) -> Option<u16> {
    find(|e| e.2 == scancode && scancode != NONE).map(|e| e.0)
}

/// Windows scancode for a HID usage (`0xE0xx` for extended keys).
pub fn windows_from_hid(usage: u16) -> Option<u16> {
    find(|e| e.0 == usage).map(|e| e.2).filter(|&c| c != NONE)
}

/// HID usage for a Linux evdev code (`KEY_*`).
pub fn hid_from_linux(code: u16) -> Option<u16> {
    find(|e| e.3 == code && code != NONE).map(|e| e.0)
}

/// Linux evdev code for a HID usage.
pub fn linux_from_hid(usage: u16) -> Option<u16> {
    find(|e| e.0 == usage).map(|e| e.3).filter(|&c| c != NONE)
}

/// HID modifier usages (Control, Shift, Alt/Option, GUI; left then right).
pub fn is_modifier(usage: u16) -> bool {
    (0xE0..=0xE7).contains(&usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_per_platform() {
        for (i, a) in TABLE.iter().enumerate() {
            for b in &TABLE[i + 1..] {
                assert_ne!(a.0, b.0, "usage {:#x} twice", a.0);
                assert!(a.1 == NONE || a.1 != b.1, "mac code {:#x} twice", a.1);
                assert!(a.2 == NONE || a.2 != b.2, "windows scancode {:#x} twice", a.2);
                assert!(a.3 == NONE || a.3 != b.3, "linux code {} twice", a.3);
            }
        }
    }

    #[test]
    fn roundtrips() {
        for &(usage, mac, win, linux) in TABLE {
            if mac != NONE {
                assert_eq!(hid_from_mac(mac), Some(usage));
                assert_eq!(mac_from_hid(usage), Some(mac));
            }
            if win != NONE {
                assert_eq!(hid_from_windows(win), Some(usage));
                assert_eq!(windows_from_hid(usage), Some(win));
            }
            assert_eq!(hid_from_linux(linux), Some(usage));
            assert_eq!(linux_from_hid(usage), Some(linux));
        }
    }

    #[test]
    fn known_keys() {
        // A, Return, Left Command, Up arrow.
        assert_eq!(hid_from_mac(0x00), Some(0x04));
        assert_eq!(windows_from_hid(0x28), Some(0x1C));
        assert_eq!(hid_from_mac(0x37), Some(0xE3));
        assert_eq!(windows_from_hid(0xE3), Some(0xE05B));
        assert_eq!(windows_from_hid(0x52), Some(0xE048));
        // Print Screen has no Mac key; unknown codes map to nothing.
        assert_eq!(mac_from_hid(0x46), None);
        assert_eq!(hid_from_mac(NONE), None);
        assert_eq!(hid_from_windows(0x7FFF), None);
        // Linux: A, Up, Left Meta.
        assert_eq!(linux_from_hid(0x04), Some(30));
        assert_eq!(linux_from_hid(0x52), Some(103));
        assert_eq!(hid_from_linux(125), Some(0xE3));
        assert!(is_modifier(0xE1) && !is_modifier(0x04));
    }
}
