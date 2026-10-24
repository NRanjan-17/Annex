import SwiftUI

/// Nearby: this Mac in the middle, the computers around it, and what's live. The one screen most
/// people need: pick a computer, then Extend or Show Its Screen.
///
/// Click a computer to open its card; double-click (or Return) uses the default action from
/// Settings. Arrow keys move between computers, Escape closes the card.
struct HomeView: View {
    @ObservedObject var model: AppModel
    @State private var selectedID: String?
    @State private var showOtherWays = false
    @State private var showNotSharing = false
    @FocusState private var focused: Bool

    private var visible: [PeerInfo] {
        showNotSharing ? model.discovered : model.discovered.filter(\.isSharing)
    }

    private var notSharingCount: Int { model.discovered.filter { !$0.isSharing }.count }

    private var selected: PeerInfo? {
        guard let id = selectedID else { return nil }
        return model.discovered.first { $0.id == id }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            NearbyHeader(model: model) { showOtherWays = true }
            UpdateBanner(updater: model.updater)
            HStack(alignment: .top, spacing: 18) {
                VStack(alignment: .leading, spacing: 10) {
                    SearchStatus(model: model, found: visible.count)
                    if visible.isEmpty {
                        Waiting(model: model) { showOtherWays = true }
                    } else {
                        NearbyList(model: model, peers: visible, selectedID: $selectedID, connect: connect)
                    }
                    if notSharingCount > 0 {
                        NotSharingToggle(count: notSharingCount, showing: showNotSharing) {
                            withAnimation(.spring(response: 0.5, dampingFraction: 0.85)) { showNotSharing.toggle() }
                        }
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                if let peer = selected {
                    PeerCard(model: model, peer: peer) { selectedID = nil }
                        .frame(width: 300)
                        .transition(.move(edge: .trailing).combined(with: .opacity))
                }
            }
            .animation(.spring(response: 0.45, dampingFraction: 0.85), value: selectedID)
            .animation(.spring(response: 0.5, dampingFraction: 0.86), value: visible.map(\.id))
            if model.isViewing || model.hostStatus.viewerName != nil {
                SessionStrip(model: model)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .padding(.horizontal, 28)
        .padding(.top, 22)
        .padding(.bottom, 24)
        .animation(.spring(response: 0.5, dampingFraction: 0.86), value: model.isViewing)
        .animation(.spring(response: 0.5, dampingFraction: 0.86), value: model.hostStatus.viewerName)
        .sheet(isPresented: $showOtherWays) { OtherWaysSheet(model: model) }
        .onChange(of: model.discovered) { _, peers in
            if let id = selectedID, !peers.contains(where: { $0.id == id }) { selectedID = nil }
        }
        // Keyboard: arrows move, Return connects, Escape closes the card.
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onAppear { focused = true }
        .onKeyPress(.escape) {
            guard selectedID != nil else { return .ignored }
            selectedID = nil
            return .handled
        }
        .onKeyPress(.return) {
            guard let peer = selected else { return .ignored }
            connect(peer)
            return .handled
        }
        .onKeyPress(keys: [.upArrow, .leftArrow]) { _ in move(by: -1) }
        .onKeyPress(keys: [.downArrow, .rightArrow]) { _ in move(by: 1) }
    }

    private func move(by step: Int) -> KeyPress.Result {
        guard !visible.isEmpty else { return .ignored }
        let i = visible.firstIndex { $0.id == selectedID }.map { ($0 + step + visible.count) % visible.count }
            ?? (step > 0 ? 0 : visible.count - 1)
        selectedID = visible[i].id
        return .handled
    }

    /// The default action (Settings → Viewing → Extend display by default), straight away.
    private func connect(_ peer: PeerInfo) {
        guard peer.isSharing, !model.isViewing else { selectedID = peer.id; return }
        model.extendMode = model.settings.extendByDefault && peer.canExtend
        model.connect(to: peer)
    }
}

// MARK: How a computer is reached

/// How this Mac would reach a computer, from the addresses it announced.
enum LinkKind {
    case cable, directWiFi, peerToPeer, network

    init(_ peer: PeerInfo) {
        if peer.instance.hasPrefix("p2p:") {
            self = .peerToPeer
        } else if peer.addresses.contains(where: { $0.hasPrefix("169.254.") || $0.lowercased().hasPrefix("fe80") }) {
            self = .cable
        } else if peer.addresses.contains("192.168.137.1") {
            self = .directWiFi
        } else {
            self = .network
        }
    }

    var label: String {
        switch self {
        case .cable: "Cable"
        case .directWiFi: "Direct Wi-Fi"
        case .peerToPeer: "Peer-to-peer"
        case .network: "Wi-Fi"
        }
    }

    var symbol: String {
        switch self {
        case .cable: "cable.connector"
        case .directWiFi: "wifi.router"
        case .peerToPeer: "dot.radiowaves.left.and.right"
        case .network: "wifi"
        }
    }
}

/// A small chip: how the computer is reached.
private struct LinkChip: View {
    let kind: LinkKind

    var body: some View {
        Label(kind.label, systemImage: kind.symbol)
            .labelStyle(.titleAndIcon)
            .font(.system(size: 10, weight: .medium))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 7)
            .frame(height: 18)
            .background(Capsule().fill(Color.white.opacity(0.07)))
            .fixedSize()
    }
}

// MARK: Header

/// Content, not chrome: a plain title. The controls on its right float, so they're glass.
private struct NearbyHeader: View {
    @ObservedObject var model: AppModel
    let otherWays: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Nearby").font(.system(size: 28, weight: .semibold, design: .rounded))
                Text("Click a computer for options. Double-click to connect.")
                    .font(.system(size: 13)).foregroundStyle(.secondary)
            }
            Spacer(minLength: 12)
            GlassGroup(spacing: 8) {
                HStack(spacing: 8) {
                    Button { model.refreshNearby() } label: {
                        Image(systemName: "arrow.clockwise").frame(width: 16)
                    }
                    .secondaryGlassButton()
                    .disabled(model.refreshingNearby)
                    .help("Look again")
                    .accessibilityLabel("Refresh Nearby")
                    Button("Connect by Address…", action: otherWays)
                        .secondaryGlassButton()
                }
            }
        }
    }
}

/// One line over the list: Annex is always looking, so say so, and say where.
private struct SearchStatus: View {
    @ObservedObject var model: AppModel
    let found: Int

    var body: some View {
        HStack(spacing: 10) {
            ScanningDots()
            Text(text).font(.system(size: 12, weight: .medium)).foregroundStyle(.secondary)
            Spacer()
        }
        .padding(.horizontal, 4)
        .accessibilityElement(children: .combine)
    }

    private var text: String {
        let ways = model.settings.peerToPeerWiFi && !model.inSession
            ? "Wi-Fi, cables and Mac to Mac" : "Wi-Fi and cables"
        switch found {
        case 0: return "Waiting for computers · looking on \(ways)"
        case 1: return "1 computer · still looking on \(ways)"
        default: return "\(found) computers · still looking on \(ways)"
        }
    }
}

/// Three dots lighting up in turn: looking, not stuck.
struct ScanningDots: View {
    var body: some View {
        TimelineView(.animation(minimumInterval: 1 / 12)) { context in
            let t = context.date.timeIntervalSinceReferenceDate
            HStack(spacing: 4) {
                ForEach(0..<3) { i in
                    let phase = (sin((t * 3.2) - Double(i) * 0.9) + 1) / 2
                    Circle()
                        .fill(Color.white.opacity(0.25 + 0.65 * phase))
                        .frame(width: 5, height: 5)
                }
            }
        }
        .accessibilityHidden(true)
    }
}

/// Nothing found yet: what to do on the other computer, and the ways round it.
private struct Waiting: View {
    @ObservedObject var model: AppModel
    let otherWays: () -> Void

    var body: some View {
        VStack(spacing: 16) {
            ZStack {
                PulseRings()
                AppIcon(size: 64)
                    .frame(width: 72, height: 72)
            }
            .frame(height: 150)
            VStack(spacing: 6) {
                Text("Waiting for a computer…").font(.system(size: 17, weight: .semibold, design: .rounded))
                Text("Open Annex on the other computer and turn on sharing.\nIt shows up here by itself.")
                    .font(.system(size: 12.5)).foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            GlassGroup(spacing: 8) {
                HStack(spacing: 8) {
                    Button("Connect by Address…", action: otherWays)
                        .secondaryGlassButton()
                    if model.isHosting && model.settings.peerToPeerWiFi && model.visibleToNewMacsUntil == nil {
                        Button("Let a New Mac Find This One") { model.letNewMacsFindThisOne() }
                            .secondaryGlassButton()
                    }
                }
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 36)
        .background(RoundedRectangle(cornerRadius: 18, style: .continuous).fill(Color.white.opacity(0.03)))
        .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous)
            .strokeBorder(Color.white.opacity(0.08), style: StrokeStyle(lineWidth: 1, dash: [4, 5])))
    }
}

/// Soft rings widening out from this Mac while it waits.
private struct PulseRings: View {
    var body: some View {
        TimelineView(.animation(minimumInterval: 1 / 30)) { context in
            let t = context.date.timeIntervalSinceReferenceDate
            ZStack {
                ForEach(0..<3) { i in
                    let p = ((t / 2.4) + Double(i) / 3).truncatingRemainder(dividingBy: 1)
                    Circle()
                        .strokeBorder(Color.white.opacity(0.28 * (1 - p)), lineWidth: 1)
                        .frame(width: 72 + 90 * p, height: 72 + 90 * p)
                }
            }
        }
        .accessibilityHidden(true)
    }
}

/// Under the list: computers that aren't sharing, hidden until asked for.
private struct NotSharingToggle: View {
    let count: Int
    let showing: Bool
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            Text(showing ? "Hide computers that aren't sharing" : "+\(count) not sharing")
                .font(.system(size: 11.5, weight: .medium))
                .padding(.horizontal, 12)
                .frame(height: 26)
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .glassCapsule(interactive: true)
    }
}

/// Symbol and one-line status for a computer.
enum PeerLook {
    static func status(_ peer: PeerInfo, paired: Bool) -> String {
        guard peer.isSharing else { return "Not sharing" }
        return (paired ? "Paired" : "New · PIN needed") + " · " + LinkKind(peer).label
    }

    static func symbol(for peer: PeerInfo) -> String {
        let n = peer.name.lowercased()
        if n.contains("book") || n.contains("laptop") { return "laptopcomputer" }
        if n.contains("imac") { return "desktopcomputer" }
        if n.contains("pc") || n.contains("windows") || n.contains("desktop-") { return "pc" }
        return "desktopcomputer"
    }
}

// MARK: List

/// The computers nearby, one row each.
private struct NearbyList: View {
    @ObservedObject var model: AppModel
    let peers: [PeerInfo]
    @Binding var selectedID: String?
    let connect: (PeerInfo) -> Void

    var body: some View {
        ScrollView {
            LazyVStack(spacing: 6) {
                ForEach(peers) { peer in
                    NearbyRow(peer: peer, paired: model.isPaired(peer), selected: selectedID == peer.id,
                              select: { selectedID = selectedID == peer.id ? nil : peer.id },
                              connect: { connect(peer) })
                }
            }
        }
        .scrollContentBackground(.hidden)
    }
}

private struct NearbyRow: View {
    let peer: PeerInfo
    let paired: Bool
    let selected: Bool
    let select: () -> Void
    let connect: () -> Void
    @State private var hovered = false

    var body: some View {
        HStack(spacing: 14) {
            Image(systemName: PeerLook.symbol(for: peer))
                .font(.system(size: 17))
                .foregroundStyle(selected ? Color.black : peer.isSharing ? Color.primary : Color.secondary)
                .frame(width: 38, height: 38)
                .background(RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(selected ? Color.black.opacity(0.08) : Color.white.opacity(0.07)))
            VStack(alignment: .leading, spacing: 2) {
                Text(peer.name).font(.system(size: 13.5, weight: .semibold))
                Text(peer.isSharing ? (paired ? "Paired" : "New · PIN needed") : "Not sharing")
                    .font(.system(size: 11.5))
                    .foregroundStyle(selected ? Color.black.opacity(0.6) : Color.secondary)
            }
            .foregroundStyle(selected ? Color.black : Color.primary)
            Spacer(minLength: 8)
            if peer.isSharing {
                LiveDot(size: 7)
                LinkChip(kind: LinkKind(peer)).colorScheme(selected ? .light : .dark)
            }
        }
        .padding(.horizontal, 12)
        .frame(height: 56)
        .background(RoundedRectangle(cornerRadius: 14, style: .continuous)
            .fill(selected ? Color.white.opacity(0.92) : Color.white.opacity(hovered ? 0.07 : 0.035)))
        .contentShape(Rectangle())
        .onTapGesture(count: 2, perform: connect)
        .onTapGesture(perform: select)
        .onHover { hovered = $0 }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(peer.name), \(PeerLook.status(peer, paired: paired))")
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction(.default, select)
        .accessibilityAction(named: "Connect", connect)
    }
}

// MARK: Selected computer

/// Floats beside the list, so it's glass; everything inside it is flat (no glass on glass).
/// Sized to what it holds.
private struct PeerCard: View {
    @ObservedObject var model: AppModel
    let peer: PeerInfo
    let close: () -> Void
    @State private var confirmForget = false

    private var trusted: TrustedPeer? {
        guard let fp = peer.fingerprint else { return nil }
        return model.trusted.first { $0.fingerprint == fp }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .top) {
                Image(systemName: PeerLook.symbol(for: peer))
                    .font(.system(size: 22))
                    .frame(width: 48, height: 48)
                    .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Color.white.opacity(0.08)))
                Spacer()
                Button(action: close) { Image(systemName: "xmark").font(.system(size: 11, weight: .bold)) }
                    .buttonStyle(.plain)
                    .frame(width: 26, height: 26)
                    .background(Circle().fill(Color.white.opacity(0.08)))
                    .help("Close (Esc)")
                    .accessibilityLabel("Close")
            }
            VStack(alignment: .leading, spacing: 6) {
                Text(peer.name).font(.system(size: 22, weight: .semibold, design: .rounded)).lineLimit(2)
                HStack(spacing: 6) {
                    Text(trusted != nil ? "Paired" : "New · asks for a PIN the first time")
                        .font(.system(size: 12)).foregroundStyle(.secondary)
                    if peer.isSharing { LinkChip(kind: LinkKind(peer)) }
                }
            }

            if !peer.isSharing {
                Label("Turn on sharing on \(peer.name) to connect.", systemImage: "info.circle")
                    .font(.system(size: 12.5)).foregroundStyle(.secondary)
            }

            VStack(spacing: 10) {
                Action(title: "Extend", detail: "This Mac becomes a second screen for it.",
                       symbol: "rectangle.split.2x1", prominent: true,
                       disabled: model.isViewing || !peer.isSharing || !peer.canExtend) {
                    model.extendMode = true
                    model.connect(to: peer)
                }
                Action(title: "Show Its Screen", detail: "Use its screen here, in a window.",
                       symbol: "rectangle.on.rectangle", prominent: false,
                       disabled: model.isViewing || !peer.isSharing) {
                    model.extendMode = false
                    model.connect(to: peer)
                }
            }

            Divider().opacity(0.5)
            VStack(alignment: .leading, spacing: 12) {
                Toggle(isOn: $model.useDirectWiFi) {
                    Detail(title: "Direct Wi-Fi first", text: "Join a Windows PC's hotspot. Less lag.")
                }
                .disabled(model.isViewing)
                if let trusted {
                    Toggle(isOn: Binding(get: { trusted.allowInput }, set: { model.setAllowInput(trusted, $0) })) {
                        Detail(title: "Can use this Mac's mouse and keyboard", text: "When it shows this Mac's screen.")
                    }
                    HStack {
                        Text(lastConnected(trusted)).font(.system(size: 11)).foregroundStyle(.secondary)
                        Spacer()
                        Button("Forget…") { confirmForget = true }
                            .buttonStyle(.plain)
                            .font(.system(size: 11.5, weight: .medium))
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .toggleStyle(.switch)
            .controlSize(.small)
            if model.settings.showTechnicalDetails, !peer.addresses.isEmpty {
                Text(peer.addresses.map { "\($0):\(peer.port)" }.joined(separator: "\n"))
                    .font(.system(size: 11, design: .monospaced)).foregroundStyle(.secondary).textSelection(.enabled)
            }
        }
        .padding(20)
        .glassPanel(radius: 26)
        .confirmationDialog("Forget \(peer.name)?", isPresented: $confirmForget) {
            Button("Forget", role: .destructive) {
                if let trusted { model.revoke(trusted) }
            }
        } message: {
            Text("It will need a PIN to connect again.")
        }
    }

    private func lastConnected(_ t: TrustedPeer) -> String {
        guard t.lastSeen > 0 else { return "Paired" }
        return "Last connected " + Date(timeIntervalSince1970: TimeInterval(t.lastSeen)).formatted(.relative(presentation: .named))
    }
}

private struct Detail: View {
    let title: String
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.system(size: 12.5, weight: .medium))
            Text(text).font(.system(size: 11)).foregroundStyle(.secondary)
        }
    }
}

/// A big two-line button: what it does, and what happens.
private struct Action: View {
    let title: String
    let detail: String
    let symbol: String
    let prominent: Bool
    let disabled: Bool
    let action: () -> Void
    @State private var hovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: symbol).font(.system(size: 17, weight: .medium)).frame(width: 22)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.system(size: 14, weight: .semibold))
                    Text(detail).font(.system(size: 11)).opacity(0.8)
                }
                Spacer(minLength: 0)
                Image(systemName: "arrow.right").font(.system(size: 12, weight: .semibold)).opacity(0.7)
            }
            .foregroundStyle(prominent ? Color.black : Color.primary)
            .padding(.horizontal, 14)
            .frame(height: 58)
            .contentShape(Rectangle())
            .background(RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(prominent ? Color.white.opacity(0.94) : Color.white.opacity(hovered ? 0.12 : 0.07)))
        }
        .buttonStyle(.plain)
        .onHover { hovered = $0 }
        .disabled(disabled)
        .opacity(disabled ? 0.45 : 1)
    }
}

// MARK: Live session

/// What's live, in numbers big enough to read from across the room.
private struct SessionStrip: View {
    @ObservedObject var model: AppModel

    var body: some View {
        HStack(alignment: .center, spacing: 28) {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    LiveDot()
                    Text("LIVE").font(.system(size: 10.5, weight: .bold)).tracking(1.4).foregroundStyle(Color.annexLive)
                }
                Text(title).font(.system(size: 18, weight: .semibold, design: .rounded)).lineLimit(1)
                Text(subtitle).font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            ForEach(stats, id: \.label) { s in
                VStack(alignment: .leading, spacing: 0) {
                    HStack(alignment: .firstTextBaseline, spacing: 3) {
                        Text(s.value).font(.system(size: 40, weight: .light, design: .rounded)).monospacedDigit()
                        Text(s.unit).font(.system(size: 13, weight: .medium)).foregroundStyle(.secondary)
                    }
                    Text(s.label).font(.system(size: 11)).foregroundStyle(.secondary)
                }
            }
            Button(role: .destructive) { stop() } label: { Text(model.isViewing ? "Disconnect" : "Stop") }
                .secondaryGlassButton()
        }
        .padding(.horizontal, 22)
        .padding(.vertical, 16)
        .glassPanel(radius: 26)
    }

    private struct Stat { let value: String; let unit: String; let label: String }

    private var stats: [Stat] {
        if model.isViewing, let l = model.latency {
            return [Stat(value: String(format: "%.0f", l.p50Ms), unit: "ms", label: "Delay"),
                    Stat(value: String(format: "%.0f", l.fps), unit: "fps", label: "Smoothness")]
        }
        if let s = model.hostStatus.stream {
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

    private var subtitle: String {
        if model.isViewing {
            if model.controllingHost { return "You're in control · ⌃⌥⇧Q to leave" }
            return model.onDirectWiFi ? "On direct Wi-Fi" : "Encrypted end to end"
        }
        if !model.hostStatus.extendViewers.isEmpty { return "Arrange it in System Settings → Displays" }
        return "New computers can't find this Mac during a session"
    }

    private func stop() {
        if model.isViewing { model.disconnectViewer() } else { model.setHosting(false) }
    }
}
