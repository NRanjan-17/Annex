import Foundation

/// A Mac's shortcuts on a Windows or Linux computer: ⌘ does what Ctrl does there, so ⌘C, ⌘V,
/// ⌘Z and ⌘S copy, paste, undo and save as they do on the Mac. Without this ⌘ arrives as the
/// Windows key (⌘V opened Windows' clipboard history). Three ⌘ shortcuts get the PC's own:
/// ⌘Tab switches windows (Alt+Tab), ⌘Space opens Start or the launcher (the Windows key), ⌘Q
/// closes the window (Alt+F4). Ctrl takes the Windows key's place, so ⌃← still snaps windows.
///
/// Works on macOS virtual key codes, before they become USB HID usages.
struct PCShortcuts {
    typealias Key = (code: UInt16, down: Bool)

    private static let leftCommand: UInt16 = 0x37, rightCommand: UInt16 = 0x36
    private static let leftControl: UInt16 = 0x3B, rightControl: UInt16 = 0x3E
    private static let leftOption: UInt16 = 0x3A
    private static let tab: UInt16 = 0x30, space: UInt16 = 0x31, q: UInt16 = 0x0C, f4: UInt16 = 0x76

    /// What the ⌘ being held sends now.
    private enum Command: Equatable {
        case up
        /// Ctrl (left or right, like the ⌘ pressed).
        case control(UInt16)
        /// Alt, since ⌘Tab: Tab again moves on, letting go of ⌘ picks the window.
        case alt
        /// Nothing more: ⌘Space or ⌘Q was sent in full.
        case spent
    }

    private var command = Command.up
    /// The ⌘ key that's down (the other one is ignored while it is).
    private var commandKey: UInt16 = 0

    /// The keys to send for one press or release on this Mac.
    mutating func translate(_ code: UInt16, down: Bool) -> [Key] {
        if code == Self.leftCommand || code == Self.rightCommand {
            if down {
                guard command == .up else { return [] }
                commandKey = code
                let control = code == Self.rightCommand ? Self.rightControl : Self.leftControl
                command = .control(control)
                return [(control, true)]
            }
            guard code == commandKey else { return [] }
            defer { command = .up }
            switch command {
            case .control(let c): return [(c, false)]
            case .alt: return [(Self.leftOption, false)]
            case .up, .spent: return []
            }
        }
        if code == Self.leftControl { return [(Self.leftCommand, down)] }
        if code == Self.rightControl { return [(Self.rightCommand, down)] }
        guard down, case .control(let control) = command else { return [(code, down)] }
        switch code {
        case Self.tab:
            command = .alt
            return [(control, false), (Self.leftOption, true), (Self.tab, true)]
        case Self.space:
            command = .spent
            return [(control, false), (Self.leftCommand, true), (Self.leftCommand, false)]
        case Self.q:
            command = .spent
            return [(control, false), (Self.leftOption, true), (Self.f4, true), (Self.f4, false), (Self.leftOption, false)]
        default:
            return [(code, true)]
        }
    }
}
