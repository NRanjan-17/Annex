import SwiftUI

/// Sidebar pages of the main window.
enum Pane: Hashable {
    case share, connect, peer(String), devices, general, viewing, about
}

/// The main window: a System Settings-style sidebar and pages.
struct MainView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        // The sidebar is the app's navigation; it always stays visible.
        NavigationSplitView(columnVisibility: .constant(.all)) {
            Sidebar(model: model)
                .navigationSplitViewColumnWidth(min: 210, ideal: 230, max: 280)
                .toolbar(removing: .sidebarToggle)
        } detail: {
            detail
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .frame(minWidth: 780, minHeight: 540)
        .onAppear {
            model.refreshTrusted()
            model.refreshPermissions()
            model.refreshDisplays()
        }
    }

    @ViewBuilder private var detail: some View {
        switch model.pane {
        case .share: SharePage(model: model, settings: model.settings)
        case .connect: ConnectPage(model: model)
        case .peer(let id):
            if let peer = model.discovered.first(where: { $0.id == id }) {
                PeerPage(model: model, peer: peer)
            } else {
                ConnectPage(model: model)
            }
        case .devices: DevicesPage(model: model)
        case .general: GeneralPage(model: model, settings: model.settings)
        case .viewing: ViewingPage(settings: model.settings)
        case .about: AboutPage()
        }
    }
}

// MARK: Sidebar

private struct Sidebar: View {
    @ObservedObject var model: AppModel

    var body: some View {
        // Rows draw their own selection (a soft grey, like Music) instead of the List's accent fill.
        List {
            Section {
                item("Share This Mac", "display.2", .share, active: model.isHosting)
                item("Connect", "rectangle.connected.to.line.below", .connect, active: model.isViewing)
            }

            Section("Nearby") {
                if model.discovered.isEmpty {
                    HStack(spacing: 10) {
                        ProgressView().controlSize(.mini).frame(width: 20)
                        Text("Searching…").font(.system(size: 13)).foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 5)
                }
                ForEach(model.discovered) { peer in
                    item(peer.name, "desktopcomputer", .peer(peer.id), active: false, dimmed: !peer.isSharing)
                }
            }

            Section("Settings") {
                item("Paired Devices", "lock.shield", .devices, active: false)
                item("General", "gearshape", .general, active: false)
                item("Viewing", "play.rectangle", .viewing, active: false)
                item("About", "info.circle", .about, active: false)
            }
        }
        .listStyle(.sidebar)
        // This Mac, at the bottom like the account row in Music.
        .safeAreaInset(edge: .bottom) {
            ThisMacFooter(model: model)
        }
    }

    private func item(_ title: String, _ symbol: String, _ pane: Pane, active: Bool, dimmed: Bool = false) -> some View {
        SidebarItem(title: title, symbol: symbol, active: active, dimmed: dimmed, selected: model.pane == pane) {
            model.pane = pane
        }
        .listRowInsets(EdgeInsets(top: 1, leading: 0, bottom: 1, trailing: 0))
    }
}

/// The Annex purple, from the app icon.
extension Color {
    static let annex = Color(red: 0.42, green: 0.36, blue: 0.95)
}

private struct ThisMacFooter: View {
    @ObservedObject var model: AppModel

    var body: some View {
        Button { model.pane = .general } label: {
            HStack(spacing: 10) {
                Image(nsImage: NSApp.applicationIconImage)
                    .resizable()
                    .frame(width: 28, height: 28)
                VStack(alignment: .leading, spacing: 0) {
                    Text(model.deviceName.isEmpty ? "This Mac" : model.deviceName)
                        .font(.system(size: 13, weight: .medium))
                        .lineLimit(1)
                    Text(model.isHosting ? "Sharing" : "This Mac")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 12)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Rename this Mac")
    }
}

private struct SidebarItem: View {
    let title: String
    let symbol: String
    let active: Bool
    let dimmed: Bool
    let selected: Bool
    let action: () -> Void
    @State private var hovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 9) {
                // A fixed box keeps every symbol centred on the same column, whatever its shape.
                Image(systemName: symbol)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(dimmed ? Color.secondary : Color.annex)
                    .frame(width: 20, height: 20)
                Text(title)
                    .font(.system(size: 13, weight: selected ? .semibold : .regular))
                    .foregroundStyle(dimmed ? .secondary : .primary)
                    .lineLimit(1)
                Spacer(minLength: 4)
                if active {
                    Circle().fill(Color.green).frame(width: 7, height: 7)
                }
            }
            .padding(.horizontal, 8)
            .frame(height: 30)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(Color.primary.opacity(selected ? 0.10 : hovered ? 0.05 : 0)))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovered = $0 }
    }
}

// MARK: Building blocks

/// A rounded, gradient-filled icon like the ones in System Settings.
struct IconTile: View {
    let symbol: String
    let tint: Color
    var size: CGFloat = 26

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: size * 0.5, weight: .semibold))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(RoundedRectangle(cornerRadius: size * 0.27, style: .continuous).fill(tint.gradient))
    }
}

/// The card at the top of each page: big icon, title, one-line description.
private struct Hero<Icon: View>: View {
    let title: String
    let text: String
    @ViewBuilder var icon: Icon

    var body: some View {
        Section {
            VStack(spacing: 6) {
                icon
                Text(title).font(.system(size: 19, weight: .semibold))
                Text(text)
                    .font(.system(size: 12.5))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 420)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 10)
        }
    }
}

private extension Hero where Icon == IconTile {
    init(title: String, text: String, symbol: String, tint: Color) {
        self.init(title: title, text: text) { IconTile(symbol: symbol, tint: tint, size: 48) }
    }
}

/// A row with an icon tile, a title, an optional subtitle and trailing content.
private struct Row<Trailing: View>: View {
    let symbol: String
    let tint: Color
    let title: String
    var subtitle: String?
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(spacing: 10) {
            IconTile(symbol: symbol, tint: tint)
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                if let subtitle {
                    Text(subtitle).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
            }
            // The text gets the room; controls take only their natural size (never truncated).
            .layoutPriority(1)
            Spacer(minLength: 12)
            trailing
                .fixedSize()
        }
        .padding(.vertical, 2)
    }
}

private struct Page<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content

    var body: some View {
        Form { content }
            .formStyle(.grouped)
            .navigationTitle(title)
    }
}

// MARK: Share

private struct SharePage: View {
    @ObservedObject var model: AppModel
    @ObservedObject var settings: AppSettings

    var body: some View {
        sharePage
            // Permission changes in System Settings don't notify apps; look again while this page is open.
            .task {
                while !Task.isCancelled {
                    model.refreshPermissions()
                    try? await Task.sleep(for: .seconds(2))
                }
            }
    }

    private var sharePage: some View {
        Page(title: "Share This Mac") {
            Hero(title: "Share This Mac",
                 text: "Let another computer show this screen or use it as an extra display.",
                 symbol: "display.2", tint: .blue)
            Section {
                HStack(spacing: 12) {
                    IconTile(symbol: model.isHosting ? "dot.radiowaves.left.and.right" : "pause.fill",
                             tint: model.isHosting ? .blue : .gray, size: 34)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(model.isHosting ? "Sharing is on" : "Sharing is off")
                            .font(.system(size: 15, weight: .semibold))
                        Text(shareStatus).font(.callout).foregroundStyle(.secondary)
                    }
                    Spacer()
                    if model.isHosting {
                        Button(role: .destructive) { model.setHosting(false) } label: {
                            Label("Stop Sharing", systemImage: "stop.fill").padding(.horizontal, 6)
                        }
                        .controlSize(.large)
                    } else {
                        Button { model.setHosting(true) } label: {
                            Label("Start Sharing", systemImage: "play.fill").padding(.horizontal, 6)
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.large)
                    }
                }
                .padding(.vertical, 4)
                if model.displays.count > 1 {
                    Row(symbol: "display", tint: .blue, title: "Screen to share",
                        subtitle: model.isHosting ? "Stop sharing to pick another screen." : nil) {
                        Picker("", selection: $model.selectedDisplayID) {
                            ForEach(model.displays) { d in Text(d.name).tag(Optional(d.id)) }
                        }
                        .labelsHidden()
                        .fixedSize()
                        .disabled(model.isHosting)
                    }
                }
            }
            if model.isHosting && !model.screenRecordingAllowed {
                Section {
                    Row(symbol: "record.circle", tint: .orange, title: "Screen Recording is off",
                        subtitle: "Allow Annex in Privacy & Security, then reopen Annex. After an update, remove it with − and add it again.") {
                        Button("Open Settings") {
                            if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") {
                                NSWorkspace.shared.open(url)
                            }
                        }
                    }
                }
            }
            if let err = model.hostStatus.error {
                Section {
                    Label(err, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                }
            }
            if model.isHosting && model.hostStatus.viewerName != nil {
                let s = model.hostStatus
                Section("Connected now") {
                    ForEach(s.mirrorViewers, id: \.self) { name in
                        Row(symbol: "rectangle.on.rectangle", tint: .green, title: "Showing this screen on \(name)",
                            subtitle: s.stream.map { "\($0.width)×\($0.height) at \($0.fps) fps" }) { EmptyView() }
                    }
                    ForEach(s.extendViewers, id: \.self) { name in
                        Row(symbol: "rectangle.split.2x1", tint: .green, title: "\(name) is an extra display",
                            subtitle: "Arrange it in System Settings → Displays.") { EmptyView() }
                    }
                    ForEach(s.controllingViewers, id: \.self) { name in
                        Row(symbol: "keyboard", tint: .orange, title: "\(name) is controlling this Mac",
                            subtitle: "Turn it off in Paired Devices.") { EmptyView() }
                    }
                }
            }
            if model.isHosting, let port = model.hostStatus.listeningPort {
                HowToConnect(model: model, port: port)
            }
            Section {
                Row(symbol: "cpu", tint: .gray, title: "Hardware video encoder",
                    subtitle: "Recommended. Lowest delay.") {
                    Toggle("", isOn: $settings.preferHardwareEncoder).toggleStyle(.switch).labelsHidden()
                        .disabled(model.isHosting)
                }
            } header: {
                Text("Video")
            }
        }
    }

    private var shareStatus: String {
        guard model.isHosting else { return "Other computers can't see this Mac." }
        let s = model.hostStatus
        if s.viewerName != nil { return "Connected." }
        if s.listeningPort != nil { return "Waiting for a connection." }
        return "Starting…"
    }
}

/// Shown while sharing: exactly what to do on the other computer.
private struct HowToConnect: View {
    @ObservedObject var model: AppModel
    let port: Int
    @State private var addresses = LocalAddresses.current()
    @State private var copied: String?

    var body: some View {
        Section {
            Row(symbol: "1.circle.fill", tint: .blue, title: "Open Annex on the other computer and choose “\(model.deviceName)”.") { EmptyView() }
            Row(symbol: "2.circle.fill", tint: .blue, title: "Pick Show Its Screen or Be Its Extra Display.") { EmptyView() }
            Row(symbol: "3.circle.fill", tint: .blue, title: "Enter the PIN shown here (first time only).") { EmptyView() }
            if addresses.isEmpty {
                Row(symbol: "wifi.exclamationmark", tint: .orange, title: "This Mac isn't on a network.",
                    subtitle: "Connect it to Wi-Fi, Ethernet or a cable.") { EmptyView() }
            }
            ForEach(addresses, id: \.self) { a in
                let text = port == Int(annexDefaultPort) ? a.ip : "\(a.ip):\(port)"
                HStack(spacing: 10) {
                    Text(text)
                        .font(.system(size: 14, weight: .medium, design: .monospaced))
                        .textSelection(.enabled)
                    Text(a.isDirectLink ? "Cable" : "Network")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button(copied == text ? "Copied" : "Copy") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(text, forType: .string)
                        copied = text
                    }
                    .controlSize(.small)
                }
            }
        } header: {
            Text("Connect from another computer")
        } footer: {
            Text(addresses.isEmpty ? "" : "Not listed there? Connect by address. Identity code: \(model.fingerprintDisplay)")
                .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
        }
        .onAppear { addresses = LocalAddresses.current() }
    }
}

/// The port hosts use unless it's taken.
private let annexDefaultPort: UInt16 = 47800

/// Shown while nothing that's sharing has been found: what usually blocks discovery.
private struct NotShowingUp: View {
    var body: some View {
        Section("Can't find it?") {
            Row(symbol: "wifi", tint: .gray, title: "Use the same network",
                subtitle: "Guest Wi-Fi and VPNs can hide computers.") { EmptyView() }
            Row(symbol: "network", tint: .gray, title: "Allow Local Network for Annex") {
                Button("Open Settings") {
                    if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork") {
                        NSWorkspace.shared.open(url)
                    }
                }
            }
            Row(symbol: "shield", tint: .gray, title: "On Windows, allow the firewall",
                subtitle: "In Annex on the PC: Connect → Allow.") { EmptyView() }
        }
    }
}

// MARK: Connect

/// The two things you can do with another computer, as buttons that say what happens.
private struct ConnectButtons: View {
    @ObservedObject var model: AppModel
    let connect: () -> Void
    var compact = false
    var peer: PeerInfo?

    var body: some View {
        HStack(spacing: 8) {
            Button {
                model.extendMode = false
                connect()
            } label: {
                Label("Show Its Screen", systemImage: "rectangle.on.rectangle")
            }
            .buttonStyle(.borderedProminent)
            .help("Use the other computer's screen on this Mac.")
            Button {
                model.extendMode = true
                connect()
            } label: {
                Label("Be Its Extra Display", systemImage: "rectangle.split.2x1")
            }
            .help(peer?.canExtend == false
                  ? "Extend isn't set up on that computer."
                  : "This Mac becomes a second screen for it.")
            .disabled(peer?.canExtend == false)
        }
        .fixedSize()
        .disabled(model.isViewing || peer?.isSharing == false)
    }
}

private struct ConnectPage: View {
    @ObservedObject var model: AppModel

    var body: some View {
        Page(title: "Connect") {
            Hero(title: "Connect",
                 text: "Show another computer's screen here, or be its extra display.",
                 symbol: "rectangle.connected.to.line.below", tint: .indigo)
            ViewerStatusSection(model: model)
            Section("Nearby") {
                if model.discovered.isEmpty {
                    Row(symbol: "magnifyingglass", tint: .gray, title: "Looking for computers…",
                        subtitle: "Start sharing on the other computer.") {
                        ProgressView().controlSize(.small)
                    }
                }
                ForEach(model.discovered) { peer in
                    PeerRow(model: model, peer: peer)
                }
            }
            if model.discovered.allSatisfy({ !$0.isSharing }) {
                NotShowingUp()
            }
            Section {
                Row(symbol: "network", tint: .gray, title: "Address") {
                    InputField(text: $model.manualAddress, placeholder: "192.168.1.20",
                               onSubmit: { model.extendMode = false; model.connectManual() })
                        .frame(width: 200)
                }
                HStack {
                    Spacer()
                    ConnectButtons(model: model, connect: { model.connectManual() }, compact: true)
                        .disabled(model.isViewing || model.manualAddress.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            } header: {
                Text("Connect by address")
            } footer: {
                Text("Use this if the computer isn't listed.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

private struct PeerRow: View {
    @ObservedObject var model: AppModel
    let peer: PeerInfo

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Row(symbol: "desktopcomputer", tint: peer.isSharing ? .teal : .gray, title: peer.name, subtitle: subtitle) { EmptyView() }
            HStack {
                Spacer()
                ConnectButtons(model: model, connect: { model.connect(to: peer) }, compact: true, peer: peer)
            }
        }
    }

    private var subtitle: String {
        guard peer.isSharing else { return "Not sharing" }
        return (model.isPaired(peer) ? "Paired" : "New · PIN needed")
            + (peer.canExtend ? "" : " · Extend unavailable")
    }
}

private struct ViewerStatusSection: View {
    @ObservedObject var model: AppModel

    var body: some View {
        if let direct = model.directStatus {
            Section {
                Row(symbol: "wifi", tint: .blue, title: "Direct Wi-Fi", subtitle: direct) { EmptyView() }
            }
        }
        if let status = model.viewerStatus {
            Section {
                Row(symbol: model.isViewing ? "play.rectangle.fill" : "info.circle", tint: model.isViewing ? .green : .gray,
                    title: status,
                    subtitle: model.latency.map { String(format: "Latency %.0f ms (p95 %.0f ms) · %.0f fps", $0.p50Ms, $0.p95Ms, $0.fps) }) {
                    if model.isViewing {
                        Button(role: .destructive) { model.disconnectViewer() } label: {
                            Label("Disconnect", systemImage: "xmark")
                        }
                    }
                }
                if model.isViewing && model.controllingHost {
                    Row(symbol: "keyboard", tint: .orange, title: "You're in control",
                        subtitle: "Press ⌃⌥⇧Q to leave.") { EmptyView() }
                }
                if model.isViewing && !model.onDirectWiFi {
                    Row(symbol: "wifi", tint: .blue, title: "Switch to direct Wi-Fi",
                        subtitle: "Less lag: connect straight to the PC's hotspot.") {
                        Button("Switch") { model.switchToDirectWiFi() }
                    }
                }
            }
        }
    }
}

private struct PeerPage: View {
    @ObservedObject var model: AppModel
    let peer: PeerInfo

    var body: some View {
        Page(title: peer.name) {
            Hero(title: peer.name,
                 text: model.isPaired(peer)
                    ? "Paired. Connects without a PIN."
                    : "The first connection needs a PIN.",
                 symbol: "desktopcomputer", tint: .teal)
            ViewerStatusSection(model: model)
            if !peer.isSharing {
                Section {
                    Label("Not sharing. Start sharing on \(peer.name) first.",
                          systemImage: "info.circle")
                }
            }
            Section {
                Row(symbol: "rectangle.on.rectangle", tint: .indigo, title: "Show its screen here",
                    subtitle: "Use \(peer.name)'s screen on this Mac.") {
                    Button("Show Its Screen") {
                        model.extendMode = false
                        model.connect(to: peer)
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(model.isViewing || !peer.isSharing)
                }
                Row(symbol: "rectangle.split.2x1", tint: .blue, title: "Be its extra display",
                    subtitle: "This Mac becomes a second screen for \(peer.name).") {
                    Button("Be Its Extra Display") {
                        model.extendMode = true
                        model.connect(to: peer)
                    }
                    .disabled(model.isViewing || !peer.isSharing || !peer.canExtend)
                }
            }
            Section("Details") {
                LabeledContent("Addresses", value: peer.addresses.joined(separator: ", "))
                LabeledContent("Port", value: "\(peer.port)")
                if let fp = peer.fingerprint {
                    LabeledContent("Identity code") {
                        Text(Annex.displayFingerprint(fp)).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                    }
                }
            }
        }
    }
}

// MARK: Devices

private struct DevicesPage: View {
    @ObservedObject var model: AppModel

    var body: some View {
        Page(title: "Paired Devices") {
            Hero(title: "Paired Devices",
                 text: "Paired devices connect without a PIN.",
                 symbol: "lock.shield.fill", tint: .green)
            if model.trusted.contains(where: \.allowInput) && !model.accessibilityTrusted {
                Section {
                    Row(symbol: "hand.raised.fill", tint: .orange, title: "Accessibility is off",
                        subtitle: "Needed for remote control. After an update, remove Annex with − and add it again.") {
                        Button("Open Settings") { InputInjector.openAccessibilitySettings() }
                    }
                }
            }
            Section {
                if model.trusted.isEmpty {
                    Row(symbol: "lock.shield", tint: .gray, title: "No paired devices",
                        subtitle: "Pair by connecting from another computer.") { EmptyView() }
                }
                ForEach(model.trusted) { peer in DeviceRow(model: model, peer: peer) }
            } footer: {
                Text("This Mac's identity code: \(model.fingerprintDisplay)")
                    .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
        }
        // Permission changes in System Settings don't notify apps; look again while this page is open.
        .task {
            while !Task.isCancelled {
                model.refreshPermissions()
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }
}

private struct DeviceRow: View {
    @ObservedObject var model: AppModel
    let peer: TrustedPeer
    @State private var confirmRemove = false

    var body: some View {
        Row(symbol: "desktopcomputer", tint: .teal, title: peer.name, subtitle: lastSeen) {
            HStack(spacing: 8) {
                Text("Can control").font(.callout).foregroundStyle(.secondary)
                Toggle("", isOn: Binding(get: { peer.allowInput }, set: { model.setAllowInput(peer, $0) }))
                    .toggleStyle(.switch)
                    .labelsHidden()
                    .controlSize(.small)
            }
            .help("Let \(peer.name) use this Mac's mouse and keyboard")
            Menu {
                Button("Remove \(peer.name)…", role: .destructive) { confirmRemove = true }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
        }
        .confirmationDialog("Remove \(peer.name)?", isPresented: $confirmRemove) {
            Button("Remove", role: .destructive) { model.revoke(peer) }
        } message: {
            Text("It will need a PIN to connect again.")
        }
    }

    private var lastSeen: String {
        var parts: [String] = []
        if peer.lastSeen > 0 {
            parts.append("Last connected " + Date(timeIntervalSince1970: TimeInterval(peer.lastSeen)).formatted(.relative(presentation: .named)))
        }
        return parts.isEmpty ? "Paired" : parts.joined(separator: " · ")
    }
}

// MARK: Preferences

private struct GeneralPage: View {
    @ObservedObject var model: AppModel
    @ObservedObject var settings: AppSettings
    @State private var name = ""

    var body: some View {
        Page(title: "General") {
            Hero(title: "General", text: "Startup and this Mac's name.",
                 symbol: "gearshape.fill", tint: .gray)
            Section {
                Row(symbol: "laptopcomputer", tint: .blue, title: "Device name",
                    subtitle: "Shown to other computers.") {
                    // Empty = the Mac's own name (shown as the placeholder).
                    InputField(text: $name, placeholder: AppModel.systemName) {
                        if name.trimmingCharacters(in: .whitespaces) != (settings.customDeviceName ?? "") {
                            model.rename(name)
                        }
                    }
                    .frame(width: 220)
                }
            } footer: {
                Text("Leave empty to use the Mac's own name.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            .onAppear { name = settings.customDeviceName ?? "" }
            Section {
                Row(symbol: "power", tint: .blue, title: "Open at login") {
                    Toggle("", isOn: Binding(get: { settings.launchAtLogin }, set: { settings.setLaunchAtLogin($0) }))
                        .toggleStyle(.switch).labelsHidden()
                }
                if let err = settings.launchAtLoginError {
                    Text(err).font(.caption).foregroundStyle(.red)
                }
                Row(symbol: "display.2", tint: .blue, title: "Start sharing when Annex opens") {
                    Toggle("", isOn: $settings.startSharingAtLaunch).toggleStyle(.switch).labelsHidden()
                }
            }
        }
    }
}

private struct ViewingPage: View {
    @ObservedObject var settings: AppSettings

    var body: some View {
        Page(title: "Viewing") {
            Hero(title: "Viewing", text: "How other computers appear here.", symbol: "play.rectangle.fill", tint: .purple)
            Section {
                Row(symbol: "dial.medium", tint: .purple, title: "Quality",
                    subtitle: "Use Fast on Wi-Fi.") {
                    Picker("", selection: $settings.quality) {
                        Text("Best").tag(0)
                        Text("Balanced").tag(1)
                        Text("Fast (Wi-Fi)").tag(2)
                    }
                    .labelsHidden()
                    .fixedSize()
                }
            }
            Section {
                Row(symbol: "arrow.up.left.and.arrow.down.right", tint: .purple, title: "Open in full screen") {
                    Toggle("", isOn: $settings.openFullScreen).toggleStyle(.switch).labelsHidden()
                }
                Row(symbol: "rectangle.split.2x1", tint: .indigo, title: "Use as an extra display by default") {
                    Toggle("", isOn: $settings.extendByDefault).toggleStyle(.switch).labelsHidden()
                }
            }
            Section {
                Row(symbol: "keyboard", tint: .orange, title: "Control the other computer",
                    subtitle: "When it allows it.") {
                    Toggle("", isOn: $settings.controlWhenViewing).toggleStyle(.switch).labelsHidden()
                }
            } footer: {
                Text("Press ⌃⌥⇧Q to leave.").font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

private struct AboutPage: View {
    var body: some View {
        Page(title: "About Annex") {
            Hero(title: "Annex", text: "Version \(Annex.version). Encrypted, local, no accounts.") {
                Image(nsImage: NSApp.applicationIconImage).resizable().frame(width: 88, height: 88)
            }
            Section {
                Link(destination: URL(string: "https://github.com/NRanjan-17/Annex")!) {
                    Row(symbol: "globe", tint: .blue, title: "Website and source code") { Image(systemName: "arrow.up.right").foregroundStyle(.tertiary) }
                }
                Link(destination: URL(string: "https://github.com/NRanjan-17/Annex/issues")!) {
                    Row(symbol: "ladybug.fill", tint: .red, title: "Report an issue") { Image(systemName: "arrow.up.right").foregroundStyle(.tertiary) }
                }
                Row(symbol: "doc.text", tint: .gray, title: "Log file", subtitle: "For bug reports. Stays on this Mac.") {
                    Button("Show in Finder") {
                        let log = AppModel.dataDirectory.appendingPathComponent("annex.log")
                        NSWorkspace.shared.activateFileViewerSelecting([log])
                    }
                }
            } footer: {
                Text("MIT License · Copyright © 2026 Nalinish Ranjan").font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}
