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
    /// Empty = automatic: the radar, or a list once it would get crowded. Set once the user picks.
    @AppStorage("nearbyLayout") private var layoutChoice: String = ""
    @FocusState private var focused: Bool

    /// The radar holds this many computers before it switches to a list by itself.
    static let radarLimit = 8

    private var visible: [PeerInfo] {
        showNotSharing ? model.discovered : model.discovered.filter(\.isSharing)
    }

    private var notSharingCount: Int { model.discovered.filter { !$0.isSharing }.count }

    private var layout: NearbyLayout {
        NearbyLayout(rawValue: layoutChoice) ?? (visible.count >= Self.radarLimit ? .list : .radar)
    }

    private var selected: PeerInfo? {
        guard let id = selectedID else { return nil }
        return model.discovered.first { $0.id == id }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            NearbyHeader(model: model, layout: layout, otherWays: { showOtherWays = true },
                         setLayout: { layoutChoice = $0.rawValue })
            UpdateBanner(updater: model.updater)
            HStack(alignment: .top, spacing: 18) {
                ZStack(alignment: .bottomLeading) {
                    Group {
                        switch layout {
                        case .radar:
                            Radar(model: model, peers: visible, selectedID: $selectedID,
                                  connect: connect, otherWays: { showOtherWays = true })
                        case .list:
                            NearbyList(model: model, peers: visible, selectedID: $selectedID, connect: connect)
                        }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    Footnote(layout: layout, hidden: showNotSharing ? 0 : notSharingCount,
                             showing: showNotSharing && notSharingCount > 0) {
                        withAnimation(.spring(response: 0.5, dampingFraction: 0.85)) { showNotSharing.toggle() }
                    }
                }
                if let peer = selected {
                    PeerCard(model: model, peer: peer) { selectedID = nil }
                        .frame(width: 300)
                        .transition(.move(edge: .trailing).combined(with: .opacity))
                }
            }
            .animation(.spring(response: 0.45, dampingFraction: 0.85), value: selectedID)
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
        .animation(.spring(response: 0.5, dampingFraction: 0.86), value: layout)
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
        .onKeyPress(keys: [.leftArrow, .upArrow]) { _ in move(by: -1) }
        .onKeyPress(keys: [.rightArrow, .downArrow]) { _ in move(by: 1) }
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

enum NearbyLayout: String {
    case radar, list
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
    let layout: NearbyLayout
    let otherWays: () -> Void
    let setLayout: (NearbyLayout) -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Nearby").font(.system(size: 28, weight: .semibold, design: .rounded))
                Text(hint).font(.system(size: 13)).foregroundStyle(.secondary)
            }
            Spacer(minLength: 12)
            GlassGroup(spacing: 8) {
                HStack(spacing: 8) {
                    Picker("Layout", selection: Binding(get: { layout }, set: setLayout)) {
                        Image(systemName: "circle.circle").help("Radar").tag(NearbyLayout.radar)
                        Image(systemName: "list.bullet").help("List").tag(NearbyLayout.list)
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .fixedSize()
                    .accessibilityLabel("Layout")
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

    private var hint: String {
        if model.discovered.isEmpty { return "Looking on your network, over cables and Mac to Mac." }
        return "Click a computer for options. Double-click to connect."
    }
}

/// Bottom-left of the radar or list: what the rings mean, and computers that aren't sharing.
private struct Footnote: View {
    let layout: NearbyLayout
    let hidden: Int
    let showing: Bool
    let toggle: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            if hidden > 0 || showing {
                Button(action: toggle) {
                    Text(showing ? "Hide computers that aren't sharing" : "+\(hidden) not sharing")
                        .font(.system(size: 11.5, weight: .medium))
                        .padding(.horizontal, 12)
                        .frame(height: 26)
                        .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .glassCapsule(interactive: true)
            }
            if layout == .radar {
                Text(showing ? "Inner ring: paired · Middle: new, PIN needed · Outer: not sharing"
                             : "Inner ring: paired · Outer: new, PIN needed")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: Radar

/// Computers laid out around this Mac: paired on the inner ring, new ones (PIN needed) on the
/// middle, and the ones that aren't sharing, when shown, on the outer.
private struct Radar: View {
    @ObservedObject var model: AppModel
    let peers: [PeerInfo]
    @Binding var selectedID: String?
    let connect: (PeerInfo) -> Void
    let otherWays: () -> Void

    var body: some View {
        GeometryReader { geo in
            let size = geo.size
            let center = CGPoint(x: size.width / 2, y: size.height / 2)
            let unit = min(size.width, size.height) / 2
            ZStack {
                Rings(searching: peers.isEmpty, live: model.isHosting || model.isViewing)
                Links(center: center, points: peers.enumerated().filter { $0.element.isSharing }
                    .map { position(of: $0.offset, peer: $0.element, center: center, unit: unit) })
                ThisMacNode(model: model) { model.pane = .share }
                    .position(center)
                ForEach(Array(peers.enumerated()), id: \.element.id) { i, peer in
                    PeerNode(peer: peer, paired: model.isPaired(peer), selected: selectedID == peer.id,
                             select: { selectedID = selectedID == peer.id ? nil : peer.id },
                             connect: { connect(peer) })
                        .position(position(of: i, peer: peer, center: center, unit: unit))
                        .transition(.scale.combined(with: .opacity))
                }
                if peers.isEmpty {
                    EmptyHint(model: model, otherWays: otherWays)
                        .position(x: center.x, y: min(size.height - 70, center.y + unit * 0.74))
                }
            }
            .animation(.spring(response: 0.6, dampingFraction: 0.8), value: peers.map(\.id))
        }
        .frame(minHeight: 320)
    }

    private func ring(for peer: PeerInfo) -> CGFloat {
        guard peer.isSharing else { return 0.86 }
        return model.isPaired(peer) ? 0.46 : 0.66
    }

    /// Spread evenly, starting top-right, so nodes never sit on the header or each other.
    private func position(of i: Int, peer: PeerInfo, center: CGPoint, unit: CGFloat) -> CGPoint {
        let angle = (-60 + Double(i) * 360 / Double(max(peers.count, 1))) * .pi / 180
        let r = unit * ring(for: peer)
        // Keep whole nodes (and their labels) inside the radar, however narrow it gets.
        let x = center.x + CGFloat(cos(angle)) * r * 1.25
        return CGPoint(x: min(max(x, 80), center.x * 2 - 80), y: center.y + CGFloat(sin(angle)) * r * 0.82)
    }
}

/// Dotted rings, and a slow sweep while nothing has been found.
private struct Rings: View {
    var searching: Bool
    var live: Bool

    var body: some View {
        TimelineView(.animation(minimumInterval: 1 / 30, paused: !searching)) { context in
            let t = context.date.timeIntervalSinceReferenceDate
            Canvas { ctx, size in
                let c = CGPoint(x: size.width / 2, y: size.height / 2)
                let unit = min(size.width, size.height) / 2
                for (k, f) in [0.46, 0.66, 0.86].enumerated() {
                    let rx = unit * f * 1.25, ry = unit * f * 0.82
                    let dots = 72 + k * 24
                    for d in 0..<dots {
                        let a = Double(d) / Double(dots) * 2 * .pi
                        let p = CGPoint(x: c.x + rx * CGFloat(cos(a)), y: c.y + ry * CGFloat(sin(a)))
                        ctx.fill(Path(ellipseIn: CGRect(x: p.x - 1, y: p.y - 1, width: 2, height: 2)),
                                 with: .color(.primary.opacity(0.22 - Double(k) * 0.05)))
                    }
                }
                if searching {
                    // A soft wedge of light going round, like a radar looking.
                    let a = (t.truncatingRemainder(dividingBy: 4) / 4) * 2 * .pi
                    var wedge = Path()
                    wedge.move(to: c)
                    wedge.addArc(center: c, radius: unit * 1.1, startAngle: .radians(a - 0.5), endAngle: .radians(a), clockwise: false)
                    wedge.closeSubpath()
                    ctx.fill(wedge, with: .linearGradient(Gradient(colors: [.clear, Color.white.opacity(0.10)]),
                                                         startPoint: CGPoint(x: c.x + unit * CGFloat(cos(a - 0.5)), y: c.y + unit * CGFloat(sin(a - 0.5))),
                                                         endPoint: CGPoint(x: c.x + unit * CGFloat(cos(a)), y: c.y + unit * CGFloat(sin(a)))))
                }
            }
        }
        .accessibilityHidden(true)
    }
}

/// Faint lines from this Mac to every computer that's sharing.
private struct Links: View {
    let center: CGPoint
    let points: [CGPoint]

    var body: some View {
        Canvas { ctx, _ in
            for p in points {
                var line = Path()
                line.move(to: center)
                line.addLine(to: p)
                ctx.stroke(line, with: .linearGradient(Gradient(colors: [Color.white.opacity(0.45), Color.white.opacity(0.12)]),
                                                       startPoint: center, endPoint: p),
                           style: StrokeStyle(lineWidth: 1.2, dash: [3, 5]))
            }
        }
        .accessibilityHidden(true)
    }
}

private struct ThisMacNode: View {
    @ObservedObject var model: AppModel
    let open: () -> Void

    var body: some View {
        Button(action: open) { node }
            .buttonStyle(.plain)
            .help(model.isHosting ? "This Mac is sharing" : "Share this Mac")
    }

    private var node: some View {
        VStack(spacing: 8) {
            Image(systemName: "laptopcomputer")
                .font(.system(size: 30, weight: .regular))
                .foregroundStyle(.primary)
                .frame(width: 92, height: 92)
                .glassCircle(tint: model.isHosting ? Color.white.opacity(0.14) : nil)
                .shadow(color: Color.white.opacity(model.isHosting ? 0.35 : 0), radius: 28)
            Text(model.isHosting ? "This Mac · sharing" : "This Mac").font(.system(size: 11.5, weight: .medium)).foregroundStyle(.secondary)
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}

/// Click selects, double-click connects. Not a Button: a Button would swallow the double-click.
private struct PeerNode: View {
    let peer: PeerInfo
    let paired: Bool
    let selected: Bool
    let select: () -> Void
    let connect: () -> Void
    @State private var hovered = false

    var body: some View {
        VStack(spacing: 7) {
            Image(systemName: Self.symbol(for: peer))
                .font(.system(size: 22))
                .foregroundStyle(selected ? Color.black : peer.isSharing ? Color.primary : Color.secondary)
                .frame(width: 64, height: 64)
                .glassCircle(tint: selected ? Color.white.opacity(0.9) : nil)
                .overlay(alignment: .topTrailing) {
                    if peer.isSharing { LiveDot(size: 9).offset(x: -4, y: 4) }
                }
                .scaleEffect(hovered && !selected ? 1.06 : 1)
            VStack(spacing: 3) {
                Text(peer.name).font(.system(size: 12.5, weight: .semibold)).lineLimit(1)
                if peer.isSharing {
                    LinkChip(kind: LinkKind(peer))
                } else {
                    Text("Not sharing").font(.system(size: 10.5)).foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: 150)
        }
        .contentShape(Rectangle())
        .onTapGesture(count: 2, perform: connect)
        .onTapGesture(perform: select)
        .onHover { h in withAnimation(.spring(response: 0.3)) { hovered = h } }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(peer.name), \(Self.status(peer, paired: paired))")
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction(.default, select)
        .accessibilityAction(named: "Connect", connect)
        .help(Self.status(peer, paired: paired))
    }

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

private struct EmptyHint: View {
    @ObservedObject var model: AppModel
    let otherWays: () -> Void

    var body: some View {
        VStack(spacing: 10) {
            Text("Looking for computers…").font(.system(size: 13, weight: .semibold))
            Text("Open Annex on the other computer and turn on sharing.")
                .font(.system(size: 12)).foregroundStyle(.secondary)
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
        .multilineTextAlignment(.center)
    }
}

// MARK: List

/// The same computers as rows: for when the radar would be crowded, or by choice.
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
                if peers.isEmpty {
                    Text("Looking for computers…")
                        .font(.system(size: 13)).foregroundStyle(.secondary)
                        .padding(.top, 40)
                }
            }
            .padding(.bottom, 44)
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
            Image(systemName: PeerNode.symbol(for: peer))
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
        .accessibilityLabel("\(peer.name), \(PeerNode.status(peer, paired: paired))")
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction(.default, select)
        .accessibilityAction(named: "Connect", connect)
    }
}

// MARK: Selected computer

/// Floats over the radar, so it's glass; everything inside it is flat (no glass on glass).
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
                Image(systemName: PeerNode.symbol(for: peer))
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
