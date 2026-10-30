import SwiftUI

/// Pages of the main window.
enum Pane: Hashable {
    case home, share, connect, peer(String), devices, settings, about
}

/// The main window: a floating glass sidebar and pages, over the living backdrop.
struct MainView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        ZStack {
            Backdrop(live: model.isViewing || model.hostStatus.viewerName != nil)
            HStack(spacing: 0) {
                Sidebar(model: model)
                    .frame(width: 216)
                    .padding(10)
                detail
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .ignoresSafeArea()
        .background(WindowChrome())
        .preferredColorScheme(.dark)
        .tint(Color(white: 0.82))
        .frame(minWidth: 940, minHeight: 620)
        // After an update (once this window is up), or from About → What's New.
        .sheet(item: $model.whatsNew) { WhatsNewSheet(notes: $0) }
        .onAppear {
            model.refreshTrusted()
            model.refreshPermissions()
            model.refreshDisplays()
            // On the next turn, once the window can carry a sheet.
            DispatchQueue.main.async { model.showPendingWhatsNew() }
            #if DEBUG
            model.openPreviewPane()
            #endif
        }
    }

    @ViewBuilder private var detail: some View {
        switch model.pane {
        case .home, .connect: HomeView(model: model)
        case .share: SharePage(model: model, settings: model.settings)
        case .peer(let id):
            if let peer = model.discovered.first(where: { $0.id == id }) {
                PeerPage(model: model, peer: peer)
            } else {
                HomeView(model: model)
            }
        case .devices: DevicesPage(model: model, settings: model.settings)
        case .settings: SettingsPage(model: model, settings: model.settings)
        case .about: AboutPage(model: model, settings: model.settings, updater: model.updater)
        }
    }
}

/// The window itself: an empty unified toolbar so the traffic lights sit inset on the sidebar's
/// glass (as in Apple's own sidebar apps) instead of on its corner, no title, content under the
/// title bar, and a real full-screen window.
struct WindowChrome: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async { view.window.map(Self.configure) }
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {}

    static func configure(_ window: NSWindow) {
        window.identifier = Self.id
        if window.toolbar == nil {
            let toolbar = NSToolbar(identifier: "annex.main")
            toolbar.showsBaselineSeparator = false
            window.toolbar = toolbar
            // The toolbar is only there to inset the traffic lights. In full screen there are
            // none, and an empty toolbar would slide down as a grey band.
            let center = NotificationCenter.default
            center.addObserver(forName: NSWindow.willEnterFullScreenNotification, object: window, queue: .main) { _ in
                MainActor.assumeIsolated { window.toolbar?.isVisible = false }
            }
            center.addObserver(forName: NSWindow.willExitFullScreenNotification, object: window, queue: .main) { _ in
                MainActor.assumeIsolated { window.toolbar?.isVisible = true }
            }
        }
        window.toolbarStyle = .unified
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.styleMask.insert(.fullSizeContentView)
        allowFullScreen(window)
        #if DEBUG
        // Screenshots: `ANNEX_PREVIEW_SIZE=1080x940`.
        if let size = ProcessInfo.processInfo.environment["ANNEX_PREVIEW_SIZE"]?.split(separator: "x").compactMap({ Double($0) }),
           size.count == 2, let screen = window.screen?.visibleFrame {
            window.setFrame(NSRect(x: screen.minX + 20, y: screen.maxY - size[1], width: size[0], height: size[1]), display: true)
        }
        #endif
    }

    static let id = NSUserInterfaceItemIdentifier("annex.main")

    /// While Annex is only in the menu bar, AppKit marks its windows `.auxiliary` and
    /// `.fullScreenNone`, which turns the green button into plain zoom. This is the app's main
    /// window, so put full screen back; `DockPresence` calls this again on every switch.
    static func allowFullScreen(_ window: NSWindow) {
        window.collectionBehavior.remove([.auxiliary, .fullScreenAuxiliary, .fullScreenNone])
        window.collectionBehavior.insert([.primary, .fullScreenPrimary])
    }
}

// MARK: Sidebar

/// Three places: the computers around you, this Mac, and the ones you've paired. Settings and
/// About sit at the bottom, out of the way.
private struct Sidebar: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            // Room for the window's traffic lights, which sit on the glass.
            Color.clear.frame(height: 30)
            HStack(spacing: 9) {
                AppIcon(size: 26)
                Text("Annex").font(.system(size: 15, weight: .semibold, design: .rounded))
            }
            .padding(.horizontal, 8)
            .padding(.bottom, 12)
            .accessibilityHidden(true)
            item("Nearby", "dot.radiowaves.left.and.right", .home, badge: sharingCount, also: [.connect])
            item("This Mac", "laptopcomputer", .share, live: model.isHosting)
            item("Paired Devices", "person.2", .devices)
            Spacer()
            item("Settings", "gearshape", .settings)
            item("About", "info.circle", .about)
        }
        .padding(8)
        .frame(maxHeight: .infinity)
        .glassPanel(radius: 22)
    }

    private var sharingCount: Int? {
        let n = model.discovered.filter(\.isSharing).count
        return n > 0 ? n : nil
    }

    private func item(_ title: String, _ symbol: String, _ pane: Pane, live: Bool = false, badge: Int? = nil,
                      also: [Pane] = []) -> some View {
        SidebarItem(title: title, symbol: symbol, live: live, badge: badge,
                    selected: model.pane == pane || also.contains(model.pane)) {
            withAnimation(.snappy) { model.pane = pane }
        }
    }
}

private struct SidebarItem: View {
    let title: String
    let symbol: String
    let live: Bool
    let badge: Int?
    let selected: Bool
    let action: () -> Void
    @State private var hovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                // A fixed box keeps every symbol centred on the same column, whatever its shape.
                Image(systemName: symbol)
                    .font(.system(size: 13.5, weight: .medium))
                    .foregroundStyle(selected ? Color.black : Color.secondary)
                    .frame(width: 20, height: 20)
                Text(title)
                    .font(.system(size: 13, weight: selected ? .semibold : .regular))
                    .foregroundStyle(selected ? Color.black : Color.primary)
                    .lineLimit(1)
                Spacer(minLength: 4)
                if live { LiveDot(size: 7) }
                if let badge {
                    Text("\(badge)")
                        .font(.system(size: 10.5, weight: .bold)).monospacedDigit()
                        .foregroundStyle(selected ? Color.black.opacity(0.6) : Color.secondary)
                }
            }
            .padding(.horizontal, 10)
            .frame(height: 32)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background {
                if selected {
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(Color.white.opacity(0.92))
                        .shadow(color: Color.white.opacity(0.18), radius: 12)
                } else if hovered {
                    RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Color.primary.opacity(0.06))
                }
            }
            .contentShape(Rectangle())
        }
        .pointingHand()
        .buttonStyle(.plain)
        .focusEffectDisabled()
        .onHover { hovered = $0 }
    }
}


// MARK: Building blocks
//
// Every page uses one design language, the one the About page started: a header card, then
// sections, each a small uppercase label over one rounded glass group of full-width rows divided by
// hairlines. Rows carry text on the left and one control on the right; only status rows get an
// icon (a check, a warning, a spinner), never a coloured tile.

/// A rounded monochrome icon. `tint` is kept for callers; the look is black and white.
struct IconTile: View {
    let symbol: String
    let tint: Color
    var size: CGFloat = 26

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: size * 0.5, weight: .semibold))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(RoundedRectangle(cornerRadius: size * 0.27, style: .continuous).fill(Color.white.opacity(0.1)))
    }
}

/// A page: a scrolling column of cards and sections, centred and no wider than reads well.
private struct Page<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) { content }
                .padding(.horizontal, 28)
                .padding(.top, 28)
                .padding(.bottom, 28)
                .frame(maxWidth: 720)
                .frame(maxWidth: .infinity)
        }
        .scrollContentBackground(.hidden)
    }
}

/// A rounded group, like System Settings' boxes. Rows end in a hairline; the last one is clipped
/// away so the group never shows a line along its bottom edge.
private struct SettingsGroup<Content: View>: View {
    var padding: CGFloat = 0
    @ViewBuilder var content: Content

    var body: some View {
        VStack(spacing: 0) { content }
            .padding(.bottom, padding == 0 ? -1 : 0)
            .clipped()
            .padding(padding)
            .frame(maxWidth: .infinity)
            .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Color.white.opacity(0.045)))
            .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Color.white.opacity(0.08)))
    }
}

/// A section: a small bold title (optionally with a control at its right), one group, a footnote.
private struct GroupSection<Content: View, Accessory: View>: View {
    var title: String?
    var footer: String?
    @ViewBuilder var accessory: Accessory
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let title {
                HStack {
                    Eyebrow(text: title)
                    Spacer()
                    accessory
                }
                .padding(.horizontal, 4)
            }
            SettingsGroup { content }
            if let footer, !footer.isEmpty {
                Text(footer)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .padding(.horizontal, 4)
            }
        }
    }
}

private extension GroupSection where Accessory == EmptyView {
    init(_ title: String? = nil, footer: String? = nil, @ViewBuilder content: () -> Content) {
        self.init(title: title, footer: footer, accessory: { EmptyView() }, content: content)
    }
}

/// One full-width cell of a group: padding and the hairline under it. For custom row layouts.
private struct GroupCell<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(spacing: 0) {
            content
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
                .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
            Divider().padding(.horizontal, 16)
        }
    }
}

/// What a status row shows on its left.
private enum RowStatus {
    case symbol(String, Color)
    case spinner
}

/// A row: title and optional subtitle on the left, one control on the right.
private struct GroupRow<Trailing: View>: View {
    let title: String
    var subtitle: String?
    var status: RowStatus?
    @ViewBuilder var trailing: Trailing

    var body: some View {
        GroupCell {
            HStack(spacing: 10) {
                switch status {
                case .symbol(let name, let tint):
                    Image(systemName: name).foregroundStyle(tint).font(.system(size: 16))
                case .spinner:
                    ProgressView().controlSize(.small)
                case nil:
                    EmptyView()
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).fixedSize(horizontal: false, vertical: true)
                    if let subtitle {
                        Text(subtitle).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                }
                // The text gets the room; controls take only their natural size (never truncated).
                .layoutPriority(1)
                Spacer(minLength: 12)
                trailing.fixedSize()
            }
        }
    }
}

private extension GroupRow where Trailing == EmptyView {
    init(title: String, subtitle: String? = nil, status: RowStatus? = nil) {
        self.init(title: title, subtitle: subtitle, status: status) { EmptyView() }
    }
}

/// A row with a switch on the right.
private struct ToggleRow: View {
    let title: String
    var subtitle: String?
    @Binding var isOn: Bool
    var disabled = false

    var body: some View {
        GroupRow(title: title, subtitle: subtitle) {
            Toggle("", isOn: $isOn).toggleStyle(.switch).labelsHidden().pointingHand().disabled(disabled)
        }
    }
}

/// The top of each page: its title and one line on what it's for. Plain text, no card.
private struct Hero<Icon: View>: View {
    let title: String
    let text: String
    @ViewBuilder var icon: Icon

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.system(size: 28, weight: .semibold, design: .rounded))
            Text(text).font(.system(size: 13)).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.bottom, 4)
    }
}

private extension Hero where Icon == IconTile {
    init(title: String, text: String, symbol: String, tint: Color) {
        self.init(title: title, text: text) { IconTile(symbol: symbol, tint: tint, size: 52) }
    }
}

/// Re-reads permissions every couple of seconds while a page is open (System Settings doesn't
/// tell apps when they change).
private struct WatchesPermissions: ViewModifier {
    let model: AppModel

    func body(content: Content) -> some View {
        content.task {
            while !Task.isCancelled {
                model.refreshPermissions()
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }
}

private func openPrivacySettings(_ anchor: String) {
    if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)") {
        NSWorkspace.shared.open(url)
    }
}

// MARK: Share

private struct SharePage: View {
    @ObservedObject var model: AppModel
    @ObservedObject var settings: AppSettings

    var body: some View {
        Page(title: "Share This Mac") {
            Hero(title: "This Mac",
                 text: "Let another computer show this screen, or extend this display onto it.",
                 symbol: "display.2", tint: .blue)
            UpdateBanner(updater: model.updater)
            SameNameNotice(model: model) { model.pane = .settings }
            GroupSection {
                GroupRow(title: model.isHosting ? "Sharing is on" : "Sharing is off", subtitle: shareStatus,
                         status: model.isHosting ? .symbol("dot.radiowaves.left.and.right", .green)
                                                 : .symbol("pause.circle.fill", .secondary)) {
                    if model.isHosting {
                        Button(role: .destructive) { model.setHosting(false) } label: {
                            Label("Stop Sharing", systemImage: "stop.fill").padding(.horizontal, 6)
                        }
                        .pointingHand()
                        .controlSize(.large)
                    } else {
                        Button { model.setHosting(true) } label: {
                            Label("Start Sharing", systemImage: "play.fill").padding(.horizontal, 6)
                        }
                        .pointingHand()
                        .primaryGlassButton()
                        .controlSize(.large)
                    }
                }
                if model.displays.count > 1 {
                    GroupRow(title: "Screen to share",
                             subtitle: model.isHosting ? "Stop sharing to pick another screen." : nil) {
                        Picker("", selection: $model.selectedDisplayID) {
                            ForEach(model.displays) { d in Text(d.name).tag(Optional(d.id)) }
                        }
                        .pointingHand()
                        .labelsHidden()
                        .fixedSize()
                        .disabled(model.isHosting)
                    }
                }
                if model.isHosting && !model.screenRecordingAllowed {
                    GroupRow(title: "Screen Recording is off",
                             subtitle: "Allow Annex in Privacy & Security, then reopen Annex. After an update, remove it with − and add it again.",
                             status: .symbol("exclamationmark.triangle.fill", .orange)) {
                        Button("Open Settings") { openPrivacySettings("Privacy_ScreenCapture") }
                            .pointingHand()
                    }
                }
                if let err = model.hostStatus.error {
                    GroupRow(title: err, status: .symbol("exclamationmark.triangle.fill", .orange))
                }
            }
            if model.isHosting && model.hostStatus.viewerName != nil {
                let s = model.hostStatus
                GroupSection("Connected now") {
                    ForEach(s.viewers) { v in
                        GroupRow(title: v.extend ? "This Mac is extending onto \(model.label(v.name))" : "This Mac's screen is on \(model.label(v.name))",
                                 subtitle: v.extend ? "Arrange it in System Settings → Displays."
                                     : model.settings.showTechnicalDetails
                                         ? s.stream.map { "\($0.width)×\($0.height) at \($0.fps) fps" } : nil) {
                            if model.hostDirectWiFi == nil {
                                Button("Direct Wi-Fi") { model.switchViewerToDirectWiFi(v) }
                                    .pointingHand()
                                    .help("Join \(v.name)'s hotspot for less lag. Works with a Windows PC.")
                            }
                        }
                    }
                    if let ssid = model.hostDirectWiFi {
                        GroupRow(title: "On direct Wi-Fi", subtitle: "Joined “\(ssid)”.")
                    } else if let direct = model.directStatus {
                        GroupRow(title: "Direct Wi-Fi", subtitle: direct)
                    }
                    ForEach(s.controllingViewers, id: \.self) { name in
                        GroupRow(title: "\(model.label(name)) is controlling this Mac",
                                 subtitle: "Turn it off in Paired Devices." + (s.relayedViewers.contains(name) ? " · Relay" : ""))
                    }
                }
            }
            if model.isHosting {
                InternetShareSection(model: model, settings: settings)
            }
            if model.isHosting, let port = model.hostStatus.listeningPort {
                HowToConnect(model: model, port: port)
            }
            if model.isHosting && settings.peerToPeerWiFi {
                NewMacSection(model: model)
            }
            if !model.isHosting {
                GroupSection("How it works") {
                    GroupRow(title: "Start sharing, then pick this Mac in Annex on the other computer.",
                             subtitle: "The first time, it asks for a PIN shown here. After that, it just connects.")
                }
            }
        }
        .modifier(WatchesPermissions(model: model))
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
        GroupSection("On the same network",
                     footer: model.settings.showTechnicalDetails ? "Identity code: \(model.fingerprintDisplay)" : nil) {
            GroupRow(title: "Pick “\(model.deviceName)” in Annex on the other computer.",
                     subtitle: "The first time, type the PIN this Mac shows. Not listed? Connect by one of these addresses.")
            if addresses.isEmpty {
                GroupRow(title: "This Mac isn't on a network.", subtitle: "Connect it to Wi-Fi, Ethernet or a cable.",
                         status: .symbol("exclamationmark.triangle.fill", .orange))
            }
            ForEach(addresses, id: \.self) { a in
                let text = port == Int(annexDefaultPort) ? a.ip : "\(a.ip):\(port)"
                GroupCell {
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
                        .pointingHand()
                        .controlSize(.small)
                    }
                }
            }
        }
        .onAppear { addresses = LocalAddresses.current() }
    }
}

/// Sharing with a computer on another network: a one-time code, and what happens with it.
private struct InternetShareSection: View {
    @ObservedObject var model: AppModel
    @ObservedObject var settings: AppSettings
    @State private var copied = false

    var body: some View {
        GroupSection("Over the internet",
                     footer: "The server only introduces the two computers. The screen goes "
                         + "straight between them, encrypted, or through a relay when a network blocks that.") {
            content
        }
    }

    @ViewBuilder private var content: some View {
        switch model.internetShare {
        case nil:
            getCode(title: "Share with a code", subtitle: "For a computer on another network. Works once, for 10 minutes.")
        case .starting:
            GroupRow(title: "Getting a code…", status: .spinner)
        case .code(let code, let expires):
            CodeCell(code: code, expires: expires, copied: $copied, stop: { model.stopCode() })
        case .joining(let name):
            GroupRow(title: "\(name) is connecting…", subtitle: "Typed the code. The PIN comes next if it's new.",
                     status: .spinner)
        case .relay(let name, true, let limited, let left, let perDay):
            GroupRow(title: "Connecting \(name) through the relay…",
                     subtitle: (limited ? "Their network blocks direct connections. Our relay passes it on, still end to end encrypted."
                                        : "Their network blocks direct connections. Using your relay.")
                         + (AppModel.relaySessionsLeft(left, perDay).map { " " + $0 } ?? ""),
                     status: .spinner)
        case .relay(let name, false, _, let left, let perDay):
            GroupRow(title: "\(name) couldn't connect directly",
                     subtitle: left == 0 ? AppModel.relayUsedUp(perDay) : "No relay is available right now.",
                     status: .symbol("exclamationmark.triangle.fill", .orange))
        case .ended(nil):
            getCode(title: "Code used", subtitle: "Get another one for the next computer.", again: true)
        case .ended(let why?):
            GroupRow(title: why, status: .symbol("exclamationmark.triangle.fill", .orange))
            getCode(title: "Try again", subtitle: nil, again: true)
        }
    }

    private func getCode(title: String, subtitle: String?, again: Bool = false) -> some View {
        GroupRow(title: title, subtitle: subtitle) {
            Button { copied = false; model.shareCode() } label: {
                Label(again ? "New Code" : "Get a Code", systemImage: "number").padding(.horizontal, 4)
            }
            .pointingHand()
            .primaryGlassButton()
        }
    }
}

/// The code itself: large, easy to read aloud, with how long it still works.
private struct CodeCell: View {
    let code: String
    let expires: Date
    @Binding var copied: Bool
    let stop: () -> Void

    var body: some View {
        GroupCell {
            HStack(alignment: .center, spacing: 16) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(code)
                        .font(.system(size: 34, weight: .semibold, design: .monospaced))
                        .tracking(3)
                        .textSelection(.enabled)
                        .accessibilityLabel("Code \(code.map(String.init).joined(separator: " "))")
                    TimelineView(.periodic(from: .now, by: 1)) { ctx in
                        let left = max(0, Int(expires.timeIntervalSince(ctx.date)))
                        Text(left > 0 ? "On the other computer: Connect by Code or Address. Works for \(left / 60):\(String(format: "%02d", left % 60))."
                                      : "Expired.")
                            .font(.callout).foregroundStyle(.secondary).monospacedDigit()
                    }
                }
                .padding(.vertical, 6)
                Spacer(minLength: 12)
                VStack(alignment: .trailing, spacing: 8) {
                    Button(copied ? "Copied" : "Copy") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(code, forType: .string)
                        copied = true
                    }
                        .pointingHand()
                    Button("Stop", role: .destructive, action: stop)
                        .pointingHand()
                }
                .fixedSize()
            }
        }
    }
}

/// The port hosts use unless it's taken.
private let annexDefaultPort: UInt16 = 47800

/// Shown while nothing that's sharing has been found: what usually blocks discovery.
private struct NotShowingUp: View {
    var body: some View {
        GroupSection("Can't find it?") {
            GroupRow(title: "Use the same network", subtitle: "Guest Wi-Fi and VPNs can hide computers.")
            GroupRow(title: "Allow Local Network for Annex") {
                Button("Open Settings") { openPrivacySettings("Privacy_LocalNetwork") }
                    .pointingHand()
            }
            GroupRow(title: "On Windows, allow the firewall", subtitle: "In Annex on the PC: Nearby → Allow Annex through the firewall.")
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
            .pointingHand()
            .primaryGlassButton()
            .help("Use the other computer's screen on this Mac.")
            Button {
                model.extendMode = true
                connect()
            } label: {
                Label("Extend Display", systemImage: "rectangle.split.2x1")
            }
            .pointingHand()
            .help(peer?.canExtend == false
                  ? "Extend isn't set up on that computer."
                  : "This Mac becomes a second screen for it.")
            .disabled(peer?.canExtend == false)
        }
        .fixedSize()
        .disabled(model.isViewing || peer?.isSharing == false)
    }
}

/// For a computer that isn't on the radar: type its address, or join a PC's Direct Wi-Fi.
struct OtherWaysSheet: View {
    @ObservedObject var model: AppModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 22) {
                    HStack(alignment: .top) {
                        Hero(title: "Connect by Code or Address",
                             text: "For a computer that isn't in Nearby: another network, a VPN, or a PC's Direct Wi-Fi.") { EmptyView() }
                        SheetCloseButton { dismiss() }
                    }
                    CodeSection(model: model, dismiss: { dismiss() })
                    GroupSection("Address", footer: "Through a VPN like Tailscale or ZeroTier, type the address it gives.") {
                        GroupRow(title: "IP address or host:port") {
                            InputField(text: $model.manualAddress, placeholder: "192.168.1.20",
                                       onSubmit: { model.extendMode = false; model.connectManual(); dismiss() })
                                .frame(width: 200)
                        }
                        GroupCell {
                            HStack {
                                Spacer()
                                ConnectButtons(model: model, connect: { model.connectManual(); dismiss() }, compact: true)
                                    .disabled(model.isViewing || model.manualAddress.trimmingCharacters(in: .whitespaces).isEmpty)
                            }
                        }
                    }
                    PCHotspotSection(model: model)
            }
            .padding(28)
            .fixedSize(horizontal: false, vertical: true)
            Divider().opacity(0.4)
            HStack {
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.defaultAction)
                    .pointingHand()
            }
            .padding(16)
        }
        .frame(width: 560)
        .preferredColorScheme(.dark)
        .tint(Color(white: 0.82))
        // Fresh every time: nothing typed last time, whether it connected, failed or was closed.
        .onAppear {
            Annex.log(.debug, "[viewer] Connect by Code or Address opened")
            model.resetConnectSheet()
        }
        .onDisappear { model.resetConnectSheet() }
    }
}

/// The round ✕ at a sheet's top right, like the one on a computer's card. Esc presses it.
private struct SheetCloseButton: View {
    let action: () -> Void

    var body: some View {
        Button(action: action) { Image(systemName: "xmark").font(.system(size: 11, weight: .bold)) }
            .keyboardShortcut(.cancelAction)
            .pointingHand()
            .buttonStyle(.plain)
            .frame(width: 26, height: 26)
            .background(Circle().fill(Color.white.opacity(0.08)))
            .contentShape(Circle())
            .help("Close (Esc)")
            .accessibilityLabel("Close")
    }
}

/// Connect with the code a computer on another network shows (This Mac → Over the internet).
private struct CodeSection: View {
    @ObservedObject var model: AppModel
    let dismiss: () -> Void

    var body: some View {
        GroupSection("Code", footer: "The other computer shows it under Over the internet. It works once.") {
            GroupRow(title: "Code from the other computer") {
                InputField(text: $model.manualCode, placeholder: "K7P-4QX",
                           onSubmit: { model.extendMode = false; model.connectByCode(); dismiss() }, focusOnAppear: true)
                    .frame(width: 200)
            }
            GroupCell {
                HStack {
                    Spacer()
                    ConnectButtons(model: model, connect: { model.connectByCode(); dismiss() }, compact: true)
                        .disabled(model.isViewing || model.manualCode.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
        }
    }
}

/// Joins a Windows PC's hotspot by hand, for networks that keep computers from reaching each other.
private struct PCHotspotSection: View {
    @ObservedObject var model: AppModel

    var body: some View {
        GroupSection("Direct Wi-Fi to a PC",
                     footer: "When this network won't connect the two computers: on the PC, turn on This PC → Direct Wi-Fi "
                         + "and type the network and password it shows. Works whether this Mac views or shares.") {
            GroupRow(title: "Network") {
                InputField(text: $model.pcHotspotName, placeholder: "Network name").frame(width: 200)
            }
            GroupRow(title: "Password") {
                InputField(text: $model.pcHotspotPassword, placeholder: "From the PC",
                           onSubmit: { model.joinPCHotspot() })
                    .frame(width: 200)
            }
            GroupCell {
                HStack {
                    Spacer()
                    if model.joiningPCHotspot { ProgressView().controlSize(.small) }
                    Button("Join") { model.joinPCHotspot() }
                        .pointingHand()
                        .disabled(model.joiningPCHotspot || model.isViewing
                                  || model.pcHotspotName.trimmingCharacters(in: .whitespaces).isEmpty
                                  || model.pcHotspotPassword.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
        }
    }
}

/// Host: peer-to-peer Wi-Fi shows this Mac to paired Macs only; this opens it to every Mac
/// nearby for a few minutes, so a new one can find it and pair.
private struct NewMacSection: View {
    @ObservedObject var model: AppModel

    var body: some View {
        GroupSection("Macs with no network",
                     footer: "Macs paired with this one find it over peer-to-peer Wi-Fi, even on another network or none. "
                         + "A new Mac still needs the PIN shown here.") {
            if let until = model.visibleToNewMacsUntil {
                GroupRow(title: "Any Mac nearby can find this one",
                         subtitle: "Until \(until.formatted(date: .omitted, time: .shortened)).",
                         status: .symbol("person.2.wave.2.fill", .primary)) {
                    Button("Stop") { model.hideFromNewMacs() }
                        .pointingHand()
                }
            } else {
                GroupRow(title: "Let a new Mac find this one", subtitle: "For 5 minutes, like AirDrop’s Everyone.") {
                    Button("Allow") { model.letNewMacsFindThisOne() }
                        .pointingHand()
                }
            }
        }
    }
}

private struct PeerRow: View {
    @ObservedObject var model: AppModel
    let peer: PeerInfo

    var body: some View {
        GroupRow(title: model.label(peer.name), subtitle: subtitle) {
            ConnectButtons(model: model, connect: { model.connect(to: peer) }, compact: true, peer: peer)
        }
        .opacity(peer.isSharing ? 1 : 0.6)
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
        if model.directStatus != nil || model.viewerStatus != nil {
            GroupSection {
                if let direct = model.directStatus {
                    GroupRow(title: "Direct Wi-Fi", subtitle: direct)
                }
                if let status = model.viewerStatus {
                    GroupRow(title: status,
                             subtitle: model.settings.showTechnicalDetails
                                 ? model.latency.map { String(format: "Latency %.0f ms (p95 %.0f ms) · %.0f fps", $0.p50Ms, $0.p95Ms, $0.fps) }
                                 : nil,
                             status: model.isViewing ? .symbol("play.circle.fill", .green) : .symbol("info.circle", .secondary)) {
                        if model.isViewing {
                            Button(role: .destructive) { model.disconnectViewer() } label: {
                                Label("Disconnect", systemImage: "xmark")
                            }
                                .pointingHand()
                        }
                    }
                    if model.isViewing && model.controllingHost {
                        GroupRow(title: "You're in control", subtitle: "Press ⌃⌥⇧Q to leave.")
                    }
                    if model.isViewing && model.onDirectWiFi {
                        GroupRow(title: "On direct Wi-Fi", subtitle: "Connected straight to the PC's hotspot.")
                    } else if model.isViewing {
                        GroupRow(title: "Switch to direct Wi-Fi", subtitle: "Less lag: connect straight to the PC's hotspot.") {
                            Button("Switch") { model.switchToDirectWiFi() }
                                .pointingHand()
                        }
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
            GroupSection {
                if !peer.isSharing {
                    GroupRow(title: "Not sharing", subtitle: "Start sharing on \(peer.name) first.",
                             status: .symbol("info.circle", .secondary))
                }
                GroupRow(title: "Show its screen here", subtitle: "Use \(peer.name)'s screen on this Mac.") {
                    Button("Show Its Screen") {
                        model.extendMode = false
                        model.connect(to: peer)
                    }
                    .pointingHand()
                    .primaryGlassButton()
                    .disabled(model.isViewing || !peer.isSharing)
                }
                GroupRow(title: "Extend display", subtitle: "This Mac becomes a second screen for \(peer.name).") {
                    Button("Extend Display") {
                        model.extendMode = true
                        model.connect(to: peer)
                    }
                    .pointingHand()
                    .disabled(model.isViewing || !peer.isSharing || !peer.canExtend)
                }
            }
            GroupSection {
                ToggleRow(title: "Use direct Wi-Fi",
                          subtitle: "Connect straight to a Windows PC's hotspot first, then start. Less lag.",
                          isOn: $model.useDirectWiFi, disabled: model.isViewing)
            }
            if model.settings.showTechnicalDetails {
                PeerDetails(peer: peer)
            }
        }
    }
}

/// Addresses, port and identity code (Settings → Developer → Show technical details).
private struct PeerDetails: View {
    let peer: PeerInfo

    var body: some View {
        GroupSection("Details") {
            GroupRow(title: "Addresses", subtitle: peer.addresses.joined(separator: ", "))
            GroupRow(title: "Port", subtitle: "\(peer.port)")
            if let fp = peer.fingerprint {
                GroupCell {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Identity code")
                        Text(Annex.displayFingerprint(fp))
                            .font(.system(.callout, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                }
            }
        }
    }
}

// MARK: Settings

/// Preferences: the five most people change, then everything else under Advanced, closed by default.
private struct SettingsPage: View {
    @ObservedObject var model: AppModel
    @ObservedObject var settings: AppSettings
    @State private var name = ""
    // Screenshots: `ANNEX_PREVIEW_ADVANCED=1` opens Advanced.
    @State private var showAdvanced = ProcessInfo.processInfo.environment["ANNEX_PREVIEW_ADVANCED"] != nil

    var body: some View {
        Page(title: "Settings") {
            Hero(title: "Settings", text: "How Annex starts, shares and shows other computers.",
                 symbol: "gearshape.fill", tint: .gray)
            general
            advancedHeader
            if showAdvanced {
                sharing
                viewing
                sound
                clipboardAndFiles
                internet
                developer
            }
        }
        .modifier(WatchesPermissions(model: model))
    }

    private var general: some View {
        GroupSection {
            GroupRow(title: "Device name", subtitle: "Shown to other computers. Leave empty to use the Mac's own name.") {
                // Empty = the Mac's own name (shown as the placeholder).
                InputField(text: $name, placeholder: AppModel.systemName, onSubmit: {
                    if name.trimmingCharacters(in: .whitespaces) != (settings.customDeviceName ?? "") {
                        model.rename(name)
                    }
                })
                .frame(width: 220)
            }
            .onAppear { name = settings.customDeviceName ?? "" }
            ToggleRow(title: "Open at login",
                      isOn: Binding(get: { settings.launchAtLogin }, set: { settings.setLaunchAtLogin($0) }))
            if let err = settings.launchAtLoginError {
                GroupRow(title: err, status: .symbol("exclamationmark.triangle.fill", .orange))
            }
            ToggleRow(title: "Start sharing when Annex opens", isOn: $settings.startSharingAtLaunch)
            ToggleRow(title: "No one at this screen",
                      subtitle: "For a Mac reached over SSH: pairing PINs also go to pairing-pin.txt in Annex's data folder, and paired devices can control it without asking.",
                      isOn: $settings.headless)
            GroupRow(title: "Quality", subtitle: "Use Fast if the picture stutters on Wi-Fi.") {
                Picker("", selection: $settings.quality) {
                    Text("Best").tag(0)
                    Text("Balanced").tag(1)
                    Text("Fast (Wi-Fi)").tag(2)
                }
                .pointingHand()
                .labelsHidden()
                .fixedSize()
            }
            ToggleRow(title: "Control the other computer", subtitle: "When it allows it. Press ⌃⌥⇧Q to leave.",
                      isOn: $settings.controlWhenViewing)
        }
    }

    private var advancedHeader: some View {
        Button { withAnimation(.snappy) { showAdvanced.toggle() } } label: {
            HStack(spacing: 6) {
                Image(systemName: "chevron.right")
                    .font(.caption.weight(.semibold))
                    .rotationEffect(.degrees(showAdvanced ? 90 : 0))
                Text("Advanced").font(.headline)
                Spacer()
                if !showAdvanced {
                    Text("You don't need to change these.").font(.callout).foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal, 4)
            .contentShape(Rectangle())
        }
        .pointingHand()
        .buttonStyle(.plain)
    }

    private var sharing: some View {
        GroupSection("Sharing") {
            ToggleRow(title: "Hardware video encoder",
                      subtitle: model.isHosting ? "Stop sharing to change it." : "Recommended. Lowest delay.",
                      isOn: $settings.preferHardwareEncoder, disabled: model.isHosting)
            ToggleRow(title: "HEVC",
                      subtitle: "A sharper picture at the same speed when the other computer supports it too. Applies to the next session.",
                      isOn: $settings.hevcWithMacs)
            ToggleRow(title: "Peer-to-peer Wi-Fi",
                      subtitle: "Another Mac can connect with no router, like AirDrop. "
                          + (model.isHosting || model.isViewing ? "Applies to the next session." : "Uses a little Wi-Fi speed while on."),
                      isOn: $settings.peerToPeerWiFi)
            GroupRow(title: "Extended display scaling",
                     subtitle: "Text size on a screen extending this Mac. Sharpest is crisp; the steps between look a little soft.") {
                Picker("", selection: $settings.extendScale) {
                    Text("Sharpest").tag(0)
                    ForEach(ExtendSession.scales, id: \.self) { Text("\($0) %").tag($0) }
                }
                .pointingHand()
                .labelsHidden()
                .fixedSize()
            }
        }
    }

    private var viewing: some View {
        GroupSection("Viewing") {
            ToggleRow(title: "Open in full screen", isOn: $settings.openFullScreen)
            ToggleRow(title: "Extend display by default", isOn: $settings.extendByDefault)
            ToggleRow(title: "Reverse Mouse Wheel on Windows and Linux",
                      subtitle: "With natural scrolling on, a mouse wheel turns the way it does on a PC. The trackpad stays natural.",
                      isOn: $settings.reverseScrollOnOthers)
            ToggleRow(title: "⌘ works like Ctrl on Windows and Linux",
                      subtitle: "⌘C, ⌘V and the rest do what Ctrl does there; ⌘Tab switches windows, ⌘Space opens Start, ⌘Q closes the window. Ctrl acts as the Windows key.",
                      isOn: $settings.commandAsControl)
        }
    }

    private var sound: some View {
        GroupSection("Sound") {
            ToggleRow(title: "Share this computer's sound", subtitle: "People viewing this computer hear what it plays.",
                      isOn: $settings.shareSound)
            ToggleRow(title: "Hear the other computer", subtitle: "Plays the shared computer's sound here, and a viewer's microphone.",
                      isOn: $settings.hearSound)
            ToggleRow(title: "Send my microphone",
                      subtitle: "Your voice plays on the computer you're viewing. Annex takes its sound back out, so neither side hears an echo.",
                      isOn: Binding(get: { settings.sendMicrophone }, set: { model.setSendMicrophone($0) }))
            if model.microphoneDenied {
                GroupRow(title: "Annex can't use the microphone. Allow it in System Settings → Privacy & Security → Microphone.",
                         status: .symbol("exclamationmark.triangle.fill", .orange))
            }
            if Annex.virtualMicrophoneSupported {
                ToggleRow(title: "Use a viewer's microphone as Annex Microphone",
                          subtitle: "Apps here, like Zoom or Meet, can pick Annex Microphone. Off: it plays aloud.",
                          isOn: $settings.virtualMicrophone)
            }
        }
    }

    private var clipboardAndFiles: some View {
        GroupSection("Clipboard and files") {
            ToggleRow(title: "Share the clipboard",
                      subtitle: "Copy on one computer, paste on the other, while one controls the other. Passwords copied from a password manager stay here.",
                      isOn: $settings.shareClipboard)
            ToggleRow(title: "Receive files", subtitle: "Files the other computer sends go to Downloads.",
                      isOn: $settings.receiveFiles)
            ToggleRow(title: "Drop files on the notch",
                      subtitle: "While connected, drag files to the top of the screen to send them.",
                      isOn: $settings.notchDrop)
        }
    }

    private var internet: some View {
        GroupSection("Internet",
                     footer: "Codes use Annex's server to swap the two computers' internet addresses; the screen goes straight "
                         + "between them, encrypted. Some networks block that: then it goes through a relay, yours if you set one here.") {
            GroupRow(title: "Server", subtitle: "Optional. Leave empty to use Annex's server.") {
                InputField(text: $settings.rendezvousServer, placeholder: AppSettings.annexServer).frame(width: 260)
            }
            GroupRow(title: "Your relay", subtitle: "Optional. A TURN server, for networks that block direct connections.") {
                InputField(text: $settings.relayURL, placeholder: "turn:relay.example.com:3478").frame(width: 260)
            }
            if !settings.relayURL.trimmingCharacters(in: .whitespaces).isEmpty {
                GroupRow(title: "Relay username") {
                    InputField(text: $settings.relayUsername, placeholder: "Username").frame(width: 260)
                }
                GroupRow(title: "Relay password", subtitle: "Kept in the Keychain.") {
                    SecureField("", text: $settings.relayPassword, prompt: Text("Password"))
                        .textFieldStyle(.roundedBorder).frame(width: 260)
                }
            }
        }
    }

    private var developer: some View {
        GroupSection("Developer") {
            ToggleRow(title: "Show technical details", subtitle: "Identity codes, addresses and connection details.",
                      isOn: $settings.showTechnicalDetails)
            ToggleRow(title: "Verbose logging",
                      subtitle: "Also logs every mouse and keyboard event and every frame, never which keys. The log fills faster; turn it on to chase a problem.",
                      isOn: $settings.verboseLogging)
            GroupRow(title: "Log files", subtitle: "Always on. Stays on this Mac; Annex never sends it anywhere. Attach annex.log, and anything in crashes, to a bug report if something goes wrong.") {
                Button("Show Log Files") {
                    let log = AppModel.dataDirectory.appendingPathComponent("annex.log")
                    Annex.log(.info, "[settings] Show Log Files pressed")
                    // The file selected in its folder; just the folder if there's no log yet.
                    if FileManager.default.fileExists(atPath: log.path) {
                        NSWorkspace.shared.activateFileViewerSelecting([log])
                    } else {
                        NSWorkspace.shared.open(AppModel.dataDirectory)
                    }
                }
                    .pointingHand()
            }
        }
    }

    /// Opens the bundled uninstaller in Terminal, which asks before removing anything.
    static func runUninstaller() {
        guard let script = Bundle.main.url(forResource: "Uninstall Annex", withExtension: "command"),
              let terminal = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Terminal")
        else { return }
        NSWorkspace.shared.open([script], withApplicationAt: terminal, configuration: NSWorkspace.OpenConfiguration())
    }
}

// MARK: Paired Devices

private struct DevicesPage: View {
    @ObservedObject var model: AppModel
    @ObservedObject var settings: AppSettings

    var body: some View {
        Page(title: "Paired Devices") {
            Hero(title: "Paired Devices", text: "Paired devices connect without a PIN.",
                 symbol: "person.2.fill", tint: .teal)
            if model.trusted.contains(where: \.allowInput) && !model.accessibilityTrusted {
                GroupSection {
                    GroupRow(title: "Accessibility is off",
                             subtitle: "Needed for remote control. After an update, remove Annex with − and add it again.",
                             status: .symbol("exclamationmark.triangle.fill", .orange)) {
                        Button("Open Settings") { InputInjector.openAccessibilitySettings() }
                            .pointingHand()
                    }
                }
            }
            if model.trusted.isEmpty {
                GroupSection {
                    GroupRow(title: "No paired devices", subtitle: "Pair by connecting from another computer.")
                }
            }
            ForEach(model.trusted) { peer in DeviceGroup(model: model, peer: peer) }
            if settings.showTechnicalDetails {
                Text("This Mac's identity code: \(model.fingerprintDisplay)")
                    .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                    .padding(.horizontal, 4)
            }
        }
        .modifier(WatchesPermissions(model: model))
    }
}

/// One paired device: who it is, whether it may use this Mac's mouse and keyboard, hear its
/// sound and talk through its microphone.
private struct DeviceGroup: View {
    @ObservedObject var model: AppModel
    let peer: TrustedPeer
    @State private var confirmRemove = false

    var body: some View {
        GroupSection {
            GroupRow(title: peer.name, subtitle: lastSeen) {
                Button("Remove…", role: .destructive) { confirmRemove = true }
                    .pointingHand()
            }
            // Headless lets every paired device control this Mac.
            ToggleRow(title: "Can use this Mac's mouse and keyboard",
                      subtitle: model.settings.headless ? "Allowed while No one at this screen is on." : nil,
                      isOn: Binding(get: { model.settings.headless || peer.allowInput }, set: { model.setAllowInput(peer, $0) }),
                      disabled: model.settings.headless)
            ToggleRow(title: "Can hear this Mac's sound",
                      subtitle: model.settings.shareSound ? nil : "This Mac's sound is off in Settings.",
                      isOn: Binding(get: { peer.allowSound }, set: { model.setAllowSound(peer, $0) }))
            ToggleRow(title: "Its microphone plays here",
                      isOn: Binding(get: { peer.allowMic }, set: { model.setAllowMic(peer, $0) }))
        }
        .confirmationDialog("Remove \(peer.name)?", isPresented: $confirmRemove) {
            Button("Remove", role: .destructive) { model.revoke(peer) }
        } message: {
            Text("It will need a PIN to connect again.")
        }
    }

    private var lastSeen: String {
        guard peer.lastSeen > 0 else { return "Paired" }
        return "Last connected " + Date(timeIntervalSince1970: TimeInterval(peer.lastSeen)).formatted(.relative(presentation: .named))
    }
}

// MARK: About

private struct AboutPage: View {
    @ObservedObject var model: AppModel
    @ObservedObject var settings: AppSettings
    @ObservedObject var updater: Updater

    private static let repo = URL(string: "https://github.com/NRanjan-17/Annex")!

    var body: some View {
        Page(title: "About") {
            header
            GroupSection("Updates") {
                ToggleRow(title: "Check for updates automatically", subtitle: "Once a day, from GitHub. Nothing about you is sent.",
                          isOn: $settings.checkForUpdates)
                ToggleRow(title: "Receive beta updates",
                          subtitle: "Beta versions include features in development and may contain bugs.",
                          isOn: $settings.betaUpdates)
                status
                GroupCell {
                    Button("Check Now") { Task { await updater.check() } }
                        .pointingHand()
                        .disabled(updater.isBusy)
                }
                GroupCell {
                    Text(lastChecked).font(.caption).foregroundStyle(.tertiary)
                }
            }
            GroupSection {
                GroupRow(title: "Uninstall Annex", subtitle: "Removes Annex, its settings and paired devices from this Mac.") {
                    Button("Uninstall…") { SettingsPage.runUninstaller() }
                        .pointingHand()
                }
            }
        }
    }

    private var header: some View {
        SettingsGroup(padding: 28) {
            VStack(spacing: 0) {
                Image(nsImage: NSApp.applicationIconImage)
                    .resizable()
                    .frame(width: 96, height: 96)
                Text("Annex").font(.system(size: 22, weight: .bold)).padding(.top, 10)
                Text("Version \(Annex.version)").font(.callout).foregroundStyle(.secondary).padding(.top, 2)
                Text("Use another computer as a second display, or mirror its screen.\nMac and Windows, over your network or a cable, encrypted end to end.")
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.top, 14)
                HStack(spacing: 12) {
                    Button("What's New") { model.showAllWhatsNew() }
                        .pointingHand()
                    Button("Release Notes") {
                        NSWorkspace.shared.open(Self.repo.appendingPathComponent("releases/tag/v\(Annex.version)"))
                    }
                        .pointingHand()
                    Button("Report an Issue") { NSWorkspace.shared.open(Self.repo.appendingPathComponent("issues")) }
                        .pointingHand()
                    Link("View on GitHub", destination: Self.repo)
                        .pointingHand()
                }
                .controlSize(.large)
                .padding(.top, 16)
                Text("MIT License · © 2026 Nalinish Ranjan and [Saidev Dhal](https://saidev.codes)")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .padding(.top, 16)
            }
            .frame(maxWidth: .infinity)
        }
    }

    /// The updater's state as one row: an icon, a sentence and the action that fits.
    private var status: some View {
        let (state, text): (RowStatus?, String) = switch updater.state {
        case .idle: (nil, "Annex \(Annex.version)")
        case .checking: (.spinner, "Checking for updates…")
        case .upToDate: (.symbol("checkmark.circle.fill", .green), "You're on the latest version.")
        case .available(let info): (.symbol("arrow.down.circle.fill", .primary), Updater.headline(info))
        case .downloading: (.spinner, "Downloading the update…")
        case .installing: (.spinner, "Installing… Annex reopens in a moment.")
        case .failed(let message): (.symbol("exclamationmark.triangle.fill", .orange), message)
        case .manual(let message): (.symbol("arrow.down.circle", .primary), message)
        }
        return GroupRow(title: text, status: state) {
            if case .available(let info) = updater.state {
                HStack(spacing: 12) {
                    if let page = URL(string: info.pageUrl) { Link("What's New", destination: page).pointingHand() }
                    Button("Install") { updater.confirmAndInstall() }
                        .pointingHand()
                }
            }
        }
    }

    private var lastChecked: String {
        guard let date = settings.lastUpdateCheck else { return "Not checked yet." }
        return "Last checked: " + date.formatted(date: .abbreviated, time: .shortened)
    }
}

// MARK: What's New

/// What's new in this version, shown once after an update; About → What's New shows every release.
private struct WhatsNewSheet: View {
    let notes: WhatsNew
    @Environment(\.dismiss) private var dismiss

    private static let groups = [("new", "New"), ("improved", "Improved"), ("fixed", "Fixed")]

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    Hero(title: "What's New in Annex \(notes.releases.first?.version ?? Annex.version)",
                         text: notes.all ? "Every version, newest first." : "What this update adds and fixes, in short.") { EmptyView() }
                    ForEach(Array(notes.releases.enumerated()), id: \.offset) { _, release in
                        // One version: the title says which. Several: a heading for each.
                        if notes.releases.count > 1 {
                            Text("Annex \(release.version)")
                                .font(.system(size: 17, weight: .semibold, design: .rounded))
                                .padding(.top, 4)
                        }
                        ForEach(Self.groups, id: \.0) { kind, heading in
                            let items = release.items.filter { $0.kind == kind }
                            if !items.isEmpty {
                                GroupSection(heading) {
                                    ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                                        GroupCell {
                                            VStack(alignment: .leading, spacing: 2) {
                                                Text(item.title).fontWeight(.semibold).fixedSize(horizontal: false, vertical: true)
                                                Text(item.detail).font(.callout).foregroundStyle(.secondary)
                                                    .fixedSize(horizontal: false, vertical: true)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if notes.releases.isEmpty {
                        Text("No release notes in this version.").foregroundStyle(.secondary)
                    }
                }
                .padding(28)
            }
            .scrollContentBackground(.hidden)
            .frame(height: 480)
            Divider().opacity(0.4)
            HStack {
                // Esc closes it too.
                Button("Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .frame(width: 0, height: 0)
                    .opacity(0)
                    .accessibilityHidden(true)
                Spacer()
                Button("Got It") { dismiss() }.keyboardShortcut(.defaultAction)
                    .pointingHand()
            }
            .padding(16)
        }
        .frame(width: 560)
        .preferredColorScheme(.dark)
        .tint(Color(white: 0.82))
        .onDisappear { Annex.log(.info, "[update] what's new seen") }
    }
}

/// Shown at the top of Share and Connect when an automatic check found an update.
struct UpdateBanner: View {
    @ObservedObject var updater: Updater

    var body: some View {
        if let info = updater.bannerUpdate {
            GroupSection {
                GroupRow(title: Updater.headline(info), status: .symbol("arrow.down.circle.fill", .primary)) {
                    HStack(spacing: 8) {
                        Button("Not Now") { updater.dismissBanner() }
                            .pointingHand()
                        Button("Install") { updater.confirmAndInstall() }
                            .pointingHand()
                    }
                }
            }
        }
    }
}
