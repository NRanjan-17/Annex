import Foundation

/// macOS's own shortcuts (⌘Space, ⌘Tab, ⌃←/→/↑/↓, screenshots…) are taken by the system before
/// any app sees them. While the viewer is in front and controlling, Annex switches them off, so
/// they arrive as ordinary keys and go to the other computer, then switches them back on.
///
/// Private SkyLight calls, looked up at run time: if a macOS release drops them, the shortcuts
/// simply stay on this Mac. Switching one off outlives the app, so a crash must never leave them
/// off: before switching any off, Annex starts a copy of itself as a guard ([`runGuardIfAsked`])
/// that waits for this process to end and switches them back on if it ended without doing so
/// (a crash, Force Quit, `kill -9`). Released normally, the guard is stopped. The ids are also
/// saved and switched back on at the next launch, in case the guard was killed too (logging out).
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
    private static var guardProcess: Process?
    private static let guardFlag = "--annex-shortcut-guard"

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
            guard !off.isEmpty else { return }
            // The guard first: from here on, however this process ends, they come back on.
            guard let g = startGuard(off) else {
                capturing = false
                return Annex.log(.warn, "[input] system shortcuts stay on this Mac: couldn't start their guard")
            }
            guardProcess = g
            UserDefaults.standard.set(off.map(Int.init), forKey: savedKey)
            for id in off { _ = setEnabled(id, false) }
            switchedOff = off
            Annex.log(.info, "[input] system shortcuts go to the other computer (\(off.count) switched off here, guard \(g.processIdentifier))")
        } else {
            for id in switchedOff { _ = setEnabled(id, true) }
            if !switchedOff.isEmpty { Annex.log(.info, "[input] system shortcuts back on this Mac (\(switchedOff.count))") }
            switchedOff = []
            UserDefaults.standard.removeObject(forKey: savedKey)
            // Stopped, not asked to restore: a guard for a later capture may already be running.
            guardProcess?.terminate()
            guardProcess = nil
        }
    }

    private static func startGuard(_ off: [Int32]) -> Process? {
        guard let exe = Bundle.main.executableURL else { return nil }
        let p = Process()
        p.executableURL = exe
        p.arguments = [guardFlag, String(getpid()), off.map(String.init).joined(separator: ",")]
        p.standardInput = FileHandle.nullDevice
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        do { try p.run() } catch { return nil }
        return p
    }

    /// In the guard copy (started with `--annex-shortcut-guard <pid> <ids>`): waits for that
    /// process to end, switches the ids back on and exits, before any of the app starts. Stopped
    /// with SIGTERM when the app gives the shortcuts back itself.
    static func runGuardIfAsked() {
        let a = CommandLine.arguments
        guard a.count == 4, a[1] == guardFlag, let pid = pid_t(a[2]) else { return }
        let ids = a[3].split(separator: ",").compactMap { Int32($0) }
        let kq = kqueue()
        var watch = kevent(ident: UInt(pid), filter: Int16(EVFILT_PROC), flags: UInt16(EV_ADD | EV_ONESHOT),
                           fflags: NOTE_EXIT, data: 0, udata: nil)
        // Fails (ESRCH) if it already ended: restore right away.
        if kq >= 0, kevent(kq, &watch, 1, nil, 0, nil) == 0 {
            var fired = kevent()
            while kevent(kq, nil, 0, &fired, 1, nil) < 0 && errno == EINTR {}
        }
        if let setEnabled {
            for id in ids { _ = setEnabled(id, true) }
        }
        exit(0)
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
