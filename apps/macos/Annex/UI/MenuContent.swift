import SwiftUI

/// The menu bar window: share this Mac, see what's live, connect to a computer nearby. Black and
/// white like the main window; the popover is the floating layer, so everything in it is flat.
struct MenuContent: View {
    @ObservedObject var model: AppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
                .padding(.horizontal, 16)
                .padding(.top, 16)
                .padding(.bottom, 14)
            if let err = model.startupError {
                Notice(text: err, systemImage: "exclamationmark.triangle.fill")
                    .padding(.horizontal, 12)
                    .padding(.bottom, 10)
            }
            VStack(spacing: 8) {
                ShareRow(model: model)
                if model.isViewing || model.hostStatus.viewerName != nil {
                    LiveBlock(model: model)
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
            }
            .padding(.horizontal, 12)
            NearbySection(model: model)
                .padding(.top, 16)
            Divider().opacity(0.4).padding(.top, 8)
            footer
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
        }
        .frame(width: 340)
        .background(Color(white: 0.04).opacity(0.94))
        .preferredColorScheme(.dark)
        .tint(Color(white: 0.82))
        .animation(.spring(response: 0.45, dampingFraction: 0.86), value: model.isViewing)
        .animation(.spring(response: 0.45, dampingFraction: 0.86), value: model.hostStatus.viewerName)
    }

    private var header: some View {
        HStack(spacing: 12) {
            AppIcon(size: 40)
                .overlay(alignment: .topTrailing) {
                    if model.isHosting { LiveDot(size: 8).offset(x: -1, y: 2) }
                }
            VStack(alignment: .leading, spacing: 2) {
                Text(model.deviceName.isEmpty ? "Annex" : model.deviceName)
                    .font(.system(size: 14, weight: .semibold, design: .rounded))
                    .lineLimit(1)
                if model.settings.showTechnicalDetails {
                    Text(model.fingerprintDisplay)
                        .font(.system(size: 10.5, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                        .help("This Mac's identity code. Compare it with what the other device shows when pairing.")
                } else {
                    Text(model.isHosting ? "Sharing" : "Not sharing")
                        .font(.system(size: 11.5)).foregroundStyle(.secondary)
                }
            }
            Spacer()
            CircleButton(symbol: "macwindow", help: "Open Annex") { open(.home) }
            CircleButton(symbol: "gearshape", help: "Settings") { open(.settings) }
                .keyboardShortcut(",")
        }
    }

    private var footer: some View {
        HStack {
            Text("Annex \(Annex.version)")
                .font(.system(size: 11))
                .foregroundStyle(.tertiary)
            Spacer()
            Button("Quit Annex") { model.quit() }
                .buttonStyle(.plain)
                .font(.system(size: 11.5, weight: .medium))
                .foregroundStyle(.secondary)
                .keyboardShortcut("q")
        }
    }

    private func open(_ pane: Pane) {
        model.pane = pane
        openWindow(id: "main")
        // A menu bar app isn't frontmost; bring the window forward.
        NSApp.activate(ignoringOtherApps: true)
    }
}

// MARK: Share

/// One switch: share this Mac. Below it, only what matters while it's on.
private struct ShareRow: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Share this Mac").font(.system(size: 13, weight: .semibold))
                    Text(summary).font(.system(size: 11.5)).foregroundStyle(.secondary).lineLimit(2)
                }
                Spacer(minLength: 0)
                Toggle("Share this Mac", isOn: Binding(get: { model.isHosting }, set: { model.setHosting($0) }))
                    .labelsHidden()
                    .toggleStyle(.switch)
                    .controlSize(.small)
            }
            if model.displays.count > 1 {
                Picker("Screen", selection: $model.selectedDisplayID) {
                    ForEach(model.displays) { d in Text(d.name).tag(Optional(d.id)) }
                }
                .controlSize(.small)
                .disabled(model.isHosting)
            }
            if let err = model.hostStatus.error {
                Notice(text: err, systemImage: "exclamationmark.triangle.fill")
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Color.white.opacity(0.05)))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Color.white.opacity(0.07)))
    }

    private var summary: String {
        guard model.isHosting else { return "Other computers can't see it." }
        let s = model.hostStatus
        if s.viewerName != nil { return "Connected." }
        if s.listeningPort != nil { return "Waiting for a computer to connect." }
        return "Starting…"
    }
}

// MARK: Live

/// What's live, the same numbers as the main window's session bar, smaller.
private struct LiveBlock: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 7) {
                LiveDot(size: 7)
                Text(title).font(.system(size: 13, weight: .semibold)).lineLimit(1)
                Spacer(minLength: 0)
            }
            if !stats.isEmpty {
                HStack(alignment: .firstTextBaseline, spacing: 18) {
                    ForEach(stats, id: \.label) { s in
                        VStack(alignment: .leading, spacing: 0) {
                            HStack(alignment: .firstTextBaseline, spacing: 2) {
                                Text(s.value).font(.system(size: 24, weight: .light, design: .rounded)).monospacedDigit()
                                Text(s.unit).font(.system(size: 11, weight: .medium)).foregroundStyle(.secondary)
                            }
                            Text(s.label).font(.system(size: 10.5)).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            if let note {
                Text(note).font(.system(size: 11)).foregroundStyle(.secondary)
            }
            HStack(spacing: 8) {
                Button(model.isViewing ? "Disconnect" : "Stop Sharing") {
                    if model.isViewing { model.disconnectViewer() } else { model.setHosting(false) }
                }
                .buttonStyle(PillStyle(prominent: true))
                if !model.isViewing, !model.hostStatus.extendViewers.isEmpty {
                    Button("Arrange Displays") {
                        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.Displays-Settings.extension")!)
                    }
                    .buttonStyle(PillStyle(prominent: false))
                }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Color.white.opacity(0.08)))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Color.annexLive.opacity(0.35)))
    }

    private struct Stat { let value: String; let unit: String; let label: String }

    private var stats: [Stat] {
        if model.isViewing, let l = model.latency {
            return [Stat(value: String(format: "%.0f", l.p50Ms), unit: "ms", label: "Delay"),
                    Stat(value: String(format: "%.0f", l.fps), unit: "fps", label: "Smoothness")]
        }
        if !model.isViewing, let s = model.hostStatus.stream {
            return [Stat(value: "\(s.fps)", unit: "fps", label: "Smoothness"),
                    Stat(value: "\(s.width)×\(s.height)", unit: "", label: "Picture")]
        }
        return []
    }

    private var title: String {
        if model.isViewing { return model.viewerStatus ?? "Connected" }
        let s = model.hostStatus
        if let name = s.extendViewers.first { return "Extending to \(name)" }
        if let name = s.mirrorViewers.first { return "Showing this screen on \(name)" }
        return "Connected"
    }

    private var note: String? {
        if model.isViewing, model.controllingHost { return "You're in control · ⌃⌥⇧Q to leave" }
        let controllers = model.hostStatus.controllingViewers
        if !model.isViewing, !controllers.isEmpty {
            return "\(controllers.joined(separator: ", ")) can use your mouse and keyboard"
        }
        return nil
    }
}

// MARK: Nearby

private struct NearbySection: View {
    @ObservedObject var model: AppModel

    private var sharing: [PeerInfo] { model.discovered.filter(\.isSharing) }
    private var notSharing: Int { model.discovered.count - sharing.count }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Eyebrow(text: "Nearby")
                Spacer()
                ScanningDots()
            }
            .padding(.horizontal, 16)
            if sharing.isEmpty {
                Text(notSharing > 0
                     ? "Waiting for a computer to share · \(notSharing) not sharing"
                     : "Waiting for a computer… Open Annex on it and turn on sharing.")
                    .font(.system(size: 11.5)).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 6)
            } else {
                VStack(spacing: 2) {
                    ForEach(sharing) { peer in
                        PeerRow(model: model, peer: peer)
                    }
                }
                .padding(.horizontal, 8)
                if notSharing > 0 {
                    Text("+\(notSharing) not sharing")
                        .font(.system(size: 11)).foregroundStyle(.tertiary)
                        .padding(.horizontal, 16)
                }
            }
            AddressField(model: model)
                .padding(.horizontal, 12)
                .padding(.top, 6)
        }
    }
}

private struct PeerRow: View {
    @ObservedObject var model: AppModel
    let peer: PeerInfo
    @State private var hovered = false

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: PeerLook.symbol(for: peer))
                .font(.system(size: 13))
                .frame(width: 30, height: 30)
                .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Color.white.opacity(0.07)))
            VStack(alignment: .leading, spacing: 1) {
                Text(peer.name).font(.system(size: 13, weight: .medium)).lineLimit(1)
                Text(LinkKind(peer).label + (model.isPaired(peer) ? " · Paired" : " · New"))
                    .font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer(minLength: 6)
            HStack(spacing: 6) {
                Button("Extend") {
                    model.extendMode = true
                    model.connect(to: peer)
                }
                .buttonStyle(PillStyle(prominent: true))
                .disabled(!peer.canExtend)
                .help("This Mac becomes a second screen for it")
                Button("View") {
                    model.extendMode = false
                    model.connect(to: peer)
                }
                .buttonStyle(PillStyle(prominent: false))
                .help("Show its screen here, in a window")
            }
            .disabled(model.isViewing)
            .opacity(hovered ? 1 : 0.85)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous)
            .fill(Color.white.opacity(hovered ? 0.06 : 0)))
        .onHover { hovered = $0 }
    }
}

private struct AddressField: View {
    @ObservedObject var model: AppModel

    private var empty: Bool { model.manualAddress.trimmingCharacters(in: .whitespaces).isEmpty }

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "network").font(.system(size: 11)).foregroundStyle(.secondary)
            TextField("Connect by address (IP or host:port)", text: $model.manualAddress)
                .textFieldStyle(.plain)
                .font(.system(size: 12))
                .onSubmit { model.connectManual() }
            Button { model.connectManual() } label: {
                Image(systemName: "arrow.right").font(.system(size: 10, weight: .bold))
                    .frame(width: 20, height: 20)
                    .background(Circle().fill(Color.white.opacity(empty ? 0.08 : 0.92)))
                    .foregroundStyle(empty ? Color.secondary : Color.black)
            }
            .buttonStyle(.plain)
            .disabled(empty || model.isViewing)
            .help("Connect")
        }
        .padding(.leading, 10)
        .padding(.trailing, 5)
        .frame(height: 30)
        .background(Capsule().fill(Color.white.opacity(0.05)))
        .overlay(Capsule().strokeBorder(Color.white.opacity(0.08)))
    }
}

// MARK: Building blocks

/// Small capsule buttons: white for the main action, faint for the other.
private struct PillStyle: ButtonStyle {
    let prominent: Bool
    @Environment(\.isEnabled) private var enabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 11.5, weight: .semibold))
            .padding(.horizontal, 10)
            .frame(height: 24)
            .foregroundStyle(prominent ? Color.black : Color.primary)
            .background(Capsule().fill(prominent ? Color.white.opacity(0.92) : Color.white.opacity(0.1)))
            .opacity(enabled ? (configuration.isPressed ? 0.7 : 1) : 0.4)
            .contentShape(Capsule())
    }
}

private struct CircleButton: View {
    let symbol: String
    let help: String
    let action: () -> Void
    @State private var hovered = false

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 12.5, weight: .medium))
                .frame(width: 28, height: 28)
                .background(Circle().fill(Color.white.opacity(hovered ? 0.12 : 0.06)))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(.secondary)
        .onHover { hovered = $0 }
        .help(help)
        .accessibilityLabel(help)
    }
}

struct Notice: View {
    let text: String
    let systemImage: String

    var body: some View {
        Label {
            Text(text).font(.system(size: 11.5)).fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: systemImage).foregroundStyle(.primary)
        }
        .padding(9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Color.white.opacity(0.08)))
    }
}
