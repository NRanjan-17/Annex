import AppKit
import SwiftUI

/// The notch as a drop zone: while another computer is connected, dragging files anywhere makes
/// the notch glow; bringing them close opens a tray, and dropping sends them. Files arriving from
/// the other computer show there the same way. On a Mac without a notch, the same shape hangs
/// from the middle of the menu bar.
@MainActor
final class NotchDrop: ObservableObject {
    enum Phase: Equatable {
        case hidden
        /// Files are being dragged somewhere on this Mac.
        case glowing
        /// They're close: the tray is open.
        case open
        /// Files going or coming; the list shows them.
        case transfers
    }

    struct Item: Identifiable, Equatable {
        let id: String
        /// The session it belongs to (as the transfer events report it) and the core's id for it.
        let session: UInt64
        let transferId: UInt64
        /// It goes over the viewer connection (this Mac viewing another), not this Mac's sharing.
        let viewer: Bool
        let incoming: Bool
        let name: String
        let size: UInt64
        var done: UInt64 = 0
        /// nil while it's going.
        var ok: Bool?
        var path: String?
        var error: String?
        /// Sending is paused (outgoing only).
        var paused = false
        /// Unsent (outgoing) or stopped (incoming) from the tray.
        var cancelled = false
    }

    @Published private(set) var phase: Phase = .hidden
    @Published private(set) var items: [Item] = []
    /// Who files go to: "MacBook Air".
    @Published private(set) var peerName = ""
    @Published private(set) var notchSize = CGSize(width: 200, height: 32)
    /// How close dragged files are to the notch: 1 at it, 0 at 400 pt or more.
    @Published private(set) var nearness: Double = 0

    /// Dropped files (folders already expanded into their files).
    var onDrop: (([URL]) -> Void)?
    /// Pause (true) or resume (false) a file being sent; returns whether the core took it.
    var onPause: ((Item, Bool) -> Bool)?
    /// Unsend a file being sent, or stop one arriving.
    var onCancel: ((Item) -> Void)?

    private var panel: NSPanel?
    private var monitors: [Any] = []
    private let dragBoard = NSPasteboard(name: .drag)
    private var dragCountAtMouseDown = 0
    private var dragging = false
    /// Polls the pointer while the mouse button is held: during a drag-and-drop session macOS
    /// stops sending leftMouseDragged to global monitors.
    private var pollTimer: Timer?
    private var hideWork: DispatchWorkItem?
    private var canSend = false

    /// The panel is this big; the shape inside grows from the notch.
    static let panelSize = CGSize(width: 440, height: 220)

    // MARK: State from the app

    /// Files can go to `peer` now (a session with control either way). Nil: nothing connected.
    /// `why`: what decided it, for the log.
    func setTarget(_ peer: String?, why: String) {
        let can = peer != nil
        if let peer { peerName = peer }
        guard can != canSend else { return }
        Annex.log(.info, "[drop] " + (peer.map { "files can go to \($0) (\(why))" } ?? "no one to send files to (\(why))"))
        canSend = can
        if can { startWatching() } else { stopWatching(); if phase != .transfers { hide(why: "no one to send to") } }
    }

    func transferStarted(session: UInt64, id: UInt64, incoming: Bool, name: String, size: UInt64, viewer: Bool) {
        let key = Self.key(session, id, incoming)
        items.removeAll { $0.id == key }
        items.append(Item(id: key, session: session, transferId: id, viewer: viewer, incoming: incoming, name: name, size: size))
        // Only the latest few; finished ones go first.
        while items.count > 4, let i = items.firstIndex(where: { $0.ok != nil }) { items.remove(at: i) }
        show(.transfers, why: "\(incoming ? "receiving" : "sending") \(name)")
    }

    func transferProgress(session: UInt64, id: UInt64, incoming: Bool, done: UInt64) {
        Annex.log(.trace, "[transfer] \(incoming ? "in" : "out") \(session)/\(id): \(done) bytes")
        if let i = items.firstIndex(where: { $0.id == Self.key(session, id, incoming) }) { items[i].done = done }
    }

    func transferEnded(session: UInt64, id: UInt64, incoming: Bool, ok: Bool, path: String?, error: String?) {
        guard let i = items.firstIndex(where: { $0.id == Self.key(session, id, incoming) }) else {
            return Annex.log(.debug, "[drop] transfer \(session)/\(id) ended but isn't in the tray")
        }
        items[i].ok = ok
        items[i].path = path
        items[i].error = error
        items[i].paused = false
        if ok { items[i].done = items[i].size }
        if items.allSatisfy({ $0.ok != nil }) { hideLater(after: 5) }
    }

    /// Pause or resume a file being sent.
    func togglePause(_ item: Item) {
        guard let i = items.firstIndex(where: { $0.id == item.id }), items[i].ok == nil, !items[i].incoming else { return }
        let pause = !items[i].paused
        Annex.log(.info, "[drop] \(pause ? "pause" : "resume") \(item.id)")
        guard onPause?(items[i], pause) == true else {
            return Annex.log(.warn, "[drop] couldn't \(pause ? "pause" : "resume") \(item.id): it isn't being sent")
        }
        items[i].paused = pause
        if pause { hideWork?.cancel() }
    }

    /// Unsend a file being sent (the other side deletes what arrived), or stop one arriving.
    func unsend(_ item: Item) {
        guard let i = items.firstIndex(where: { $0.id == item.id }), items[i].ok == nil else { return }
        Annex.log(.info, "[drop] \(item.incoming ? "stop" : "unsend") \(item.id)")
        onCancel?(items[i])
        // The core reports the end too; mark it now so the row doesn't wait on it.
        items[i].cancelled = true
        items[i].paused = false
        items[i].ok = false
        if items.allSatisfy({ $0.ok != nil }) { hideLater(after: 3) }
    }

    func reveal(_ item: Item) {
        guard let path = item.path else { return }
        Annex.log(.info, "[drop] Show pressed: \(path)")
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }

    func dismiss() {
        items.removeAll { $0.ok != nil }
        if items.isEmpty { hide(why: "dismissed") }
    }

    private static func key(_ session: UInt64, _ id: UInt64, _ incoming: Bool) -> String { "\(session)-\(id)-\(incoming)" }

    // MARK: Watching drags

    private func startWatching() {
        guard monitors.isEmpty else { return }
        // Mouse monitors need no permission; they see drags that start in other apps.
        let down = NSEvent.addGlobalMonitorForEvents(matching: .leftMouseDown) { [weak self] _ in
            MainActor.assumeIsolated { self?.mouseWentDown() }
        }
        // Backup: a drag-and-drop session usually swallows this, so the poll notices the release.
        let up = NSEvent.addGlobalMonitorForEvents(matching: .leftMouseUp) { [weak self] _ in
            MainActor.assumeIsolated { self?.mouseReleased() }
        }
        monitors = [down, up].compactMap { $0 }
        Annex.log(monitors.count == 2 ? .debug : .warn, "[drop] watching drags (\(monitors.count) of 2 mouse monitors)")
    }

    private func stopWatching() {
        if !monitors.isEmpty { Annex.log(.debug, "[drop] stopped watching drags") }
        monitors.forEach(NSEvent.removeMonitor)
        monitors.removeAll()
        stopPolling()
        dragging = false
        nearness = 0
    }

    private func mouseWentDown() {
        dragCountAtMouseDown = dragBoard.changeCount
        guard canSend, pollTimer == nil else { return }
        // ~30 a second, only while the button is held; .common so it keeps firing during the drag.
        let t = Timer(timeInterval: 1.0 / 30, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.poll() }
        }
        RunLoop.main.add(t, forMode: .common)
        pollTimer = t
    }

    private func poll() {
        guard NSEvent.pressedMouseButtons & 1 != 0 else { return mouseReleased() }
        dragged()
    }

    private func mouseReleased() {
        stopPolling()
        dragEnded()
    }

    private func stopPolling() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    private func dragged() {
        if !dragging {
            // A drag of files started (the drag pasteboard changed since the mouse went down).
            guard canSend, dragBoard.changeCount != dragCountAtMouseDown,
                  dragBoard.types?.contains(.fileURL) == true else { return }
            dragging = true
            Annex.log(.info, "[drop] a file drag started")
            if phase != .transfers { show(.glowing, why: "file drag started") }
        }
        guard phase == .glowing || phase == .open else { return }
        let distance = distanceToNotch(NSEvent.mouseLocation)
        let close = Double(max(0, 1 - distance / 400))
        if abs(close - nearness) > 0.01 || (close == 0) != (nearness == 0) { nearness = close }
        let near = distance < 140
        if near && phase == .glowing {
            show(.open, why: "drag \(Int(distance)) pt from the notch")
        } else if !near && phase == .open {
            show(.glowing, why: "drag moved away, \(Int(distance)) pt from the notch")
        }
    }

    private func dragEnded() {
        guard dragging else { return }
        dragging = false
        nearness = 0
        // A drop on the tray arrives just after the mouse goes up.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in
            guard let self, !self.dragging, self.phase == .glowing || self.phase == .open else { return }
            self.hide(why: "drag ended without a drop")
        }
    }

    /// Dragged files reached the tray itself.
    fileprivate func dragEnteredTray() {
        if phase == .glowing || phase == .hidden { show(.open, why: "drag reached the tray") }
    }

    /// Files dropped on the tray or the viewer window.
    func dropped(_ urls: [URL]) {
        dragging = false
        let files = urls.flatMap(Self.files(in:))
        Annex.log(.info, "[drop] \(urls.count) item(s) dropped, \(files.count) file(s) to send")
        guard !files.isEmpty else { hide(why: "nothing to send in the drop"); return }
        show(.transfers, why: "files dropped")
        onDrop?(files)
    }

    /// A file, or every file inside a folder (hidden ones skipped).
    private static func files(in url: URL) -> [URL] {
        var isDir: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir) else {
            Annex.log(.warn, "[drop] dropped item not found: \(url.path)")
            return []
        }
        guard isDir.boolValue else { return [url] }
        let e = FileManager.default.enumerator(at: url, includingPropertiesForKeys: [.isRegularFileKey],
                                               options: [.skipsHiddenFiles, .skipsPackageDescendants])
        return (e?.allObjects as? [URL] ?? []).filter { (try? $0.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true }
    }

    // MARK: The panel

    private static var screen: NSScreen? {
        NSScreen.screens.first { $0.safeAreaInsets.top > 0 } ?? NSScreen.main
    }

    /// The notch in screen coordinates; on a Mac without one, a pill-sized area in the menu bar.
    private static func notchRect(on screen: NSScreen) -> NSRect {
        let f = screen.frame
        if let l = screen.auxiliaryTopLeftArea, let r = screen.auxiliaryTopRightArea, screen.safeAreaInsets.top > 0 {
            let h = screen.safeAreaInsets.top
            return NSRect(x: f.minX + l.width, y: f.maxY - h, width: f.width - l.width - r.width, height: h)
        }
        let h = NSStatusBar.system.thickness
        return NSRect(x: f.midX - 100, y: f.maxY - h, width: 200, height: h)
    }

    private func distanceToNotch(_ p: NSPoint) -> CGFloat {
        guard let screen = Self.screen else { return .infinity }
        let n = Self.notchRect(on: screen)
        let dx = max(n.minX - p.x, 0, p.x - n.maxX), dy = max(n.minY - p.y, 0, p.y - n.maxY)
        return (dx * dx + dy * dy).squareRoot()
    }

    private func show(_ phase: Phase, why: String) {
        if phase != self.phase { Annex.log(.debug, "[drop] \(self.phase) → \(phase): \(why)") }
        hideWork?.cancel()
        let panel = self.panel ?? makePanel()
        if let screen = Self.screen {
            let n = Self.notchRect(on: screen)
            notchSize = n.size
            let s = Self.panelSize
            panel.setFrame(NSRect(x: n.midX - s.width / 2, y: screen.frame.maxY - s.height, width: s.width, height: s.height),
                           display: false)
        }
        panel.ignoresMouseEvents = false
        panel.orderFrontRegardless()
        // Pops out of the notch with a little overshoot.
        withAnimation(.spring(response: 0.5, dampingFraction: 0.66)) { self.phase = phase }
    }

    private func hide(why: String) {
        if phase != .hidden { Annex.log(.debug, "[drop] \(phase) → hidden: \(why)") }
        hideWork?.cancel()
        // Pops back in, quicker and without the bounce.
        withAnimation(.spring(response: 0.34, dampingFraction: 0.86)) { phase = .hidden; nearness = 0 }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.45) { [weak self] in
            guard let self, self.phase == .hidden else { return }
            self.panel?.ignoresMouseEvents = true
            self.panel?.orderOut(nil)
            self.items.removeAll { $0.ok != nil }
        }
    }

    private func hideLater(after seconds: Double) {
        hideWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.phase == .transfers, !self.dragging, !self.hovering,
                      !self.items.contains(where: { $0.paused }) else { return }
                self.hide(why: "transfers done")
            }
        }
        hideWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds, execute: work)
    }

    /// The pointer is over the shape: don't hide what it's reading.
    fileprivate var hovering = false {
        didSet { if !hovering, phase == .transfers, items.allSatisfy({ $0.ok != nil }) { hideLater(after: 2) } }
    }

    private func makePanel() -> NSPanel {
        let p = NSPanel(contentRect: NSRect(origin: .zero, size: Self.panelSize),
                        styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        p.level = NSWindow.Level(rawValue: NSWindow.Level.statusBar.rawValue + 1)
        p.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary, .ignoresCycle]
        p.isOpaque = false
        p.backgroundColor = .clear
        p.hasShadow = false
        p.isMovable = false
        p.hidesOnDeactivate = false
        p.becomesKeyOnlyIfNeeded = true
        let drop = TrayDropView(owner: self)
        let host = NSHostingView(rootView: NotchDropView(model: self))
        // The panel sits over the notch, whose safe area would push the island down and leave a
        // gap under the camera. The island belongs right up against it.
        host.safeAreaRegions = []
        host.translatesAutoresizingMaskIntoConstraints = false
        drop.addSubview(host)
        NSLayoutConstraint.activate([
            host.leadingAnchor.constraint(equalTo: drop.leadingAnchor), host.trailingAnchor.constraint(equalTo: drop.trailingAnchor),
            host.topAnchor.constraint(equalTo: drop.topAnchor), host.bottomAnchor.constraint(equalTo: drop.bottomAnchor),
        ])
        p.contentView = drop
        panel = p
        return p
    }

    fileprivate func setHovering(_ on: Bool) { hovering = on }
}

/// Takes the drop for the whole panel (the SwiftUI view inside doesn't register for files).
private final class TrayDropView: NSView {
    private weak var owner: NotchDrop?

    init(owner: NotchDrop) {
        self.owner = owner
        super.init(frame: .zero)
        registerForDraggedTypes([.fileURL])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        owner?.dragEnteredTray()
        return .copy
    }

    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation { .copy }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        let urls = sender.draggingPasteboard.readObjects(forClasses: [NSURL.self],
                                                         options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        guard !urls.isEmpty else { return false }
        owner?.dropped(urls)
        return true
    }
}

// MARK: - The look

/// The notch's outline: flat top, rounded bottom corners, flared top corners so it melts into
/// the menu bar like the hardware notch.
private struct NotchShape: Shape {
    var bottomRadius: CGFloat
    /// Leaves out the flat top edge, for outlines: a line there would split the island from the
    /// camera housing it hangs from.
    var openTop = false
    var animatableData: CGFloat {
        get { bottomRadius }
        set { bottomRadius = newValue }
    }

    func path(in r: CGRect) -> Path {
        let flare: CGFloat = 6
        let br = min(bottomRadius, r.height / 2, r.width / 2)
        var p = Path()
        if openTop {
            p.move(to: CGPoint(x: r.maxX + flare, y: r.minY))
        } else {
            p.move(to: CGPoint(x: r.minX - flare, y: r.minY))
            p.addLine(to: CGPoint(x: r.maxX + flare, y: r.minY))
        }
        p.addQuadCurve(to: CGPoint(x: r.maxX, y: r.minY + flare), control: CGPoint(x: r.maxX, y: r.minY))
        p.addLine(to: CGPoint(x: r.maxX, y: r.maxY - br))
        p.addQuadCurve(to: CGPoint(x: r.maxX - br, y: r.maxY), control: CGPoint(x: r.maxX, y: r.maxY))
        p.addLine(to: CGPoint(x: r.minX + br, y: r.maxY))
        p.addQuadCurve(to: CGPoint(x: r.minX, y: r.maxY - br), control: CGPoint(x: r.minX, y: r.maxY))
        p.addLine(to: CGPoint(x: r.minX, y: r.minY + flare))
        p.addQuadCurve(to: CGPoint(x: r.minX - flare, y: r.minY), control: CGPoint(x: r.minX, y: r.minY))
        if !openTop { p.closeSubpath() }
        return p
    }
}

private struct NotchDropView: View {
    @ObservedObject var model: NotchDrop
    @State private var spin = false

    private var size: CGSize {
        let n = model.notchSize
        switch model.phase {
        case .hidden: return n
        // Grows a little as the files come closer.
        case .glowing: return CGSize(width: n.width + 56 + 36 * near, height: n.height + 14 + 8 * near)
        case .open: return CGSize(width: 380, height: n.height + 118)
        case .transfers: return CGSize(width: 380, height: n.height + 20 + CGFloat(max(model.items.count, 1)) * 44)
        }
    }

    /// Bright silver flowing round the edge.
    private func glow(_ turn: Bool) -> AngularGradient {
        AngularGradient(colors: [Color(white: 0.62), .white, Color(white: 0.84), .white, Color(white: 0.7), .white, Color(white: 0.62)],
                        center: .center, angle: .degrees(turn ? 360 : 0))
    }

    private var shown: Bool { model.phase != .hidden }
    private var near: CGFloat { CGFloat(model.nearness) }
    private var isGlowing: Bool { model.phase == .glowing || model.phase == .open }
    private var hasContent: Bool { model.phase == .open || model.phase == .transfers }

    var body: some View {
        let radius: CGFloat = model.phase == .glowing || model.phase == .hidden ? 10 : 22
        let shape = NotchShape(bottomRadius: radius)
        let edge = NotchShape(bottomRadius: radius, openTop: true)
        ZStack(alignment: .top) {
            shape.fill(.black)
            content
                .padding(.top, model.notchSize.height + 6)
                .padding(.horizontal, 18)
                .opacity(hasContent ? 1 : 0)
                .scaleEffect(hasContent ? 1 : 0.9, anchor: .top)
                .blur(radius: hasContent ? 0 : 6)
        }
        .frame(width: size.width, height: size.height)
        // The light: silver flowing round the edge while files are dragged. Behind the black, so
        // only the outer half and the halo show and the inside stays black.
        .background {
            // Brighter and wider as the files come closer.
            edge.stroke(glow(spin), lineWidth: 4 + 2 * near)
                .shadow(color: .white.opacity(0.6 + 0.35 * near), radius: 4 + 6 * near)
                .shadow(color: .white.opacity(0.3 + 0.4 * near), radius: 12 + 16 * near)
                .opacity(isGlowing ? 0.75 + 0.25 * near : 0)
        }
        .animation(.easeOut(duration: 0.15), value: model.nearness)
        .overlay { edge.stroke(.white.opacity(isGlowing ? 0.85 : 0), lineWidth: 1) }
        .scaleEffect(x: shown ? 1 : 0.8, y: shown ? 1 : 0.5, anchor: .top)
        .opacity(shown ? 1 : 0)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .ignoresSafeArea()
        .onHover { model.setHovering($0) }
        .onAppear {
            withAnimation(.linear(duration: 2.4).repeatForever(autoreverses: false)) { spin = true }
        }
    }

    @ViewBuilder private var content: some View {
        switch model.phase {
        case .open:
            VStack(spacing: 8) {
                Image(systemName: "arrow.down.doc.fill")
                    .font(.system(size: 30, weight: .medium))
                    .foregroundStyle(.white)
                    .symbolEffect(.bounce, options: .repeating)
                Text("Drop to send to \(model.peerName)")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(.white)
                Text("Same quality, byte for byte")
                    .font(.system(size: 11))
                    .foregroundStyle(.white.opacity(0.55))
            }
            .frame(maxWidth: .infinity)
        case .transfers:
            VStack(spacing: 6) {
                ForEach(model.items) {
                    ItemRow(item: $0, peer: model.peerName, reveal: { model.reveal($0) },
                            pause: { model.togglePause($0) }, unsend: { model.unsend($0) })
                }
            }
        default:
            EmptyView()
        }
    }
}

private struct ItemRow: View {
    let item: NotchDrop.Item
    let peer: String
    let reveal: (NotchDrop.Item) -> Void
    let pause: (NotchDrop.Item) -> Void
    let unsend: (NotchDrop.Item) -> Void

    private var fraction: Double { item.size == 0 ? 1 : min(Double(item.done) / Double(item.size), 1) }

    var body: some View {
        HStack(spacing: 10) {
            ZStack {
                Circle().stroke(.white.opacity(0.15), lineWidth: 3)
                Circle().trim(from: 0, to: item.ok == true ? 1 : fraction)
                    .stroke(item.ok == false ? Color.white.opacity(0.5) : Color.annexLive, style: StrokeStyle(lineWidth: 3, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                    .opacity(item.paused ? 0.35 : 1)
                Image(systemName: item.ok == true ? "checkmark" : item.ok == false ? "xmark"
                      : item.paused ? "pause" : item.incoming ? "arrow.down" : "arrow.up")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundStyle(.white)
                    .contentTransition(.symbolEffect(.replace))
            }
            .frame(width: 28, height: 28)
            VStack(alignment: .leading, spacing: 1) {
                Text(item.name).font(.system(size: 12, weight: .semibold)).foregroundStyle(.white).lineLimit(1).truncationMode(.middle)
                Text(status).font(.system(size: 11)).foregroundStyle(.white.opacity(0.6)).lineLimit(1)
            }
            Spacer(minLength: 0)
            if item.incoming, item.ok == true, item.path != nil {
                Button("Show") { reveal(item) }
                    .buttonStyle(.plain)
                    .font(.system(size: 11, weight: .semibold))
                    .padding(.horizontal, 10).padding(.vertical, 4)
                    .background(.white.opacity(0.15), in: Capsule())
                    .foregroundStyle(.white)
            }
            if item.ok == nil {
                if !item.incoming {
                    roundButton(item.paused ? "play.fill" : "pause.fill", help: item.paused ? "Resume" : "Pause") { pause(item) }
                }
                roundButton("xmark", help: item.incoming ? "Stop" : "Unsend") { unsend(item) }
            }
        }
        .frame(height: 38)
        .animation(.easeOut(duration: 0.2), value: item)
    }

    /// A small round icon button, styled like "Show".
    private func roundButton(_ symbol: String, help: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(.white)
                .frame(width: 24, height: 24)
                .background(.white.opacity(0.15), in: Circle())
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .help(help)
    }

    private var status: String {
        let total = ByteCountFormatter.string(fromByteCount: Int64(item.size), countStyle: .file)
        switch item.ok {
        case true?: return item.incoming ? "Received from \(peer) · \(total)" : "Sent to \(peer) · \(total)"
        case false?:
            if item.cancelled { return item.incoming ? "Stopped" : "Unsent" }
            return item.error ?? "Didn't arrive"
        case nil:
            let done = ByteCountFormatter.string(fromByteCount: Int64(item.done), countStyle: .file)
            if item.paused { return "Paused · \(done) of \(total)" }
            return "\(item.incoming ? "Receiving" : "Sending") \(done) of \(total)"
        }
    }
}
