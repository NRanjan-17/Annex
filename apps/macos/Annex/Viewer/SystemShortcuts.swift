import Foundation

/// macOS's own shortcuts (⌘Space, ⌘Tab, ⌃←/→/↑/↓, screenshots…) are taken by the system before
/// any app sees them. While the viewer is in front and controlling, Annex switches them off, so
/// they arrive as ordinary keys and go to the other computer, then switches them back on.
///
/// Private SkyLight calls, looked up at run time: if a macOS release drops them, the shortcuts
/// simply stay on this Mac. Switching one off outlives the app, so the ones switched off are saved
/// first and switched back on at the next launch if Annex quit without doing it.
enum SystemShortcuts {
    /// Symbolic hotkey ids (`com.apple.symbolichotkeys`; ⌘Tab and ⌘⇧Tab are the Dock's 1 and 2).
    private static let ids: [Int32] = [
        1, 2,  // ⌘Tab, ⌘⇧Tab
        28, 29, 30, 31, 184,  // screenshots: ⌘⇧3, ⌃⌘⇧3, ⌘⇧4, ⌃⌘⇧4, ⌘⇧5
        32, 33, 36,  // Mission Control, application windows, show desktop
        52,  // ⌥⌘D: hide the Dock
        60, 61,  // ⌃Space, ⌃⌥Space: input sources
        64, 65,  // ⌘Space: Spotlight, ⌥⌘Space: Finder search
        79, 80, 81, 82,  // ⌃← and ⌃→: move a space left or right
    ] + Array(118...133)  // ⌃1…⌃16: switch to desktop n

    private static let savedKey = "systemShortcutsSwitchedOff"

    private typealias SetEnabled = @convention(c) (Int32, Bool) -> Int32
    private typealias IsEnabled = @convention(c) (Int32) -> Bool
    private static let setEnabled: SetEnabled? = symbol("CGSSetSymbolicHotKeyEnabled")
    private static let isEnabled: IsEnabled? = symbol("CGSIsSymbolicHotKeyEnabled")

    private static func symbol<T>(_ name: String) -> T? {
        guard let p = dlsym(UnsafeMutableRawPointer(bitPattern: -2), name) else { return nil }  // RTLD_DEFAULT
        return unsafeBitCast(p, to: T.self)
    }

    /// The ids switched off now (only those that were on: the user's own choices stay).
    private static var switchedOff: [Int32] = []
    private static var capturing = false

    /// Sends the system's shortcuts to the other computer (`true`) or gives them back.
    static func capture(_ on: Bool) {
        guard on != capturing else { return }
        capturing = on
        guard let setEnabled, let isEnabled else {
            if on { Annex.log(.info, "[input] system shortcuts stay on this Mac: macOS doesn't offer the switch") }
            return
        }
        if on {
            let off = ids.filter { isEnabled($0) }
            UserDefaults.standard.set(off.map(Int.init), forKey: savedKey)
            for id in off { _ = setEnabled(id, false) }
            switchedOff = off
            Annex.log(.info, "[input] system shortcuts go to the other computer (\(off.count) switched off here)")
        } else {
            for id in switchedOff { _ = setEnabled(id, true) }
            Annex.log(.info, "[input] system shortcuts back on this Mac (\(switchedOff.count))")
            switchedOff = []
            UserDefaults.standard.removeObject(forKey: savedKey)
        }
    }

    /// At launch: switches back on what a previous run left off (it crashed or was killed mid-session).
    static func restoreLeftovers() {
        guard let saved = UserDefaults.standard.array(forKey: savedKey) as? [Int], !saved.isEmpty else { return }
        if let setEnabled {
            for id in saved { _ = setEnabled(Int32(id), true) }
        }
        Annex.log(.info, "[input] switched \(saved.count) system shortcut(s) back on, left off by the last run")
        UserDefaults.standard.removeObject(forKey: savedKey)
    }
}
