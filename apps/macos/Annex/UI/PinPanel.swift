import AppKit
import SwiftUI

/// Floating, non-modal panel that shows the pairing PIN on the host while the viewer's user
/// types it. Closed automatically when pairing finishes.
@MainActor
final class PinPanel {
    private let panel: NSPanel

    init(pin: String, peerName: String, fingerprint: String, onCancel: @escaping () -> Void) {
        panel = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 360, height: 220),
                        styleMask: [.titled, .closable, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.title = "Pair with \(peerName)"
        panel.level = .floating
        panel.isReleasedWhenClosed = false
        panel.hidesOnDeactivate = false
        let view = PinView(pin: pin, peerName: peerName, fingerprint: fingerprint) { [panel] in
            onCancel()
            panel.close()
        }
        panel.contentView = NSHostingView(rootView: view)
        panel.center()
    }

    func show() {
        NSApp.activate(ignoringOtherApps: true)
        panel.makeKeyAndOrderFront(nil)
    }

    func close() {
        panel.close()
    }
}

private struct PinView: View {
    let pin: String
    let peerName: String
    let fingerprint: String
    let cancel: () -> Void

    var body: some View {
        VStack(spacing: 12) {
            Text("“\(peerName)” wants to view this Mac").font(.headline)
            Text("Enter this PIN on \(peerName):").foregroundStyle(.secondary)
            Text(pin)
                .font(.system(size: 40, weight: .semibold, design: .monospaced))
                .textSelection(.enabled)
                .accessibilityLabel("PIN \(pin.map(String.init).joined(separator: " "))")
            Text("Its identity code: \(fingerprint)").font(.caption).foregroundStyle(.secondary)
            Button("Don’t Allow", role: .cancel, action: cancel).keyboardShortcut(.cancelAction)
        }
        .padding(20)
        .frame(width: 360)
    }
}
