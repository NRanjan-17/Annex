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
            ThisMacCard(model: model)
                .listRowInsets(EdgeInsets(top: 6, leading: 6, bottom: 10, trailing: 6))
                .contentShape(Rectangle())
                .onTapGesture { model.pane = .general }
                .help("Rename this Mac in General")

            Section {
                item("Share This Mac", "display.2", .share, badge: model.isHosting ? "On" : nil)
                item("Connect", "rectangle.connected.to.line.below", .connect, badge: model.isViewing ? "Live" : nil)
            }

            Section("Nearby") {
                if model.discovered.isEmpty {
                    HStack(spacing: 10) {
                        ProgressView().controlSize(.small).frame(width: 22)
                        Text("Searching…").foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 8)
                }
                ForEach(model.discovered) { peer in
                    item(peer.name, "desktopcomputer", .peer(peer.id), badge: nil)
                }
            }

            Section("Annex") {
                item("Paired Devices", "lock.shield", .devices, badge: model.trusted.isEmpty ? nil : "\(model.trusted.count)")
                item("General", "gearshape", .general, badge: nil)
                item("Viewing", "play.rectangle", .viewing, badge: nil)
                item("About", "info.circle", .about, badge: nil)
            }
        }
        .listStyle(.sidebar)
    }

    private func item(_ title: String, _ symbol: String, _ pane: Pane, badge: String?) -> some View {
        SidebarItem(title: title, symbol: symbol, badge: badge, selected: model.pane == pane) {
            model.pane = pane
        }
        .listRowInsets(EdgeInsets(top: 1, leading: 0, bottom: 1, trailing: 0))
    }
}

/// The Annex purple, from the app icon.
extension Color {
    static let annex = Color(red: 0.42, green: 0.36, blue: 0.95)
}

private struct ThisMacCard: View {
    @ObservedObject var model: AppModel

    var body: some View {
        HStack(spacing: 10) {
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .frame(width: 40, height: 40)
            VStack(alignment: .leading, spacing: 1) {
                Text(model.deviceName.isEmpty ? "This Mac" : model.deviceName)
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(1)
                Text(model.isHosting ? "Sharing" : "This Mac")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
        }
    }
}

private struct SidebarItem: View {
    let title: String
    let symbol: String
    let badge: String?
    let selected: Bool
    let action: () -> Void
    @State private var hovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                // A fixed box keeps every symbol centred on the same column, whatever its shape.
                Image(systemName: symbol)
                    .font(.system(size: 15, weight: .regular))
                    .foregroundStyle(Color.annex)
                    .frame(width: 22, height: 22)
                Text(title)
                    .font(.system(size: 13, weight: selected ? .semibold : .regular))
                    .lineLimit(1)
                Spacer(minLength: 4)
                if let badge {
                    Text(badge)
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(Color.primary.opacity(selected ? 0.11 : hovered ? 0.05 : 0)))
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
            VStack(spacing: 8) {
                icon
                Text(title).font(.system(size: 22, weight: .bold))
                Text(text)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 460)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 14)
        }
    }
}

private extension Hero where Icon == IconTile {
    init(title: String, text: String, symbol: String, tint: Color) {
        self.init(title: title, text: text) { IconTile(symbol: symbol, tint: tint, size: 64) }
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
            Spacer(minLength: 12)
            // Controls keep their full size; the text on the left wraps instead (no "C…" buttons).
            trailing
                .fixedSize()
                .layoutPriority(1)
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
        Page(title: "Share This Mac") {
            Hero(title: "Share This Mac",
                 text: "Start sharing to let another computer show this Mac's screen, or become an extra display for this Mac.",
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
            if let err = model.hostStatus.error {
                Section {
                    Label(err, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                }
            }
            if model.isHosting && model.hostStatus.viewerName != nil {
                let s = model.hostStatus
                Section("Connected now") {
                    ForEach(s.mirrorViewers, id: \.self) { name in
                        Row(symbol: "rectangle.on.rectangle", tint: .green, title: "\(name) is showing this screen",
                            subtitle: s.stream.map { "\($0.width)×\($0.height) at \($0.fps) fps" }) { EmptyView() }
                    }
                    ForEach(s.extendViewers, id: \.self) { name in
                        Row(symbol: "rectangle.split.2x1", tint: .green, title: "\(name) is an extra display for this Mac",
                            subtitle: "Arrange it in System Settings → Displays") { EmptyView() }
                    }
                    ForEach(s.controllingViewers, id: \.self) { name in
                        Row(symbol: "keyboard", tint: .orange, title: "\(name) is using your mouse and keyboard",
                            subtitle: "Turn it off in Paired Devices") { EmptyView() }
                    }
                }
            }
            if model.isHosting, let port = model.hostStatus.listeningPort {
                HowToConnect(model: model, port: port)
            }
            Section {
                Row(symbol: "cpu", tint: .gray, title: "Hardware video encoder",
                    subtitle: "Recommended. The software encoder adds about a quarter of a second of delay.") {
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
        if s.viewerName != nil { return "Connected · \(s.hardwareEncoder ? "hardware" : "software") encoder" }
        if s.listeningPort != nil { return "Waiting for another computer to connect" }
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
            Row(symbol: "1.circle.fill", tint: .blue, title: "Open Annex on the other computer",
                subtitle: "Go to Connect. “\(model.deviceName)” is listed under Nearby.") { EmptyView() }
            Row(symbol: "2.circle.fill", tint: .blue, title: "Choose what you want",
                subtitle: "“Show its screen here” puts this Mac's screen on that computer. “Be its extra display” turns that computer into a second screen for this Mac.") { EmptyView() }
            Row(symbol: "3.circle.fill", tint: .blue, title: "First time only: enter the PIN",
                subtitle: "This Mac shows a 6-digit PIN to type there. The identity codes should match: \(model.fingerprintDisplay)") { EmptyView() }
            if !addresses.isEmpty {
                Row(symbol: "questionmark.circle.fill", tint: .gray, title: "Not listed there?",
                    subtitle: "Use Connect by address on the other computer with one of these:") { EmptyView() }
                ForEach(addresses, id: \.self) { a in
                    let text = port == Int(annexDefaultPort) ? a.ip : "\(a.ip):\(port)"
                    HStack(spacing: 10) {
                        Text(text)
                            .font(.system(size: 15, weight: .medium, design: .monospaced))
                            .textSelection(.enabled)
                        Text(a.isDirectLink ? "Direct cable" : "Network · \(a.interface)")
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
                    .padding(.leading, 36)
                }
            } else {
                Row(symbol: "wifi.exclamationmark", tint: .orange, title: "This Mac isn't on a network",
                    subtitle: "Connect it to Wi-Fi, Ethernet or a cable to the other computer.") { EmptyView() }
            }
        } header: {
            Text("How to connect from the other computer")
        }
        .onAppear { addresses = LocalAddresses.current() }
    }
}

/// The port hosts use unless it's taken.
private let annexDefaultPort: UInt16 = 47800

/// Shown while nothing that's sharing has been found: what usually blocks discovery.
private struct NotShowingUp: View {
    var body: some View {
        Section("Not showing up?") {
            Row(symbol: "wifi", tint: .gray, title: "Both computers on the same network",
                subtitle: "Same Wi-Fi or router, or a cable between them. Guest Wi-Fi and VPNs often block computers from seeing each other.") { EmptyView() }
            Row(symbol: "network", tint: .gray, title: "Allow Local Network access",
                subtitle: "Annex needs it to find other computers. Make sure Annex is on in Privacy & Security → Local Network.") {
                Button("Open Settings") {
                    if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork") {
                        NSWorkspace.shared.open(url)
                    }
                }
            }
            Row(symbol: "shield", tint: .gray, title: "On a Windows PC: allow Annex through the firewall",
                subtitle: "In Annex on that PC, open Connect and click Allow under Not showing up.") { EmptyView() }
            Row(symbol: "keyboard", tint: .gray, title: "Still nothing?",
                subtitle: "Use Connect by address below with an address the other computer shows.") { EmptyView() }
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
                Label(compact ? "Show Its Screen" : "Show Its Screen Here", systemImage: "rectangle.on.rectangle")
            }
            .buttonStyle(.borderedProminent)
            .help("See and use the other computer's screen on this Mac")
            Button {
                model.extendMode = true
                connect()
            } label: {
                Label("Be Its Extra Display", systemImage: "rectangle.split.2x1")
            }
            .help(peer?.canExtend == false
                  ? "It can't add an extra display (on Windows it needs the Annex display driver)"
                  : "This Mac becomes a second screen for the other computer")
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
            Hero(title: "Connect to Another Computer",
                 text: "Show another computer's screen on this Mac, or use this Mac as an extra display for it. Turn on sharing in Annex on that computer first.",
                 symbol: "rectangle.connected.to.line.below", tint: .indigo)
            ViewerStatusSection(model: model)
            Section("Nearby") {
                if model.discovered.isEmpty {
                    Row(symbol: "magnifyingglass", tint: .gray, title: "Looking for computers…",
                        subtitle: "Open Annex on the other computer and click Start Sharing. It appears here automatically.") {
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
                Row(symbol: "network", tint: .gray, title: "Address",
                    subtitle: "Shown on the other computer under How to connect.") {
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
                Text("For when the other computer isn't listed under Nearby (guest Wi-Fi, VPNs, other subnets).")
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
        guard peer.isSharing else { return "Annex is open there, but it isn't sharing. Click Start Sharing on it first." }
        return (model.isPaired(peer) ? "Paired" : "Not paired yet · you'll enter a PIN the first time")
            + (peer.canExtend ? "" : " · can't be extended (no display driver)")
    }
}

private struct ViewerStatusSection: View {
    @ObservedObject var model: AppModel

    var body: some View {
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
                    Row(symbol: "keyboard", tint: .orange, title: "Your mouse and keyboard control it",
                        subtitle: "Press ⌃⌥⇧Q in the viewer to leave") { EmptyView() }
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
                    ? "Paired with this Mac. Connects without a PIN."
                    : "On your network. The first time, you'll enter the PIN shown on \(peer.name).",
                 symbol: "desktopcomputer", tint: .teal)
            ViewerStatusSection(model: model)
            if !peer.isSharing {
                Section {
                    Label("Annex is open on \(peer.name), but it isn't sharing. Click Start Sharing there, then come back.",
                          systemImage: "info.circle")
                }
            }
            Section {
                Row(symbol: "rectangle.on.rectangle", tint: .indigo, title: "Show its screen here",
                    subtitle: "See and use \(peer.name)'s screen on this Mac.") {
                    Button("Show Its Screen") {
                        model.extendMode = false
                        model.connect(to: peer)
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(model.isViewing || !peer.isSharing)
                }
                Row(symbol: "rectangle.split.2x1", tint: .blue, title: "Be its extra display",
                    subtitle: "This Mac becomes a second screen for \(peer.name). Drag windows onto it from there.") {
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
                 text: "Devices you've paired with a PIN connect without asking again. Choose which ones may use this Mac's mouse and keyboard.",
                 symbol: "lock.shield.fill", tint: .green)
            if model.trusted.contains(where: \.allowInput) && !model.accessibilityTrusted {
                Section {
                    Row(symbol: "hand.raised.fill", tint: .orange, title: "Annex needs Accessibility access",
                        subtitle: "macOS only lets apps use the mouse and keyboard with this permission.") {
                        Button("Open Settings") { InputInjector.openAccessibilitySettings() }
                    }
                }
            }
            Section {
                if model.trusted.isEmpty {
                    Row(symbol: "lock.shield", tint: .gray, title: "No paired devices yet",
                        subtitle: "Connect from another computer and enter the PIN this Mac shows.") { EmptyView() }
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
            Toggle("Control", isOn: Binding(get: { peer.allowInput }, set: { model.setAllowInput(peer, $0) }))
                .toggleStyle(.switch)
                .controlSize(.small)
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
        if peer.allowInput { parts.append("can control this Mac") }
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
            Hero(title: "General", text: "How Annex starts. It lives in the menu bar and keeps everything on your own network: no accounts, no cloud.",
                 symbol: "gearshape.fill", tint: .gray)
            Section {
                Row(symbol: "laptopcomputer", tint: .blue, title: "Device name",
                    subtitle: "What other computers see when they find this Mac") {
                    // Empty = the Mac's own name (shown as the placeholder).
                    InputField(text: $name, placeholder: AppModel.systemName) {
                        if name.trimmingCharacters(in: .whitespaces) != (settings.customDeviceName ?? "") {
                            model.rename(name)
                        }
                    }
                    .frame(width: 220)
                }
            } footer: {
                Text("Leave it empty to use the name from System Settings → General → About.")
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
            Hero(title: "Viewing", text: "How other computers appear on this Mac.", symbol: "play.rectangle.fill", tint: .purple)
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
                    subtitle: "Use this Mac's mouse and keyboard on it. It must allow this Mac in its Paired Devices.") {
                    Toggle("", isOn: $settings.controlWhenViewing).toggleStyle(.switch).labelsHidden()
                }
            } footer: {
                Text("While in control, press ⌃⌥⇧Q to leave the session.").font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

private struct AboutPage: View {
    var body: some View {
        Page(title: "About Annex") {
            Hero(title: "Annex", text: "Version \(Annex.version)\nUse another computer as a second display, or mirror its screen. Encrypted end to end.") {
                Image(nsImage: NSApp.applicationIconImage).resizable().frame(width: 88, height: 88)
            }
            Section {
                Link(destination: URL(string: "https://github.com/NRanjan-17/Annex")!) {
                    Row(symbol: "globe", tint: .blue, title: "Website and source code") { Image(systemName: "arrow.up.right").foregroundStyle(.tertiary) }
                }
                Link(destination: URL(string: "https://github.com/NRanjan-17/Annex/issues")!) {
                    Row(symbol: "ladybug.fill", tint: .red, title: "Report an issue") { Image(systemName: "arrow.up.right").foregroundStyle(.tertiary) }
                }
            } footer: {
                Text("MIT License · Copyright © 2026 Nalinish Ranjan").font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}
