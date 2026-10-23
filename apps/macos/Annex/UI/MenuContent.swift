import SwiftUI

/// The menu bar window: share this Mac, or connect to another computer.
struct MenuContent: View {
    @ObservedObject var model: AppModel
    @Environment(\.openWindow) private var openWindow
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if let err = model.startupError {
                Notice(text: err, systemImage: "exclamationmark.triangle.fill", tint: .red)
            }
            ShareCard(model: model)
            ConnectCard(model: model)
            footer
        }
        .padding(14)
        .frame(width: 360)
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .frame(width: 38, height: 38)
            VStack(alignment: .leading, spacing: 1) {
                Text(model.deviceName.isEmpty ? "Annex" : model.deviceName)
                    .font(.headline)
                    .lineLimit(1)
                if model.settings.showTechnicalDetails {
                    Text(model.fingerprintDisplay)
                        .font(.system(size: 10.5, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                        .help("This Mac's identity code. Compare it with what the other device shows when pairing.")
                } else {
                    Text(model.isHosting ? "Sharing" : "This Mac").font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer()
            Button {
                model.pane = .settings
                openWindow(id: "main")
                // A menu bar app isn't frontmost; bring the settings window forward.
                NSApp.activate(ignoringOtherApps: true)
            } label: {
                Image(systemName: "gearshape")
                    .font(.system(size: 14, weight: .medium))
                    .frame(width: 28, height: 28)
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .help("Settings")
            .keyboardShortcut(",")
        }
    }

    private var footer: some View {
        HStack {
            Text("Annex \(Annex.version)")
                .font(.caption)
                .foregroundStyle(.tertiary)
            Spacer()
            Button("Quit") { model.quit() }
                .buttonStyle(.plain)
                .font(.caption)
                .foregroundStyle(.secondary)
                .keyboardShortcut("q")
        }
        .padding(.horizontal, 2)
    }
}

// MARK: Share

private struct ShareCard: View {
    @ObservedObject var model: AppModel

    var body: some View {
        Card {
            HStack(spacing: 10) {
                IconBadge(systemImage: "display.2", tint: model.isHosting ? .accentColor : .secondary)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Share this Mac").font(.system(size: 13, weight: .semibold))
                    Text(summary).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                }
                Spacer()
                if model.isHosting {
                    Button("Stop", role: .destructive) { model.setHosting(false) }
                        .controlSize(.small)
                } else {
                    Button("Start Sharing") { model.setHosting(true) }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                }
            }
            if model.displays.count > 1 {
                Picker("Display", selection: $model.selectedDisplayID) {
                    ForEach(model.displays) { d in Text(d.name).tag(Optional(d.id)) }
                }
                .controlSize(.small)
                .disabled(model.isHosting)
            }
            if model.isHosting {
                let s = model.hostStatus
                if !s.mirrorViewers.isEmpty, let st = s.stream {
                    StatusRow(systemImage: "rectangle.on.rectangle", tint: .green,
                              text: "Mirroring to \(s.mirrorViewers.joined(separator: ", "))",
                              detail: model.settings.showTechnicalDetails
                                ? "\(st.width)×\(st.height) · \(st.fps) fps · \(s.hardwareEncoder ? "hardware" : "software") encoder"
                                : nil)
                }
                if !s.extendViewers.isEmpty {
                    StatusRow(systemImage: "rectangle.split.2x1", tint: .green,
                              text: "Extended to \(s.extendViewers.joined(separator: ", "))",
                              detail: "Arrange it in System Settings → Displays")
                }
                if !s.controllingViewers.isEmpty {
                    StatusRow(systemImage: "keyboard", tint: .orange,
                              text: "\(s.controllingViewers.joined(separator: ", ")) can use your mouse and keyboard",
                              detail: nil)
                }
            }
            if let err = model.hostStatus.error {
                Notice(text: err, systemImage: "exclamationmark.triangle.fill", tint: .orange)
            }
        }
    }

    private var summary: String {
        guard model.isHosting else { return "Not visible to other computers." }
        let s = model.hostStatus
        if s.viewerName != nil { return "Connected." }
        if s.listeningPort != nil { return "Waiting for a connection." }
        return "Starting…"
    }
}

// MARK: Connect

private struct ConnectCard: View {
    @ObservedObject var model: AppModel
    @State private var hovered: String?

    var body: some View {
        Card {
            HStack(spacing: 10) {
                IconBadge(systemImage: "macbook.and.iphone", tint: model.isViewing ? .accentColor : .secondary)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Connect to a computer").font(.system(size: 13, weight: .semibold))
                    Text("Show its screen here, or be its extra display")
                        .font(.caption).foregroundStyle(.secondary).lineLimit(2)
                }
            }

            VStack(spacing: 2) {
                if model.discovered.isEmpty {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.mini)
                        Text("Looking for computers…")
                            .font(.caption).foregroundStyle(.secondary)
                        Spacer()
                    }
                    .padding(.vertical, 6)
                    .padding(.horizontal, 6)
                }
                ForEach(model.discovered) { peer in
                    HStack(spacing: 8) {
                        Image(systemName: "desktopcomputer")
                            .foregroundStyle(Color.accentColor)
                            .frame(width: 18)
                        Text(peer.isSharing ? peer.name : "\(peer.name) (not sharing)")
                            .lineLimit(1)
                            .foregroundStyle(peer.isSharing ? .primary : .secondary)
                        Spacer()
                        Button {
                            model.extendMode = false
                            model.connect(to: peer)
                        } label: { Image(systemName: "rectangle.on.rectangle") }
                        .help("Show its screen here")
                        Button {
                            model.extendMode = true
                            model.connect(to: peer)
                        } label: { Image(systemName: "rectangle.split.2x1") }
                        .help("Be its extra display")
                        .disabled(!peer.canExtend)
                    }
                    .font(.system(size: 13))
                    .controlSize(.small)
                    .disabled(model.isViewing || !peer.isSharing)
                    .padding(.vertical, 4)
                    .padding(.horizontal, 8)
                    .background(RoundedRectangle(cornerRadius: 7)
                        .fill(hovered == peer.id ? Color.primary.opacity(0.06) : .clear))
                    .onHover { hovered = $0 ? peer.id : nil }
                }
            }

            HStack(spacing: 6) {
                TextField("IP address or host:port", text: $model.manualAddress)
                    .textFieldStyle(.roundedBorder)
                    .controlSize(.small)
                    .onSubmit { model.connectManual() }
                Button {
                    model.connectManual()
                } label: {
                    Image(systemName: "arrow.right")
                }
                .controlSize(.small)
                .disabled(model.manualAddress.trimmingCharacters(in: .whitespaces).isEmpty)
                .help("Connect")
            }

            if let s = model.viewerStatus {
                StatusRow(systemImage: model.isViewing ? "play.rectangle.fill" : "info.circle",
                          tint: model.isViewing ? .green : .secondary, text: s,
                          detail: model.settings.showTechnicalDetails
                            ? model.latency.map {
                                String(format: "Latency %.0f ms (p95 %.0f) · %.0f fps", $0.p50Ms, $0.p95Ms, $0.fps)
                            }
                            : nil)
                if model.isViewing {
                    HStack {
                        if model.controllingHost {
                            Label("You're in control · ⌃⌥⇧Q to leave", systemImage: "keyboard")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Disconnect") { model.disconnectViewer() }
                            .controlSize(.small)
                    }
                }
            }
        }
    }
}

// MARK: Building blocks

struct Card<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) { content }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Color.primary.opacity(0.045)))
            .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Color.primary.opacity(0.07)))
    }
}

struct IconBadge: View {
    let systemImage: String
    let tint: Color

    var body: some View {
        Image(systemName: systemImage)
            .font(.system(size: 14, weight: .semibold))
            .foregroundStyle(tint == .secondary ? Color.secondary : Color.white)
            .frame(width: 30, height: 30)
            .background(RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(tint == .secondary ? AnyShapeStyle(Color.primary.opacity(0.08)) : AnyShapeStyle(tint.gradient)))
            .animation(.easeInOut(duration: 0.2), value: systemImage + "\(tint)")
    }
}

struct StatusRow: View {
    let systemImage: String
    let tint: Color
    let text: String
    let detail: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: systemImage)
                .foregroundStyle(tint)
                .frame(width: 16)
            VStack(alignment: .leading, spacing: 1) {
                Text(text).font(.callout).fixedSize(horizontal: false, vertical: true)
                if let detail {
                    Text(detail).font(.caption).foregroundStyle(.secondary).monospacedDigit()
                }
            }
        }
    }
}

struct Notice: View {
    let text: String
    let systemImage: String
    let tint: Color

    var body: some View {
        Label {
            Text(text).font(.caption).fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: systemImage).foregroundStyle(tint)
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(tint.opacity(0.12)))
    }
}
