import AppKit
import ApplicationServices
import CoreGraphics

/// Replays a viewer's mouse and keyboard on this Mac with CGEvent. Posting events needs the
/// Accessibility permission (System Settings → Privacy & Security → Accessibility); without it
/// macOS drops them silently, so the UI checks `isTrusted` and asks first.
final class InputInjector: @unchecked Sendable {
    private let queue = DispatchQueue(label: "annex.input", qos: .userInteractive)
    private let source = CGEventSource(stateID: .hidSystemState)

    // State, only touched on `queue`.
    private var location = CGPoint.zero
    private var buttonsDown: Set<CGMouseButton> = []
    private var flags: CGEventFlags = []
    private var lastClick: (button: CGMouseButton, time: TimeInterval, at: CGPoint, count: Int64)?

    /// Posting input events needs the Accessibility ("Device Control") permission.
    /// CGPreflightPostEventAccess is the exact check for CGEventPost; AXIsProcessTrusted can stay
    /// stale until relaunch, so either one saying yes is enough.
    static var isTrusted: Bool { CGPreflightPostEventAccess() || AXIsProcessTrusted() }

    /// Shows the system prompt that sends the user to the Accessibility list (once per launch).
    static func requestTrust() {
        _ = CGRequestPostEventAccess()
        let key = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
        _ = AXIsProcessTrustedWithOptions([key: true] as CFDictionary)
    }

    static func openAccessibilitySettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility") {
            NSWorkspace.shared.open(url)
        }
    }

    /// `display`: where pointer positions land (the mirrored display or the session's virtual one).
    func inject(_ event: InputEvent, display: CGDirectDisplayID) {
        queue.async { [self] in handle(event, display: display) }
    }

    /// Releases anything still held, e.g. when a session ends mid-drag or mid-shortcut.
    func releaseAll() {
        queue.async { [self] in
            for b in buttonsDown { post(mouseType(b, down: false), button: b) }
            buttonsDown.removeAll()
            for (usage, _) in Self.modifierFlags where flags.contains(Self.flag(for: usage)) {
                if let code = Annex.macKeyCode(hid: usage) { postKey(code, down: false) }
            }
            flags = []
        }
    }

    private static let modifierFlags: [(UInt16, CGEventFlags)] = [
        (0xE0, .maskControl), (0xE4, .maskControl),
        (0xE1, .maskShift), (0xE5, .maskShift),
        (0xE2, .maskAlternate), (0xE6, .maskAlternate),
        (0xE3, .maskCommand), (0xE7, .maskCommand),
    ]

    private static func flag(for usage: UInt16) -> CGEventFlags {
        modifierFlags.first { $0.0 == usage }?.1 ?? []
    }

    private func handle(_ event: InputEvent, display: CGDirectDisplayID) {
        switch event {
        case .mouseMove(let x, let y):
            let b = CGDisplayBounds(display)
            location = CGPoint(x: b.minX + (b.width - 1) * CGFloat(x) / 65535,
                               y: b.minY + (b.height - 1) * CGFloat(y) / 65535)
            // While a button is held macOS expects drag events, or drags (selection, moving windows) stall.
            if let held = [CGMouseButton.left, .right, .center].first(where: buttonsDown.contains) {
                post(held == .left ? .leftMouseDragged : held == .right ? .rightMouseDragged : .otherMouseDragged, button: held)
            } else {
                post(.mouseMoved, button: .left)
            }
        case .mouseDelta(let dx, let dy):
            // Like a real mouse: from wherever the pointer is, across every screen (so it can leave
            // the Annex display for this Mac's own), stopping at the outer edges.
            let start = CGEvent(source: nil)?.location ?? location
            location = Self.clampToScreens(CGPoint(x: start.x + CGFloat(dx), y: start.y + CGFloat(dy)))
            if let held = [CGMouseButton.left, .right, .center].first(where: buttonsDown.contains) {
                post(held == .left ? .leftMouseDragged : held == .right ? .rightMouseDragged : .otherMouseDragged, button: held)
            } else {
                post(.mouseMoved, button: .left)
            }
        case .mouseButton(let name, let down):
            let button: CGMouseButton = name == "right" ? .right : name == "middle" ? .center : .left
            if down { buttonsDown.insert(button) } else { buttonsDown.remove(button) }
            post(mouseType(button, down: down), button: button, clickState: down ? clickCount(button) : lastClick?.count ?? 1)
        case .scroll(let dx, let dy):
            guard let e = CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2, wheel1: dy, wheel2: dx, wheel3: 0) else { return }
            // Trackpad-style: apps scroll by exactly these pixels instead of animating each event
            // as a mouse-wheel step, which turns a stream of small scrolls into a laggy judder.
            e.setIntegerValueField(.scrollWheelEventIsContinuous, value: 1)
            e.flags = flags
            e.post(tap: .cghidEventTap)
        case .key(let usage, let down):
            guard let code = Annex.macKeyCode(hid: usage) else { return }
            let f = Self.flag(for: usage)
            if !f.isEmpty {
                if down { flags.insert(f) } else { flags.remove(f) }
            }
            postKey(code, down: down)
        }
    }

    /// The nearest point that's on a screen: inside the display the point is on, or else the
    /// closest edge of the nearest display.
    static func clampToScreens(_ p: CGPoint) -> CGPoint {
        var count: UInt32 = 0
        var ids = [CGDirectDisplayID](repeating: 0, count: 16)
        guard CGGetActiveDisplayList(16, &ids, &count) == .success, count > 0 else { return p }
        var best = p
        var bestDistance = CGFloat.infinity
        for id in ids.prefix(Int(count)) {
            let b = CGDisplayBounds(id)
            let q = CGPoint(x: min(max(p.x, b.minX), b.maxX - 1), y: min(max(p.y, b.minY), b.maxY - 1))
            let d = hypot(q.x - p.x, q.y - p.y)
            if d < bestDistance { best = q; bestDistance = d }
            if d == 0 { break }
        }
        return best
    }

    private func mouseType(_ b: CGMouseButton, down: Bool) -> CGEventType {
        switch b {
        case .left: return down ? .leftMouseDown : .leftMouseUp
        case .right: return down ? .rightMouseDown : .rightMouseUp
        default: return down ? .otherMouseDown : .otherMouseUp
        }
    }

    /// Double and triple clicks: macOS counts clicks from the event's click state, not by itself.
    private func clickCount(_ button: CGMouseButton) -> Int64 {
        let now = ProcessInfo.processInfo.systemUptime
        var count: Int64 = 1
        if let last = lastClick, last.button == button, now - last.time < NSEvent.doubleClickInterval,
           abs(last.at.x - location.x) < 5, abs(last.at.y - location.y) < 5 {
            count = last.count + 1
        }
        lastClick = (button, now, location, count)
        return count
    }

    private func post(_ type: CGEventType, button: CGMouseButton, clickState: Int64 = 0) {
        guard let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: location, mouseButton: button) else { return }
        if clickState > 0 { e.setIntegerValueField(.mouseEventClickState, value: clickState) }
        e.flags = flags
        e.post(tap: .cghidEventTap)
    }

    private func postKey(_ code: CGKeyCode, down: Bool) {
        guard let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down) else { return }
        e.flags = flags
        e.post(tap: .cghidEventTap)
    }
}
