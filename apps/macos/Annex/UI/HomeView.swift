import SwiftUI

/// Nearby: this Mac in the middle, the computers around it, and what's live. The one screen most
/// people need: pick a computer, then Extend or Show Its Screen.
struct HomeView: View {
    @ObservedObject var model: AppModel
    @State private var selectedID: String? = {
        #if DEBUG
        // Screenshots of the selected-computer card (`ANNEX_PREVIEW_SELECT=<instance>`).
        return ProcessInfo.processInfo.environment["ANNEX_PREVIEW_SELECT"]
        #else
        return nil
        #endif
    }()
    @State private var showOtherWays: Bool = {
        #if DEBUG
        return ProcessInfo.processInfo.environment["ANNEX_PREVIEW_SHEET"] != nil
        #else
        return false
        #endif
    }()

    private var selected: PeerInfo? {
        guard let id = selectedID else { return nil }
        return model.discovered.first { $0.id == id }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            NearbyHeader(model: model) { showOtherWays = true }
            UpdateBanner(updater: model.updater)
            HStack(alignment: .top, spacing: 18) {
                Radar(model: model, selectedID: $selectedID) { showOtherWays = true }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
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
        .sheet(isPresented: $showOtherWays) { OtherWaysSheet(model: model) }
        .onChange(of: model.discovered) { _, peers in
            if let id = selectedID, !peers.contains(where: { $0.id == id }) { selectedID = nil }
        }
    }
}

// MARK: Header

/// Content, not chrome: a plain title. The two controls on its right float, so they're glass.
private struct NearbyHeader: View {
    @ObservedObject var model: AppModel
    let otherWays: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Nearby").font(.system(size: 28, weight: .semibold, design: .rounded))
                Text(hint).font(.system(size: 13)).foregroundStyle(.secondary)
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

    private var hint: String {
        if model.discovered.isEmpty { return "Looking on your network, over cables and Mac to Mac." }
        return "Choose a computer to extend onto, or to show its screen here."
    }
}

// MARK: Radar

/// Computers laid out around this Mac: paired and sharing on the inner ring, sharing on the
/// middle, the rest (open but not sharing) on the outer.
private struct Radar: View {
    @ObservedObject var model: AppModel
    @Binding var selectedID: String?
    let otherWays: () -> Void

    var body: some View {
        GeometryReader { geo in
            let size = geo.size
            let center = CGPoint(x: size.width / 2, y: size.height / 2)
            let unit = min(size.width, size.height) / 2
            let peers = model.discovered
            ZStack {
                Rings(searching: peers.isEmpty, live: model.isHosting || model.isViewing)
                Links(center: center, points: peers.enumerated().filter { $0.element.isSharing }
                    .map { position(of: $0.offset, count: peers.count, peer: $0.element, center: center, unit: unit) })
                ThisMacNode(model: model) { model.pane = .share }
                    .position(center)
                ForEach(Array(peers.enumerated()), id: \.element.id) { i, peer in
                    PeerNode(peer: peer, paired: model.isPaired(peer), selected: selectedID == peer.id) {
                        selectedID = selectedID == peer.id ? nil : peer.id
                    }
                    .position(position(of: i, count: peers.count, peer: peer, center: center, unit: unit))
                    .transition(.scale.combined(with: .opacity))
                }
                if peers.isEmpty {
                    EmptyHint(model: model, otherWays: otherWays)
                        .position(x: center.x, y: min(size.height - 40, center.y + unit * 0.78))
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
    private func position(of i: Int, count: Int, peer: PeerInfo, center: CGPoint, unit: CGFloat) -> CGPoint {
        let angle = (-60 + Double(i) * 360 / Double(max(count, 1))) * .pi / 180
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
                    ctx.scaleBy(x: 1, y: 1)
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

private struct PeerNode: View {
    let peer: PeerInfo
    let paired: Bool
    let selected: Bool
    let action: () -> Void
    @State private var hovered = false

    var body: some View {
        Button(action: action) {
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
                VStack(spacing: 1) {
                    Text(peer.name).font(.system(size: 12.5, weight: .semibold)).lineLimit(1)
                    Text(subtitle).font(.system(size: 10.5)).foregroundStyle(.secondary).lineLimit(1)
                }
                .frame(maxWidth: 140)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { h in withAnimation(.spring(response: 0.3)) { hovered = h } }
        .accessibilityLabel("\(peer.name), \(subtitle)")
    }

    private var subtitle: String {
        guard peer.isSharing else { return "Not sharing" }
        return paired ? "Paired · sharing" : "Sharing · PIN needed"
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

// MARK: Selected computer

private struct PeerCard: View {
    @ObservedObject var model: AppModel
    let peer: PeerInfo
    let close: () -> Void

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
                    .accessibilityLabel("Close")
            }
            VStack(alignment: .leading, spacing: 4) {
                Text(peer.name).font(.system(size: 22, weight: .semibold, design: .rounded)).lineLimit(2)
                Text(model.isPaired(peer) ? "Paired · connects without a PIN" : "New · asks for a PIN the first time")
                    .font(.system(size: 12)).foregroundStyle(.secondary)
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
            Toggle(isOn: $model.useDirectWiFi) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Direct Wi-Fi first").font(.system(size: 12.5, weight: .medium))
                    Text("Join a Windows PC's hotspot. Less lag.").font(.system(size: 11)).foregroundStyle(.secondary)
                }
            }
            .toggleStyle(.switch)
            .controlSize(.small)
            .disabled(model.isViewing)
            if model.settings.showTechnicalDetails {
                Text(peer.addresses.map { "\($0):\(peer.port)" }.joined(separator: "\n"))
                    .font(.system(size: 11, design: .monospaced)).foregroundStyle(.secondary).textSelection(.enabled)
            }
            Spacer(minLength: 0)
        }
        .padding(20)
        .frame(maxHeight: .infinity, alignment: .top)
        .glassPanel(radius: 26)
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
