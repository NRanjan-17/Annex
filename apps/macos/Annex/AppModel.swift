import AppKit
import Combine
import Foundation

/// App state shared by the menu bar UI. Owns the core, discovery, hosting and viewer windows.
@MainActor
final class AppModel: ObservableObject {
    @Published private(set) var deviceName = ""
    @Published private(set) var fingerprintDisplay = ""
    @Published private(set) var startupError: String?

    @Published var displays: [Displays.Info] = []
    @Published var selectedDisplayID: CGDirectDisplayID?
    @Published var preferHardwareEncoder = true
    @Published private(set) var isHosting = false
    @Published private(set) var hostStatus = HostController.Status()

    @Published private(set) var discovered: [PeerInfo] = []
    @Published var manualAddress = ""
    @Published private(set) var viewerStatus: String?
    @Published private(set) var latency: LatencyStats?
    @Published private(set) var trusted: [TrustedPeer] = []

    private var core: CoreClient?
    private var discovery: DiscoveryHandle?
    private var host: HostController?
    private var viewer: ViewerHandle?
    private var viewerWindow: ViewerWindowController?

    static var isRunningTests: Bool {
        ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
    }

    init() {
        guard !Self.isRunningTests else { return }
        start()
    }

    static var dataDirectory: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("Annex", isDirectory: true)
    }

    private func start() {
        do {
            let name = Host.current().localizedName ?? "Mac"
            let core = try CoreClient(dataDirectory: Self.dataDirectory, deviceName: name)
            self.core = core
            deviceName = core.deviceName
            fingerprintDisplay = Annex.displayFingerprint(core.fingerprint)
            discovery = try DiscoveryHandle(core: core) { [weak self] e in
                DispatchQueue.main.async { self?.handleDiscovery(e) }
            }
        } catch {
            startupError = error.localizedDescription
        }
        refreshDisplays()
        refreshTrusted()
    }

    func refreshDisplays() {
        displays = Displays.all()
        if selectedDisplayID == nil || !displays.contains(where: { $0.id == selectedDisplayID }) {
            selectedDisplayID = displays.first?.id
        }
    }

    func refreshTrusted() {
        trusted = core?.trustedPeers ?? []
    }

    // MARK: Hosting

    func setHosting(_ on: Bool) {
        on ? startHosting() : stopHosting()
    }

    private func startHosting() {
        guard let core, host == nil,
              let display = displays.first(where: { $0.id == selectedDisplayID }) ?? displays.first else { return }
        if !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess()
        }
        let h = HostController(core: core, display: display, preferHardware: preferHardwareEncoder)
        h.onStatus = { [weak self] s in self?.hostStatus = s }
        h.onEvent = { [weak self] e in self?.handleSessionEvent(e) }
        do {
            try h.start()
            host = h
            isHosting = true
            hostStatus = h.status
        } catch {
            hostStatus = HostController.Status(error: error.localizedDescription)
        }
    }

    private func stopHosting() {
        host?.stop()
        host = nil
        isHosting = false
        hostStatus = HostController.Status()
    }

    // MARK: Viewing

    func connect(to peer: PeerInfo) {
        guard let address = peer.connectAddress else { return }
        // Pin the certificate only for hosts we already trust; unknown hosts get a prompt.
        let pinned = peer.fingerprint.flatMap { fp in trusted.contains { $0.fingerprint == fp } ? fp : nil }
        connect(address: address, title: peer.name, expectedFingerprint: pinned)
    }

    func connectManual() {
        let address = manualAddress.trimmingCharacters(in: .whitespaces)
        guard !address.isEmpty else { return }
        connect(address: address, title: address, expectedFingerprint: nil)
    }

    private func connect(address: String, title: String, expectedFingerprint: String?) {
        guard let core else { return }
        disconnectViewer()
        let window = ViewerWindowController(title: "Annex — \(title)")
        window.onClose = { [weak self] in self?.disconnectViewer() }
        do {
            let v = try ViewerHandle(
                core: core, address: address, expectedFingerprint: expectedFingerprint, display: Displays.mainDisplayInfo(),
                onEvent: { [weak self] e in DispatchQueue.main.async { self?.handleViewerEvent(e) } },
                onFrame: { [weak window] buf, id, _, key in
                    window?.enqueue(Data(buf), frameID: id, keyframe: key)
                })
            window.attach(v)
            viewer = v
            viewerWindow = window
            viewerStatus = "Connecting to \(title)…"
        } catch {
            viewerStatus = "Connect failed: \(error.localizedDescription)"
        }
    }

    func disconnectViewer() {
        let w = viewerWindow
        viewerWindow = nil
        viewer?.disconnect()
        viewer = nil
        w?.onClose = nil
        w?.close()
        latency = nil
    }

    private func handleViewerEvent(_ e: CoreEvent) {
        switch e {
        case .sessionStarted(let s):
            viewerStatus = "Viewing \(s.peerName)"
            viewerWindow?.show(fullScreen: true)
        case .streamConfig(let s):
            viewerStatus = "Viewing — \(s.width)×\(s.height) @ \(s.fps) fps"
        case .latencyStats(let l):
            latency = l
        case .sessionEnded(let reason):
            viewerStatus = reason == "user" ? nil : "Disconnected: \(reason)"
            viewer = nil
            let w = viewerWindow
            viewerWindow = nil
            w?.onClose = nil
            w?.close()
            latency = nil
        default:
            handleSessionEvent(e)
        }
    }

    // MARK: Trust

    private func handleSessionEvent(_ e: CoreEvent) {
        switch e {
        case .trustRequest(let req):
            promptTrust(req)
        case .sessionStarted, .sessionEnded:
            refreshTrusted()
        default:
            break
        }
    }

    private func promptTrust(_ req: TrustRequest) {
        guard let core else { return }
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        let role = req.peerRole == "viewer" ? "wants to view this Mac's screen" : "is offering its screen"
        alert.messageText = "“\(req.peerName)” \(role)"
        alert.informativeText = """
        Make sure the other device shows the same code before trusting it:

        \(req.fingerprintDisplay)

        This device's own code is \(fingerprintDisplay).
        """
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Trust")
        alert.addButton(withTitle: "Reject")
        let accept = alert.runModal() == .alertFirstButtonReturn
        core.respondTrust(requestID: req.requestId, accept: accept)
        refreshTrusted()
    }

    func revoke(_ peer: TrustedPeer) {
        _ = try? core?.untrust(peer.fingerprint)
        refreshTrusted()
    }

    // MARK: Discovery

    private func handleDiscovery(_ e: CoreEvent) {
        switch e {
        case .peerFound(let p):
            discovered.removeAll { $0.instance == p.instance }
            discovered.append(p)
            discovered.sort { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        case .peerLost(let instance):
            discovered.removeAll { $0.instance == instance }
        default:
            break
        }
    }

    func quit() {
        disconnectViewer()
        stopHosting()
        NSApp.terminate(nil)
    }
}
