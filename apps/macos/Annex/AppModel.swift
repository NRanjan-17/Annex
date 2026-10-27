import AppKit
import AVFoundation
import Combine
import Foundation

/// App state shared by the menu bar UI. Owns the core, discovery, hosting and viewer windows.
@MainActor
final class AppModel: ObservableObject {
    @Published private(set) var deviceName = ""
    @Published private(set) var fingerprintDisplay = ""
    @Published private(set) var startupError: String?

    let settings = AppSettings()
    /// In-app updates from GitHub releases.
    lazy var updater = Updater(settings: settings)
    /// The main window's selected page.
    @Published var pane: Pane = .home

    @Published var displays: [Displays.Info] = []
    @Published var selectedDisplayID: CGDirectDisplayID?
    @Published private(set) var isHosting = false
    @Published private(set) var hostStatus = HostController.Status() {
        didSet {
            updateKeepAwake()
            updatePeerToPeerVisibility()
            updateInSession()
        }
    }
    /// Host: every Mac nearby can find this one on peer-to-peer Wi-Fi until then (else only
    /// paired Macs can).
    @Published private(set) var visibleToNewMacsUntil: Date?

    /// Nearby: computers found on the network, plus Macs found only on peer-to-peer Wi-Fi.
    @Published private(set) var discovered: [PeerInfo] = []
    private var lanPeers: [PeerInfo] = []
    /// Macs sharing on peer-to-peer Wi-Fi (no router needed), while the Connect page looks for them.
    private var peerToPeerHosts: [PeerToPeerHost] = []
    private var peerToPeerBrowser: PeerToPeerBrowser?
    private var peerToPeerAdvertiser: PeerToPeerAdvertiser?
    /// The current viewer's relay over peer-to-peer Wi-Fi, if it has one.
    private var peerToPeerTunnel: PeerToPeerTunnel?
    @Published var manualAddress = ""
    /// Connect by Code or Address, open over Nearby (the menu bar panel asks for it too).
    @Published var showConnectSheet = false
    /// Connect by Address sheet: a code from a computer sharing over the internet.
    @Published var manualCode = ""
    /// The address field holds the PC hotspot's address, filled in by `joinPCHotspot`: kept when
    /// the sheet opens again.
    private var addressFromHotspot = false
    /// Release notes shown over the main window: after an update, or from About → What's New.
    @Published var whatsNew: WhatsNew?
    /// After an update: waits for the main window (Annex may have started quietly at login).
    private var pendingWhatsNew: WhatsNew?
    /// The current viewer connected by code (its errors read differently).
    private var viewingByCode = false
    /// Sharing over the internet: what the This Mac page shows.
    @Published private(set) var internetShare: InternetShare?
    /// Viewer: ask hosts for a new virtual display instead of mirroring their screen.
    @Published var extendMode = false
    @Published private(set) var viewerStatus: String?
    @Published private(set) var latency: LatencyStats?
    @Published private(set) var trusted: [TrustedPeer] = []
    /// Screen Recording permission (needed to share this screen).
    @Published private(set) var screenRecordingAllowed = CGPreflightScreenCaptureAccess()
    private var askedForScreenRecording = false
    /// Accessibility permission (needed to let viewers control this Mac).
    @Published private(set) var accessibilityTrusted = InputInjector.isTrusted
    /// Settings → Sound: macOS refused Annex the microphone, so Send my microphone can't work.
    @Published private(set) var microphoneDenied = false
    /// Viewer: the host currently accepts this Mac's mouse and keyboard.
    @Published private(set) var controllingHost = false
    /// Viewer: the computer this Mac shows, by the name it gave.
    @Published private(set) var viewerPeerName = ""
    /// Viewer: the session started as an extra display (Extend) rather than a copy of its screen.
    @Published private(set) var viewerExtends: Bool?
    private var viewerFingerprint: String?
    /// Direct Wi-Fi: what's happening, for the Connect page.
    @Published private(set) var directStatus: String?
    /// Connected over the other computer's hotspot right now.
    @Published private(set) var onDirectWiFi = false
    /// Reconnecting over the hotspot after switching: who, how, and attempts left.
    private var directReconnect: (fingerprint: String, name: String, extend: Bool, attempts: Int)?
    /// Viewer: connect, then switch to direct Wi-Fi before showing the picture.
    @Published var useDirectWiFi = false
    private var switchOnStart = false
    /// The viewer window waits for the direct link before it shows.
    private var holdingWindow = false
    /// Host: the hotspot this Mac joined for its viewers, if any.
    @Published private(set) var hostDirectWiFi: String?
    /// Nearby refresh in progress (the button shows a spinner).
    /// Bumped whenever a viewer is replaced, so late events from the old one are ignored.
    private var viewerGeneration = 0
    private var cancellables: Set<AnyCancellable> = []
    private let localNetwork = LocalNetworkAccess()

    private var core: CoreClient?
    private var discovery: DiscoveryHandle?
    private var host: HostController?
    // Published by hand: the menu bar icon follows `isViewing`, and a plain property never redraws it.
    private var viewer: ViewerHandle? {
        willSet { objectWillChange.send() }
        didSet { updateKeepAwake(); updateInSession() }
    }
    /// Viewing, or showing this Mac to someone. Peer-to-peer browsing pauses meanwhile: it shares
    /// the Wi-Fi radio with the session and would add lag.
    @Published private(set) var inSession = false
    /// Held while a session runs, so neither screen sleeps: the viewer gets no local input, and
    /// the host's screen (or its Annex display) is what the viewer is watching.
    private var keepAwake: NSObjectProtocol?

    private func updateKeepAwake() {
        let active = viewer != nil || !hostStatus.viewers.isEmpty || !hostStatus.extendViewers.isEmpty
        if active, keepAwake == nil {
            Annex.log(.debug, "[session] keeping this Mac awake while the session runs")
            keepAwake = ProcessInfo.processInfo.beginActivity(
                options: [.idleDisplaySleepDisabled, .idleSystemSleepDisabled], reason: "An Annex session is running")
        } else if !active, let token = keepAwake {
            Annex.log(.debug, "[session] no session: this Mac may sleep again")
            ProcessInfo.processInfo.endActivity(token)
            keepAwake = nil
        }
    }
    private var viewerWindow: ViewerWindowController?
    /// Viewer: the host's system ("macos", "windows", "linux"), for scroll direction.
    private var viewerPeerOS: String?
    private let clipboard = ClipboardSync()
    let notch = NotchDrop()
    /// Viewer: something was copied on this Mac while the viewer window wasn't in front.
    private var copiedWhileAway = false

    /// Started by "Open at login": stay quietly in the menu bar.
    static let launchedAtLogin: Bool = {
        guard let e = NSAppleEventManager.shared().currentAppleEvent else { return false }
        return e.eventID == kAEOpenApplication
            && e.paramDescriptor(forKeyword: keyAEPropData)?.enumCodeValue == keyAELaunchedAsLogInItem
    }()

    static var isRunningTests: Bool {
        ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
    }

    init() {
        extendMode = settings.extendByDefault
        // Re-publish settings changes so views observing the model refresh.
        settings.objectWillChange.sink { [weak self] in self?.objectWillChange.send() }.store(in: &cancellables)
        // A new scaling choice applies straight away to screens extending this Mac.
        settings.$extendScale.dropFirst().removeDuplicates()
            .sink { [weak self] scale in self?.host?.extendScale = scale }
            .store(in: &cancellables)
        // So does sharing this Mac's sound, on a running mirror capture.
        settings.$shareSound.dropFirst().removeDuplicates()
            .sink { [weak self] on in self?.host?.shareSound = on }
            .store(in: &cancellables)
        guard !Self.isRunningTests else { return }
        $pane.dropFirst().removeDuplicates()
            .sink { Annex.log(.debug, "[app] page: \($0)") }
            .store(in: &cancellables)
        start()
        checkWhatsNew()
        NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)
            .sink { [weak self] _ in self?.refreshPermissions() }
            .store(in: &cancellables)
        if settings.startSharingAtLaunch { startHosting() }
        updater.objectWillChange.sink { [weak self] in self?.objectWillChange.send() }.store(in: &cancellables)
        updater.startAutomaticChecks()
    }

    var isViewing: Bool { viewer != nil }
    /// A session either way: this Mac shows another computer, or another computer shows this one.
    var isLive: Bool { isViewing || hostStatus.viewerName != nil }

    /// Which way the picture goes right now, said the same way on every screen.
    var sessionRole: SessionRole? {
        if isViewing, let extend = viewerExtends { return .viewing(from: viewerPeerName, extend: extend) }
        let s = hostStatus
        if let name = s.extendViewers.first { return .sharing(with: name, extend: true) }
        if let name = s.mirrorViewers.first { return .sharing(with: name, extend: false) }
        return nil
    }

    /// Another computer's name as Annex shows it. Two Macs of one owner are often both called
    /// "Name's MacBook Pro"; then "(other Mac)" says which one is meant.
    func label(_ name: String) -> String { hasThisName(name) ? "\(name) (other Mac)" : name }

    func hasThisName(_ name: String) -> Bool {
        func key(_ s: String) -> String {
            s.replacingOccurrences(of: "’", with: "'").trimmingCharacters(in: .whitespaces).lowercased()
        }
        return !deviceName.isEmpty && key(name) == key(deviceName)
    }

    /// A computer nearby or in the session with this Mac's name, for the notice that asks to rename one.
    var sameNamePeer: String? {
        let session = sessionRole.map { role -> String in
            switch role { case .sharing(let n, _), .viewing(let n, _): n }
        }
        return ([session].compactMap { $0 } + discovered.map(\.name)).first(where: hasThisName)
    }

    private func updateInSession() {
        let busy = viewer != nil || hostStatus.viewerName != nil
        if busy != inSession { inSession = busy }
    }

    func isPaired(_ peer: PeerInfo) -> Bool {
        peer.fingerprint.map { fp in trusted.contains { $0.fingerprint == fp } } ?? false
    }

    func refreshPermissions() {
        let recording = CGPreflightScreenCaptureAccess()
        if recording != screenRecordingAllowed {
            Annex.log(.info, "[permissions] Screen Recording is now \(recording ? "allowed" : "off")")
            screenRecordingAllowed = recording
        }
        let trusted = InputInjector.isTrusted
        if trusted != accessibilityTrusted {
            Annex.log(.info, "[permissions] Accessibility is now \(trusted ? "allowed" : "off")")
            accessibilityTrusted = trusted
        }
        settings.refreshLaunchAtLogin()
        // Allowed again (or taken away) in System Settings while Annex was in the background.
        let micDenied = settings.sendMicrophone && Self.microphoneRefused
        if micDenied != microphoneDenied { microphoneDenied = micDenied }
    }

    private static var microphoneRefused: Bool {
        let status = AVCaptureDevice.authorizationStatus(for: .audio)
        return status == .denied || status == .restricted
    }

    /// Settings → Sound → Send my microphone. Turning it on asks macOS for the microphone first, so
    /// the prompt shows now rather than when a session opens it; refused, the switch stays off.
    func setSendMicrophone(_ on: Bool) {
        guard on else {
            microphoneDenied = false
            settings.sendMicrophone = false
            return
        }
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized:
            microphoneDenied = false
            settings.sendMicrophone = true
        case .notDetermined:
            Annex.log(.info, "[permissions] asking macOS for the microphone")
            AVCaptureDevice.requestAccess(for: .audio) { [weak self] granted in
                DispatchQueue.main.async {
                    Annex.log(.info, "[permissions] microphone prompt: \(granted ? "allowed" : "not allowed")")
                    guard let self else { return }
                    self.microphoneDenied = !granted
                    self.settings.sendMicrophone = granted
                }
            }
        default:
            Annex.log(.warn, "[permissions] the microphone isn't allowed; Send my microphone stays off")
            microphoneDenied = true
            settings.sendMicrophone = false
        }
    }

    static var dataDirectory: URL {
        #if DEBUG
        // Its own identity and pairings, kept in a file (see `start`), so a Debug build never
        // touches the installed app's.
        let folder = "Annex Debug"
        #else
        let folder = "Annex"
        #endif
        return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent(folder, isDirectory: true)
    }

    private func start() {
        do {
            #if DEBUG
            // Debug builds are signed ad hoc, so each rebuild is a new app to the Keychain and
            // macOS asks for the login password on every launch. Keep their key in a file, in
            // their own data folder.
            setenv("ANNEX_KEY_STORAGE", "file", 0)
            #endif
            // Before the core starts, so its first lines already follow the setting.
            settings.$verboseLogging.removeDuplicates().sink { Annex.setVerboseLogging($0) }.store(in: &cancellables)
            let name = settings.customDeviceName ?? Self.systemName
            let core = try CoreClient(dataDirectory: Self.dataDirectory, deviceName: name)
            self.core = core
            logStartup(core)
            settings.$headless.sink { [weak core] on in core?.setHeadless(on) }.store(in: &cancellables)
            Publishers.CombineLatest4(settings.$shareSound, settings.$hearSound, settings.$sendMicrophone, settings.$virtualMicrophone)
                .sink { [weak core] share, hear, mic, virtualMic in
                    core?.setAudio(shareSound: share, hearSound: hear, sendMicrophone: mic, virtualMicrophone: virtualMic)
                }
                .store(in: &cancellables)
            settings.$receiveFiles.sink { [weak core] on in
                let folder = on ? FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first : nil
                Annex.log(.debug, "[transfer] incoming files " + (folder.map { "go to \($0.path)" } ?? "are refused"))
                core?.setDownloads(folder)
            }.store(in: &cancellables)
            clipboard.onCopy = { [weak self] mime, data in self?.copied(mime: mime, data: data) }
            notch.onDrop = { [weak self] urls in self?.send(files: urls) }
            notch.onPause = { [weak self] item, paused in self?.pauseTransfer(item, paused: paused) ?? false }
            notch.onCancel = { [weak self] item in self?.cancelTransfer(item) }
            settings.$notchDrop.sink { [weak self] _ in DispatchQueue.main.async { self?.updateNotch() } }.store(in: &cancellables)
            settings.$shareClipboard.sink { [weak self] on in
                guard let self else { return }
                if on { clipboard.start() } else { clipboard.stop(); copiedWhileAway = false }
            }.store(in: &cancellables)
            settings.$reverseScrollOnOthers.sink { [weak self] on in
                guard let self, let viewerWindow else { return }
                let reverse = on && viewerPeerOS.map { $0 != "macos" } == true
                Annex.log(.info, "[scroll] reverse scrolling \(reverse ? "on" : "off") (setting \(on), host \(viewerPeerOS ?? "unknown"))")
                viewerWindow.setReverseScroll(reverse)
            }.store(in: &cancellables)
            deviceName = core.deviceName
            // Show up under Nearby on other computers as soon as Annex is open.
            core.setPresence(true)
            fingerprintDisplay = Annex.displayFingerprint(core.fingerprint)
            discovery = try DiscoveryHandle(core: core) { [weak self] e in
                DispatchQueue.main.async { self?.handleDiscovery(e) }
            }
            // Ask for Local Network access now, not when the first viewer connects.
            if !Self.isRunningTests { localNetwork.request() }
            $pane.combineLatest(settings.$peerToPeerWiFi, $inSession.removeDuplicates())
                .sink { [weak self] pane, on, busy in self?.updatePeerToPeerBrowsing(pane: pane, on: on && !busy) }
                .store(in: &cancellables)
        } catch {
            // Usually before the core could open the log, so this may only reach a terminal.
            Annex.log(.error, "[app] couldn't start: \(error.localizedDescription)")
            startupError = error.localizedDescription
        }
        refreshDisplays()
        refreshTrusted()
    }

    /// The first lines of every log: what this Mac is and how Annex is set up.
    private func logStartup(_ core: CoreClient) {
        let info = Bundle.main.infoDictionary ?? [:]
        let build = "\(info["CFBundleShortVersionString"] as? String ?? "?") (\(info["CFBundleVersion"] as? String ?? "?"))"
        #if DEBUG
        let config = "Debug"
        #else
        let config = "Release"
        #endif
        #if arch(arm64)
        let arch = "Apple silicon"
        #else
        let arch = "Intel"
        #endif
        Annex.log(.info, "[app] Annex \(build) \(config), core \(Annex.version), "
                  + "\(ProcessInfo.processInfo.operatingSystemVersionString), \(Self.hardwareModel) (\(arch))")
        Annex.log(.info, "[app] device \"\(core.deviceName)\", identity \(core.fingerprint.prefix(16))…, "
                  + "data \(Self.dataDirectory.path), opened \(Self.launchedAtLogin ? "at login" : "by the user")")
        for (i, screen) in NSScreen.screens.enumerated() {
            let f = screen.frame
            Annex.log(.info, "[display] screen \(i + 1) of \(NSScreen.screens.count): \(screen.localizedName), "
                      + "\(Int(f.width))×\(Int(f.height)) points at \(screen.backingScaleFactor)x"
                      + (screen.safeAreaInsets.top > 0 ? ", notch" : "") + (screen == NSScreen.main ? ", main" : ""))
        }
        Annex.log(.info, "[permissions] Screen Recording \(CGPreflightScreenCaptureAccess() ? "allowed" : "off"), "
                  + "Accessibility \(InputInjector.isTrusted ? "allowed" : "off"), "
                  + "virtual displays \(HostController.extendSupported ? "supported" : "not supported")")
        Annex.log(.info, "[settings] \(settings.snapshot)")
    }

    /// "Mac15,12", for the log.
    private static var hardwareModel: String {
        var size = 0
        sysctlbyname("hw.model", nil, &size, nil, 0)
        guard size > 0 else { return "Mac" }
        var buf = [CChar](repeating: 0, count: size)
        sysctlbyname("hw.model", &buf, &size, nil, 0)
        return String(cString: buf)
    }

    static var systemName: String { Host.current().localizedName ?? "Mac" }

    /// Renames this Mac for other devices. An empty name goes back to the system name.
    func rename(_ name: String) {
        guard let core else { return }
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let stored = core.setDeviceName(trimmed.isEmpty ? Self.systemName : trimmed)
        Annex.log(.info, "[app] renamed \"\(deviceName)\" to \"\(stored)\"")
        settings.customDeviceName = trimmed.isEmpty ? nil : stored
        deviceName = stored
        core.setPresence(true)
        // Discovery advertises the name; re-advertise if we're sharing.
        if isHosting, hostStatus.viewerName == nil {
            stopHosting()
            startHosting()
        }
    }

    func refreshDisplays() {
        let all = Displays.all()
        if all != displays {
            Annex.log(.debug, "[display] screens: " + all.map { "\($0.name) \($0.id) \($0.pixelWidth)×\($0.pixelHeight) px" }.joined(separator: "; "))
        }
        displays = all
        if selectedDisplayID == nil || !displays.contains(where: { $0.id == selectedDisplayID }) {
            selectedDisplayID = displays.first?.id
        }
    }

    func refreshTrusted() {
        trusted = core?.trustedPeers ?? []
    }

    // MARK: Hosting

    func setHosting(_ on: Bool) {
        Annex.log(.info, "[host] \(on ? "start" : "stop") sharing (asked by the user)")
        on ? startHosting() : stopHosting()
    }

    private func startHosting() {
        guard let core else { return Annex.log(.warn, "[host] can't share: the core didn't start") }
        guard host == nil else { return Annex.log(.debug, "[host] already sharing") }
        guard let display = displays.first(where: { $0.id == selectedDisplayID }) ?? displays.first else {
            return Annex.log(.warn, "[host] can't share: no screen found")
        }
        // Ask macOS once per launch; after that the Share page explains what to do instead of
        // showing the system dialog every time sharing starts.
        screenRecordingAllowed = CGPreflightScreenCaptureAccess()
        if !screenRecordingAllowed && !askedForScreenRecording {
            askedForScreenRecording = true
            Annex.log(.info, "[permissions] asking macOS for Screen Recording")
            let granted = CGRequestScreenCaptureAccess()
            Annex.log(.info, "[permissions] Screen Recording prompt: \(granted ? "allowed" : "not allowed yet")")
        } else if !screenRecordingAllowed {
            Annex.log(.warn, "[permissions] sharing without Screen Recording: viewers will see a black screen")
        }
        Annex.log(.info, "[host] sharing \(display.name) (\(display.id), \(display.pixelWidth)×\(display.pixelHeight) px), "
                  + "hardware encoder \(settings.preferHardwareEncoder ? "preferred" : "off"), extend scale \(settings.extendScale)")
        let h = HostController(core: core, display: display, preferHardware: settings.preferHardwareEncoder)
        h.extendScale = settings.extendScale
        h.shareSound = settings.shareSound
        h.onStatus = { [weak self] s in self?.hostStatus = s }
        h.onEvent = { [weak self] e in self?.handleSessionEvent(e) }
        do {
            try h.start()
            host = h
            isHosting = true
            hostStatus = h.status
            Annex.log(.info, "[host] sharing on port \(h.status.listeningPort ?? 0)")
            if settings.peerToPeerWiFi, let port = h.status.listeningPort.flatMap({ UInt16(exactly: $0) }), port > 0 {
                do {
                    peerToPeerAdvertiser = try PeerToPeerAdvertiser(name: core.deviceName, fingerprint: core.fingerprint,
                                                                    extend: HostController.extendSupported, corePort: port,
                                                                    visibility: peerToPeerVisibility)
                } catch {
                    Annex.log(.warn, "[network] peer-to-peer Wi-Fi: couldn't advertise: \(error)")
                }
            }
        } catch {
            Annex.log(.error, "[host] couldn't start sharing: \(error.localizedDescription)")
            hostStatus = HostController.Status(error: error.localizedDescription)
        }
    }

    private func stopHosting() {
        if host != nil { Annex.log(.info, "[host] sharing stopped") }
        internetShare = nil
        peerToPeerAdvertiser?.stop()
        peerToPeerAdvertiser = nil
        visibleToNewMacsUntil = nil
        host?.stop()
        host = nil
        isHosting = false
        hostStatus = HostController.Status()
        hostDirectWiFi = nil
    }

    // MARK: Internet

    /// Where a code-sharing attempt is.
    enum InternetShare: Equatable {
        case starting
        case code(String, expires: Date)
        case joining(String)
        /// The viewer couldn't connect directly. `relayed`: it got a relay address (`limited`: ours).
        case relay(String, relayed: Bool, limited: Bool)
        /// The code no longer works: why (nil when it was used).
        case ended(String?)
    }

    /// Host: get a code for a computer on another network to type.
    func shareCode() {
        guard let host else { return Annex.log(.warn, "[network] no code: not sharing") }
        internetShare = .starting
        Annex.log(.info, "[network] getting a code from \(settings.server), own relay \(settings.ownRelay == nil ? "none" : "set")")
        do {
            try host.shareCode(server: settings.server,
                               ownRelay: settings.ownRelay)
        } catch {
            Annex.log(.warn, "[network] couldn't get a code: \(error.localizedDescription)")
            internetShare = .ended(error.localizedDescription)
        }
    }

    func stopCode() {
        Annex.log(.info, "[network] code stopped by the user")
        host?.stopCode()
        internetShare = nil
    }

    private func handleInternetEvent(_ e: CoreEvent) {
        switch e {
        case .internetCode(let code, let seconds):
            internetShare = .code(code, expires: Date().addingTimeInterval(TimeInterval(seconds)))
        case .internetJoined(let name):
            internetShare = .joining(name)
        case .internetRelay(let relayed, let limited):
            if case .joining(let name) = internetShare {
                internetShare = .relay(name, relayed: relayed, limited: limited)
            }
        case .internetCodeEnded(let reason):
            switch (reason, internetShare) {
            case ("stopped", _): internetShare = nil
            case ("used", .relay(_, false, _)):
                Annex.log(.warn, "[network] the code was used, but no direct path or relay reached the other computer")
                internetShare = .ended("The other computer's network blocks direct connections, and no relay was available. "
                                       + "Add your own relay in Settings → Advanced.")
            case ("used", _): internetShare = .ended(nil)
            case ("expired", _): internetShare = .ended("The code expired.")
            case (let r, _): internetShare = .ended(Self.friendlyInternetError(r))
            }
        default: break
        }
    }

    static func friendlyInternetError(_ r: String) -> String {
        let l = r.lowercased()
        if l.contains("stun") || l.contains("didn't answer") {
            return "Couldn't find this Mac's internet address. Check the internet connection."
        }
        if l.contains("rendezvous") || l.contains("websocket") || l.contains("i/o") || l.contains("dns") {
            return "Couldn't reach the server. Check the internet connection, or the server address in Settings → Advanced."
        }
        return r
    }

    /// Viewer: connect by a code from a computer sharing over the internet.
    func connectByCode() {
        let code = manualCode.trimmingCharacters(in: .whitespaces)
        guard let core, !code.isEmpty else { return }
        // Never the code: it lets anyone in for 10 minutes.
        Annex.log(.info, "[viewer] connecting by code through \(settings.server), extend \(extendMode), "
                  + "control \(settings.controlWhenViewing), quality \(settings.quality)")
        disconnectViewer()
        switchOnStart = false
        holdingWindow = false
        let generation = viewerGeneration
        let window = ViewerWindowController(title: "Code \(code.uppercased())")
        window.onClose = { [weak self] in self?.disconnectViewer() }
        window.onFocus = { [weak self] in self?.viewerFocused() }
        window.onFiles = { [weak self] urls in self?.notch.dropped(urls) }
        do {
            let v = try ViewerHandle(
                core: core, code: code, server: settings.server,
                display: Displays.mainDisplayInfo(), extend: extendMode, input: settings.controlWhenViewing,
                quality: settings.quality,
                onEvent: { [weak self] e in
                    DispatchQueue.main.async {
                        guard let self, self.viewerGeneration == generation else { return }
                        self.handleViewerEvent(e)
                    }
                },
                onFrame: { [weak window] buf, id, _, key in
                    window?.enqueue(Data(buf), frameID: id, keyframe: key)
                })
            window.attach(v)
            viewer = v
            viewerWindow = window
            viewerPeerName = "the other computer"
            viewerStatus = "Connecting with code \(code.uppercased())…"
            viewingByCode = true
        } catch {
            Annex.log(.warn, "[viewer] couldn't connect by code: \(error.localizedDescription)")
            viewerStatus = "Couldn't connect: \(error.localizedDescription)"
        }
    }

    #if DEBUG
    /// Screenshots: `ANNEX_PREVIEW_PANE=share|settings|about` opens that page; with `ANNEX_PREVIEW_CODE=1`
    /// (and sharing on) it also gets a code. `ANNEX_PREVIEW_CONNECT=address` connects there.
    /// `ANNEX_PREVIEW_MESH=name` lists a computer as found on a private network.
    func openPreviewPane() {
        let env = ProcessInfo.processInfo.environment
        if let name = env["ANNEX_PREVIEW_MESH"] {
            var peer = PeerInfo(instance: "mesh:preview", name: name, addresses: ["100.70.1.2"], port: 47800,
                                fingerprint: String(repeating: "cd", count: 32), protoMajor: 1, sharing: true, extend: true)
            peer.mesh = true
            lanPeers.append(peer)
            rebuildNearby()
        }
        if let address = env["ANNEX_PREVIEW_CONNECT"] {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
                self?.manualAddress = address
                self?.connectManual()
            }
        }
        switch env["ANNEX_PREVIEW_PANE"] {
        case "share": pane = .share
        case "settings": pane = .settings
        case "about": pane = .about
        default: return
        }
        if env["ANNEX_PREVIEW_CODE"] != nil {
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in self?.shareCode() }
        }
    }
    #endif

    // MARK: Peer-to-peer visibility

    /// How long "Let a new Mac find this one" lasts.
    static let newMacWindow: TimeInterval = 5 * 60

    /// Host: for a few minutes, every Mac nearby can see this one on peer-to-peer Wi-Fi. It still
    /// has to pair with the PIN shown here.
    func letNewMacsFindThisOne() {
        let until = Date().addingTimeInterval(Self.newMacWindow)
        Annex.log(.info, "[network] every Mac nearby can find this one for \(Int(Self.newMacWindow / 60)) minutes")
        visibleToNewMacsUntil = until
        updatePeerToPeerVisibility()
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.newMacWindow) { [weak self] in
            guard let self, self.visibleToNewMacsUntil == until else { return }
            self.hideFromNewMacs()
        }
    }

    func hideFromNewMacs() {
        Annex.log(.info, "[network] only paired Macs can find this one again")
        visibleToNewMacsUntil = nil
        updatePeerToPeerVisibility()
    }

    /// Peer-to-peer Wi-Fi slows the normal Wi-Fi link while it's on, which would hurt a session
    /// streaming over that link, so this Mac stops advertising while any session runs (flows
    /// already relayed keep going). The user asking for new Macs overrides that.
    private var peerToPeerVisibility: PeerToPeer.Visibility {
        if let until = visibleToNewMacsUntil, until > Date() { return .everyone }
        let busy = !hostStatus.viewers.isEmpty || !hostStatus.extendViewers.isEmpty
        return busy ? .hidden : .paired
    }

    private func updatePeerToPeerVisibility() {
        peerToPeerAdvertiser?.setVisibility(peerToPeerVisibility)
    }

    /// Host: ask a viewer (a Windows PC) to start its hotspot; this Mac joins it when it answers.
    func switchViewerToDirectWiFi(_ viewer: HostController.Viewer) {
        directStatus = "Asking \(viewer.name) to start direct Wi-Fi…"
        Annex.log(.info, "[network] asking \(viewer.name) (session \(viewer.id)) to start direct Wi-Fi")
        if host?.requestDirectLink(session: viewer.id) != true {
            Annex.log(.warn, "[network] direct Wi-Fi: \(viewer.name) is no longer connected")
            directStatus = "\(viewer.name) is no longer connected."
        }
    }

    // MARK: Viewing

    func connect(to peer: PeerInfo) {
        // Pin the certificate only for hosts we already trust; unknown hosts get a prompt.
        let pinned = peer.fingerprint.flatMap { fp in trusted.contains { $0.fingerprint == fp } ? fp : nil }
        let useDirect = useDirectWiFi
        Annex.log(.info, "[viewer] connect to \(peer.name) (\(peer.instance)), \(pinned == nil ? "not paired" : "paired"), "
                  + "extend \(extendMode), direct Wi-Fi \(useDirect)")
        guard settings.peerToPeerWiFi, let fp = peer.fingerprint,
              let p2p = peerToPeerHosts.first(where: { $0.fingerprint == fp }) else {
            guard let address = peer.connectAddress else {
                return Annex.log(.warn, "[viewer] \(peer.name) has no address to connect to")
            }
            connect(address: address, title: peer.name, expectedFingerprint: pinned)
            switchOnStart = useDirect && viewer != nil
            return
        }
        // Also reachable over peer-to-peer Wi-Fi: open the relay first, then let the core race
        // it against the network addresses.
        disconnectViewer()
        let generation = viewerGeneration
        viewerStatus = "Connecting to \(peer.name)…"
        var tunnel: PeerToPeerTunnel?
        Annex.log(.debug, "[network] \(peer.name) is also on peer-to-peer Wi-Fi: opening a relay")
        do {
            tunnel = try PeerToPeerTunnel(to: p2p.endpoint) { [weak self] local in
                guard let self, self.viewerGeneration == generation else { tunnel?.stop(); return }
                if local == nil { Annex.log(.warn, "[network] peer-to-peer relay to \(peer.name) didn't open") }
                let address = [peer.connectAddress, local].compactMap { $0 }.joined(separator: ",")
                guard !address.isEmpty else {
                    tunnel?.stop()
                    Annex.log(.warn, "[viewer] couldn't reach \(peer.name): no network address and no peer-to-peer relay")
                    self.viewerStatus = "Couldn't reach \(peer.name) over peer-to-peer Wi-Fi."
                    return
                }
                self.connect(address: address, title: peer.name, expectedFingerprint: pinned, tunnel: local == nil ? nil : tunnel)
                if local == nil { tunnel?.stop() }
                self.switchOnStart = useDirect && self.viewer != nil
            }
        } catch {
            Annex.log(.warn, "[network] couldn't open a peer-to-peer relay to \(peer.name): \(error)")
        }
        if tunnel == nil, let address = peer.connectAddress {
            connect(address: address, title: peer.name, expectedFingerprint: pinned)
            switchOnStart = useDirect && viewer != nil
        }
    }

    func connectManual() {
        let address = manualAddress.trimmingCharacters(in: .whitespaces)
        guard !address.isEmpty else { return }
        addressFromHotspot = false
        connect(address: address, title: address, expectedFingerprint: nil)
    }

    private func connect(address: String, title: String, expectedFingerprint: String?, tunnel: PeerToPeerTunnel? = nil) {
        guard let core else { tunnel?.stop(); return Annex.log(.warn, "[viewer] can't connect: the core didn't start") }
        Annex.log(.info, "[viewer] connecting to \(address) (\(title)), \(expectedFingerprint == nil ? "unpinned" : "pinned"), "
                  + "peer-to-peer relay \(tunnel != nil), extend \(extendMode), control \(settings.controlWhenViewing), quality \(settings.quality)")
        disconnectViewer()
        peerToPeerTunnel = tunnel
        switchOnStart = false
        holdingWindow = false
        let generation = viewerGeneration
        let window = ViewerWindowController(title: title)
        window.onClose = { [weak self] in self?.disconnectViewer() }
        window.onFocus = { [weak self] in self?.viewerFocused() }
        window.onFiles = { [weak self] urls in self?.notch.dropped(urls) }
        do {
            let v = try ViewerHandle(
                core: core, address: address, expectedFingerprint: expectedFingerprint, display: Displays.mainDisplayInfo(),
                extend: extendMode, input: settings.controlWhenViewing, quality: settings.quality,
                onEvent: { [weak self] e in
                    DispatchQueue.main.async {
                        // A replaced viewer's last events (its sessionEnded) arrive after the new one started.
                        guard let self, self.viewerGeneration == generation else { return }
                        self.handleViewerEvent(e)
                    }
                },
                onFrame: { [weak window] buf, id, _, key in
                    window?.enqueue(Data(buf), frameID: id, keyframe: key)
                })
            window.attach(v)
            viewer = v
            viewerWindow = window
            viewerPeerName = title
            viewerStatus = "Connecting to \(title)…"
            viewingByCode = false
        } catch {
            Annex.log(.warn, "[viewer] couldn't connect to \(address): \(error.localizedDescription)")
            viewerStatus = "Couldn't connect: \(error.localizedDescription)"
        }
    }

    func disconnectViewer() {
        // This viewer's own sessionEnded is ignored from here on (see viewerGeneration), so reset
        // what it would have reset.
        if viewer != nil { Annex.log(.info, "[viewer] disconnecting from \(viewerPeerName)") }
        viewerGeneration += 1
        // The window goes first: freeing the connection waits for the bye to go out.
        let w = viewerWindow
        viewerWindow = nil
        w?.onClose = nil
        w?.close()
        release(viewer)
        viewer = nil
        peerToPeerTunnel?.stop()
        peerToPeerTunnel = nil
        latency = nil
        controllingHost = false
        viewerStatus = nil
        viewerExtends = nil
        onDirectWiFi = false
        holdingWindow = false
    }

    /// Frees a viewer connection off the main thread: it sends the bye and waits up to 300 ms for
    /// it to arrive, which would otherwise hold up closing the window.
    private func release(_ handle: ViewerHandle?) {
        guard let handle else { return }
        DispatchQueue.global(qos: .userInitiated).async { handle.disconnect() }
    }

    private func handleViewerEvent(_ e: CoreEvent) {
        defer { updateNotch() }
        switch e {
        case .sessionStarted(let s):
            let via = switch s.link {
            case "direct": " · cable"
            case "internet": " · internet"
            case "relayed": " · relay"
            default: peerToPeerTunnel != nil ? " · peer-to-peer" : ""
            }
            viewerPeerName = s.peerName
            viewerExtends = s.mode == "extend"
            viewerStatus = (s.mode == "extend" ? "Extra display from \(label(s.peerName))" : "Viewing \(label(s.peerName))") + via
            viewerFingerprint = s.fingerprint
            if directReconnect != nil {
                directReconnect = nil
                directStatus = nil
                onDirectWiFi = true
                holdingWindow = false
            }
            let route = switch s.link {
            case "direct": "Over a cable"
            case "internet": "Over the internet"
            case "relayed": "Through a relay"
            default: peerToPeerTunnel != nil ? "Over peer-to-peer Wi-Fi" : onDirectWiFi ? "On direct Wi-Fi" : ""
            }
            viewerWindow?.setPeer(s.mode == "extend" ? "Extra display from \(label(s.peerName))" : "Viewing \(label(s.peerName))",
                                  detail: route)
            viewerWindow?.setStreamSize(width: s.stream.width, height: s.stream.height)
            if switchOnStart {
                // Set up direct Wi-Fi first; the picture starts once reconnected over it.
                switchOnStart = false
                holdingWindow = true
                switchToDirectWiFi()
            }
            if !holdingWindow { viewerWindow?.show(fullScreen: settings.openFullScreen) }
            controllingHost = s.input ?? false
            viewerPeerOS = s.peerOs
            let reverse = settings.reverseScrollOnOthers && s.peerOs.map { $0 != "macos" } == true
            let relative = s.mode == "extend" && s.pointerDeltas == true
            Annex.log(.info, "[viewer] \(holdingWindow ? "window waits for direct Wi-Fi" : "window shown\(settings.openFullScreen ? " full screen" : "")")"
                      + " (\(route.isEmpty ? "on the network" : route.lowercased())), controlling \(controllingHost), "
                      + "pointer \(relative ? "movement" : "positions")")
            Annex.log(.info, "[scroll] reverse scrolling \(reverse ? "on" : "off") (setting \(settings.reverseScrollOnOthers), host \(s.peerOs ?? "unknown"))")
            viewerWindow?.setReverseScroll(reverse)
            viewerWindow?.setRelativePointer(relative)
            if settings.controlWhenViewing {
                viewerWindow?.setControlling(controllingHost, hostName: label(s.peerName))
            }
        case .streamConfig(let s):
            // The title set at session start keeps how it's reached (" · relay"); the size is a detail.
            viewerWindow?.setStreamSize(width: s.width, height: s.height)
        case .inputAllowed(_, let allowed):
            if allowed != controllingHost { Annex.log(.info, "[input] \(allowed ? "now controlling" : "no longer controlling") \(viewerPeerName)") }
            controllingHost = allowed
            viewerWindow?.setControlling(allowed, hostName: label(viewerPeerName))
        case .latencyStats(let l):
            latency = l
        case .directLinkOffer(_, let ssid, let passphrase):
            joinDirectLink(ssid: ssid, passphrase: passphrase, asViewer: true)
        case .directLinkUnavailable(_, let reason):
            Annex.log(.warn, "[network] direct Wi-Fi not available: \(reason)")
            directStatus = reason
            directReconnect = nil
            showHeldWindow()
        case .directLinkRequested:
            // A host asks this Mac to run the hotspot; only Windows can.
            Annex.log(.info, "[network] refused to run a hotspot: a Mac can't")
            viewer?.refuseDirectLink(reason: "This Mac can't run a hotspot. Start direct Wi-Fi from the Windows PC.")
        case .sessionEnded(_, let reason):
            if reason == "user" { onDirectWiFi = false }
            // Switching networks drops the old connection on purpose; reconnect instead of reporting it.
            if var d = directReconnect, d.attempts > 0 {
                d.attempts -= 1
                Annex.log(.info, "[network] reconnecting to \(d.name) over direct Wi-Fi in 2 s (\(d.attempts) tries left after this)")
                directReconnect = d
                DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in self?.reconnectDirect() }
                viewerStatus = nil
            } else {
                if directReconnect != nil {
                    Annex.log(.warn, "[network] gave up reconnecting to \(viewerPeerName) over direct Wi-Fi")
                    directStatus = "Couldn't reach \(viewerPeerName) on direct Wi-Fi. Connect again from Nearby."
                    directReconnect = nil
                }
                viewerStatus = reason == "user" ? nil : Self.friendlyReason(reason, peer: viewerPeerName, byCode: viewingByCode)
            }
            let w = viewerWindow
            viewerWindow = nil
            w?.onClose = nil
            w?.close()
            release(viewer)
            viewer = nil
            peerToPeerTunnel?.stop()
            peerToPeerTunnel = nil
            latency = nil
            controllingHost = false
            viewerPeerOS = nil
            copiedWhileAway = false
            viewerExtends = nil
            holdingWindow = false
        default:
            handleSessionEvent(e, viewer: true)
        }
    }

    /// The direct link didn't happen: show the picture over the current network after all.
    private func showHeldWindow() {
        guard holdingWindow else { return }
        holdingWindow = false
        Annex.log(.info, "[viewer] no direct Wi-Fi: showing the picture over the current network")
        viewerWindow?.show(fullScreen: settings.openFullScreen)
    }

    // MARK: Direct Wi-Fi

    /// Viewer: ask the other computer (a Windows PC) to start its hotspot for a direct link.
    func switchToDirectWiFi() {
        guard let viewer else { return }
        Annex.log(.info, "[network] asking \(viewerPeerName) to start direct Wi-Fi")
        directStatus = "Asking \(viewerPeerName) to start direct Wi-Fi…"
        viewer.requestDirectLink()
    }

    /// Joins the other computer's hotspot. As the viewer, reconnects to it afterwards; as the host,
    /// the viewer reconnects by itself once this Mac shows up on the hotspot.
    private func joinDirectLink(ssid: String, passphrase: String, asViewer: Bool) {
        directStatus = "Joining “\(ssid)”. The picture pauses for a moment."
        // Never the passphrase.
        Annex.log(.info, "[network] joining hotspot \"\(ssid)\" as the \(asViewer ? "viewer" : "host")")
        if asViewer, let fp = viewerFingerprint {
            directReconnect = (fp, viewerPeerName, extendMode, 4)
        }
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let error = DirectWiFi.join(ssid: ssid, passphrase: passphrase)
            DispatchQueue.main.async {
                guard let self else { return }
                if let error {
                    Annex.log(.warn, "[network] couldn't join \"\(ssid)\": \(DirectWiFi.redact(error, passphrase))")
                    self.directReconnect = nil
                    self.directStatus = "Couldn't join automatically. Join “\(ssid)” from the Wi-Fi menu (password: \(passphrase)), then connect again."
                    self.showHeldWindow()
                } else if !asViewer {
                    Annex.log(.info, "[network] on \"\(ssid)\"; the viewer reconnects by itself")
                    self.hostDirectWiFi = ssid
                    self.directStatus = "On direct Wi-Fi (“\(ssid)”)."
                } else if self.directReconnect != nil {
                    // This Mac left the old network, so the old session is dead: drop it now
                    // instead of waiting for it to time out, and reconnect over the hotspot.
                    Annex.log(.info, "[network] on \"\(ssid)\"; dropping the old session to reconnect over it")
                    self.disconnectViewer()
                    self.reconnectDirect()
                }
            }
        }
    }

    /// Joining a PC's hotspot by hand: for networks that keep computers from reaching each other,
    /// where no session can start to hand the hotspot over (the PC shows it under This PC → Direct Wi-Fi).
    @Published var pcHotspotName = ""
    @Published var pcHotspotPassword = ""
    @Published private(set) var joiningPCHotspot = false

    func joinPCHotspot() {
        let ssid = pcHotspotName.trimmingCharacters(in: .whitespaces)
        let passphrase = pcHotspotPassword.trimmingCharacters(in: .whitespaces)
        guard !ssid.isEmpty, !passphrase.isEmpty, !joiningPCHotspot else { return }
        joiningPCHotspot = true
        directStatus = "Joining “\(ssid)”…"
        Annex.log(.info, "[network] joining a PC's hotspot \"\(ssid)\" by hand")
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let error = DirectWiFi.join(ssid: ssid, passphrase: passphrase)
            DispatchQueue.main.async {
                guard let self else { return }
                self.joiningPCHotspot = false
                Annex.log(error == nil ? .info : .warn, "[network] hotspot \"\(ssid)\": "
                          + (error.map { "couldn't join: \(DirectWiFi.redact($0, passphrase))" } ?? "joined"))
                if let error {
                    self.directStatus = "Couldn't join “\(ssid)”: \(error)"
                } else if self.isHosting {
                    // The PC finds this Mac on its hotspot and connects as usual.
                    self.hostDirectWiFi = ssid
                    self.directStatus = "On “\(ssid)”. Connect from the PC: this Mac shows up in its Nearby list."
                } else {
                    self.manualAddress = "\(DirectWiFi.windowsHotspotAddress):47800"
                    self.addressFromHotspot = true
                    self.directStatus = "On “\(ssid)”. Show the PC's screen or extend it below."
                }
            }
        }
    }

    /// Connect by Code or Address starts empty each time it opens or closes: no code, address or
    /// hotspot login left from last time (only an address a joined PC hotspot filled in).
    func resetConnectSheet() {
        manualCode = ""
        if !addressFromHotspot { manualAddress = "" }
        pcHotspotName = ""
        pcHotspotPassword = ""
    }

    private func reconnectDirect() {
        guard let d = directReconnect else { return }
        Annex.log(.info, "[network] reconnecting to \(d.name) at \(DirectWiFi.windowsHotspotAddress) on direct Wi-Fi")
        extendMode = d.extend
        connect(address: "\(DirectWiFi.windowsHotspotAddress):47800", title: d.name, expectedFingerprint: d.fingerprint)
    }

    // MARK: What's new

    /// After an update, the notes of every version since the last one that ran here (shown once
    /// the main window is up). Remembers this version either way.
    private func checkWhatsNew() {
        let current = Annex.version
        // Versions before 0.4.0 never saved it: if Annex ran here before, that was 0.3.0.
        let last = settings.lastSeenVersion ?? (settings.usedBefore ? "0.3.0" : nil)
        if let last, last != current {
            let releases = Annex.whatsNew(since: last)
            if releases.isEmpty {
                Annex.log(.info, "[update] now \(current) (was \(last)), no release notes to show")
            } else {
                let versions = releases.map(\.version).joined(separator: ", ")
                Annex.log(.info, "[update] showing what's new for \(versions) (was \(last))")
                pendingWhatsNew = WhatsNew(releases: releases, all: false)
            }
        } else if last == nil {
            Annex.log(.info, "[update] first run of Annex \(current) here: no what's new")
        }
        if settings.lastSeenVersion != current { settings.lastSeenVersion = current }
    }

    /// The main window is up: show what's new after an update, once.
    func showPendingWhatsNew() {
        guard let notes = pendingWhatsNew else { return }
        pendingWhatsNew = nil
        whatsNew = notes
    }

    /// About → What's New: every release's notes.
    func showAllWhatsNew() {
        Annex.log(.info, "[update] What's New pressed")
        whatsNew = WhatsNew(releases: Annex.whatsNewAll(), all: true)
    }

    // MARK: Pairing

    private var pinPanels: [UInt64: PinPanel] = [:]

    // MARK: Clipboard

    /// Copied on this Mac: to the host this Mac controls (if copied elsewhere, once its window
    /// comes to the front, and only what's on the clipboard then), and to the viewers controlling
    /// this Mac.
    private func copied(mime: String, data: Data) {
        if viewer != nil, controllingHost {
            if viewerWindow?.isFocused == true {
                let sent = viewer?.sendClipboard(mime: mime, data: data) == true
                Annex.log(sent ? .debug : .warn, "[clipboard] \(sent ? "sent" : "couldn't send") \(mime), \(data.count) bytes to \(viewerPeerName)")
            } else {
                Annex.log(.debug, "[clipboard] held \(mime), \(data.count) bytes until the viewer window is in front")
                copiedWhileAway = true
            }
        } else if viewer != nil {
            Annex.log(.debug, "[clipboard] not sent to \(viewerPeerName): not controlling it")
        }
        host?.sendClipboard(mime: mime, data: data)
    }

    // MARK: Files

    /// Who dropped files go to: the host this Mac controls, else the viewers controlling it.
    private func updateNotch() {
        guard settings.notchDrop else { return notch.setTarget(nil, why: "notch drop is off") }
        if viewer != nil, controllingHost {
            notch.setTarget(label(viewerPeerName), why: "controlling it")
        } else if let name = host?.controllingViewer {
            notch.setTarget(label(name), why: "it controls this Mac")
        } else {
            notch.setTarget(nil, why: viewer != nil ? "viewing without control" : isHosting ? "no viewer controls this Mac" : "no session")
        }
    }

    private func send(files: [URL]) {
        let bytes = files.reduce(Int64(0)) { $0 + Int64((try? $1.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0) }
        let to = viewer != nil && controllingHost ? viewerPeerName : host?.controllingViewer ?? "nobody"
        Annex.log(.info, "[transfer] sending \(files.count) file(s), \(bytes) bytes, to \(to): "
                  + files.map(\.lastPathComponent).joined(separator: ", "))
        for url in files {
            do {
                if let viewer, controllingHost {
                    _ = try viewer.sendFile(url)
                } else {
                    try host?.sendFile(url)
                }
            } catch {
                Annex.log(.warn, "[transfer] couldn't send \(url.path): \(error.localizedDescription)")
                notify("Couldn't send \(url.lastPathComponent)", error.localizedDescription)
            }
        }
    }

    private func viewerFocused() {
        Annex.log(.debug, "[viewer] window in front")
        guard copiedWhileAway, controllingHost else { return }
        copiedWhileAway = false
        guard let item = clipboard.current() else {
            return Annex.log(.debug, "[clipboard] copied while away, but nothing to send now")
        }
        let sent = viewer?.sendClipboard(mime: item.mime, data: item.data) == true
        Annex.log(sent ? .debug : .warn, "[clipboard] re-read on focus: \(sent ? "sent" : "couldn't send") \(item.mime), \(item.data.count) bytes")
    }

    /// Pauses or resumes a file being sent from the notch. False: it isn't being sent any more.
    private func pauseTransfer(_ item: NotchDrop.Item, paused: Bool) -> Bool {
        if item.viewer {
            return viewer?.pauseTransfer(id: item.transferId, paused: paused) == true
        }
        return host?.pauseTransfer(session: item.session, id: item.transferId, paused: paused) == true
    }

    /// Unsend (outgoing) or Stop (incoming) from the notch; the transfer then ends as cancelled.
    private func cancelTransfer(_ item: NotchDrop.Item) {
        if item.viewer {
            viewer?.cancelTransfer(incoming: item.incoming, id: item.transferId)
        } else {
            host?.cancelTransfer(session: item.session, incoming: item.incoming, id: item.transferId)
        }
    }

    /// `viewer`: the event came from the viewer connection, not from sharing this Mac.
    private func handleSessionEvent(_ e: CoreEvent, viewer fromViewer: Bool = false) {
        defer { updateNotch() }
        switch e {
        case .clipboardReceived(_, let mime, let data):
            if settings.shareClipboard {
                clipboard.apply(mime: mime, data: data)
            } else {
                Annex.log(.debug, "[clipboard] ignored \(mime), \(data.count) bytes: sharing the clipboard is off")
            }
        case .transferStarted(let t):
            notch.transferStarted(session: t.sessionId, id: t.id, incoming: t.incoming, name: t.name, size: t.size, viewer: fromViewer)
        case .transferProgress(let session, let id, let incoming, let done):
            notch.transferProgress(session: session, id: id, incoming: incoming, done: done)
        case .transferEnded(let session, let id, let incoming, let ok, let path, let error):
            notch.transferEnded(session: session, id: id, incoming: incoming, ok: ok, path: path, error: error)
        case .internetCode, .internetJoined, .internetRelay, .internetCodeEnded:
            handleInternetEvent(e)
        case .directLinkRequested(let session):
            Annex.log(.info, "[network] refused to run a hotspot for session \(session): a Mac can't")
            host?.refuseDirectLink(session: session)
        case .directLinkUnavailable(_, let reason):
            // Host: the viewer couldn't start a hotspot.
            Annex.log(.warn, "[network] the viewer couldn't start direct Wi-Fi: \(reason)")
            directStatus = reason
        case .inputRequested(_, let name, let fingerprint):
            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.messageText = "Let \(name) control this Mac?"
            alert.informativeText = "\(name) wants to use this Mac's mouse and keyboard. You can change this in Paired Devices."
            alert.addButton(withTitle: "Allow")
            alert.addButton(withTitle: "Not Now")
            refreshTrusted()
            Annex.log(.info, "[input] asking the user: may \(name) control this Mac?")
            let allowed = alert.runModal() == .alertFirstButtonReturn
            Annex.log(.info, "[input] the user said \(allowed ? "Allow" : "Not Now") to \(name)")
            if allowed {
                if let peer = trusted.first(where: { $0.fingerprint == fingerprint }) {
                    setAllowInput(peer, true)
                } else {
                    Annex.log(.warn, "[input] \(name) isn't paired any more; nothing to allow")
                }
            }
        case .directLinkOffer(_, let ssid, let passphrase):
            // A Windows viewer runs the hotspot; this Mac (the host) joins it.
            joinDirectLink(ssid: ssid, passphrase: passphrase, asViewer: false)
        case .pairingPin(let p):
            // Host: show the PIN until pairing finishes or the user cancels. Never log the PIN.
            Annex.log(.info, "[pairing] showing a PIN for \(p.peerName) (request \(p.requestId))")
            let panel = PinPanel(pin: p.pin, peerName: p.peerName,
                                 fingerprint: settings.showTechnicalDetails ? p.fingerprintDisplay : nil) { [weak self] in
                Annex.log(.info, "[pairing] the user cancelled pairing with \(p.peerName)")
                self?.core?.cancelRequest(p.requestId)
            }
            pinPanels[p.requestId] = panel
            panel.show()
        case .pairingPinRequired(let r):
            promptForPin(r)
        case .pairingFinished(let f):
            pinPanels.removeValue(forKey: f.requestId)?.close()
            if !f.success {
                notify("Pairing with \(f.peerName) failed", "The PIN didn't match or pairing was cancelled.")
            }
            refreshTrusted()
        case .sessionStarted, .sessionEnded:
            refreshTrusted()
        default:
            break
        }
    }

    private func promptForPin(_ req: PairingPinRequired) {
        guard let core else { return }
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.messageText = "Pair with “\(req.peerName)”"
        alert.informativeText = "Enter the PIN shown on \(req.peerName)."
            + (settings.showTechnicalDetails ? "\n\nIdentity code: \(req.fingerprintDisplay)" : "")
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 160, height: 28))
        field.placeholderString = "******"
        field.font = .monospacedDigitSystemFont(ofSize: 18, weight: .medium)
        field.alignment = .center
        alert.accessoryView = field
        alert.addButton(withTitle: "Pair")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        Annex.log(.info, "[pairing] asking the user for \(req.peerName)'s PIN (request \(req.requestId))")
        if alert.runModal() == .alertFirstButtonReturn {
            // Its length only.
            let ok = core.submitPin(requestID: req.requestId, pin: field.stringValue)
            Annex.log(ok ? .info : .warn, "[pairing] PIN entered (\(field.stringValue.count) characters)"
                      + (ok ? "" : ", but the request had already ended"))
        } else {
            Annex.log(.info, "[pairing] the user cancelled entering the PIN")
            core.cancelRequest(req.requestId)
        }
    }

    private func notify(_ title: String, _ text: String) {
        Annex.log(.debug, "[app] alert: \(title)")
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = text
        alert.alertStyle = .informational
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }

    /// Host: let a trusted device control this Mac's mouse and keyboard (or stop it).
    func setAllowInput(_ peer: TrustedPeer, _ allow: Bool) {
        Annex.log(.info, "[input] \(allow ? "allow" : "stop") \(peer.name) using this Mac's mouse and keyboard")
        if allow && !InputInjector.isTrusted {
            Annex.log(.info, "[permissions] asking macOS for Accessibility")
            InputInjector.requestTrust()
        }
        do {
            if try core?.setAllowInput(peer.fingerprint, allow) == false {
                Annex.log(.warn, "[input] \(peer.name) isn't paired; nothing changed")
            }
        } catch {
            Annex.log(.warn, "[input] couldn't change control for \(peer.name): \(error.localizedDescription)")
        }
        refreshTrusted()
        refreshPermissions()
    }

    /// Session-end codes from the core, in words.
    static func friendlyReason(_ reason: String, peer: String, byCode: Bool = false) -> String {
        switch reason {
        case "extend_not_supported":
            return "\(peer) can't extend its display. Set up Extend on that computer, or use Show Its Screen."
        case "busy": return "\(peer) is already sharing with 4 computers."
        case "pairing_failed": return "Wrong PIN. Try again."
        case "pairing_cancelled": return "Pairing was cancelled on the other computer."
        case "pairing_locked": return "Too many wrong PINs. Try again in 10 minutes."
        case "host_stopped": return "\(peer) stopped sharing."
        case "host_ended_session": return "\(peer) ended the session."
        case "unknown_code": return "That code doesn't work. Codes work once, for 10 minutes: ask for a new one."
        case "too_many_attempts": return "Too many codes tried. Wait 10 minutes, then try again."
        case "code_expired": return "The code expired. Ask for a new one."
        case "closed", "connection closed": return "Disconnected from \(peer)."
        default:
            let r = reason.lowercased()
            if byCode && (r.contains("timed out") || r.contains("timeout")) {
                return "Couldn't connect across the internet. One of the networks blocks direct connections "
                    + "and no relay was available. Add your own relay in Settings → Advanced on the sharing computer."
            }
            if byCode && (r.contains("rendezvous") || r.contains("i/o") || r.contains("websocket")) {
                return Self.friendlyInternetError(reason)
            }
            if r.contains("timed out") || r.contains("timeout") {
                // No answer at all: also what a network that keeps computers apart looks like.
                return "Couldn't reach \(peer). Check that it's sharing and on the same network. "
                    + "School and office Wi-Fi often blocks computers from reaching each other: "
                    + "for a Windows PC, use Direct Wi-Fi to a PC below."
            }
            if r.contains("refused") {
                return "Couldn't reach \(peer). Check that it's sharing and on the same network."
            }
            return reason
        }
    }

    func revoke(_ peer: TrustedPeer) {
        Annex.log(.info, "[pairing] forgetting \(peer.name) (\(peer.fingerprint.prefix(16))…)")
        do {
            try core?.untrust(peer.fingerprint)
        } catch {
            Annex.log(.warn, "[pairing] couldn't forget \(peer.name): \(error.localizedDescription)")
        }
        refreshTrusted()
    }

    // MARK: Discovery

    private func handleDiscovery(_ e: CoreEvent) {
        switch e {
        case .peerFound(let p):
            lanPeers.removeAll { $0.instance == p.instance }
            lanPeers.append(p)
            rebuildNearby()
        case .peerLost(let instance):
            lanPeers.removeAll { $0.instance == instance }
            rebuildNearby()
        default:
            break
        }
    }

    /// The network's list, plus each Mac found only on peer-to-peer Wi-Fi (one row per computer:
    /// a Mac on both shows once, and connecting tries both ways).
    private func rebuildNearby() {
        let known = Set(lanPeers.compactMap(\.fingerprint))
        let peerOnly = peerToPeerHosts.filter { !known.contains($0.fingerprint) }.map(Self.peerInfo)
        discovered = (lanPeers + peerOnly).sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        if case .peer(let instance) = pane, !discovered.contains(where: { $0.instance == instance }) { pane = .home }
    }

    nonisolated static func peerInfo(_ h: PeerToPeerHost) -> PeerInfo {
        PeerInfo(instance: "p2p:\(h.fingerprint)", name: h.name, addresses: [], port: 0, fingerprint: h.fingerprint,
                 protoMajor: 1, sharing: true, extend: h.extend)
    }

    /// Looks for Macs on peer-to-peer Wi-Fi only while Nearby or a computer's page is open, and not
    /// during a session: browsing keeps peer-to-peer Wi-Fi on, which takes some speed from the
    /// normal Wi-Fi link.
    private func updatePeerToPeerBrowsing(pane: Pane, on: Bool) {
        let wanted: Bool
        switch pane {
        case .home, .connect, .peer: wanted = on
        default: wanted = false
        }
        if wanted, peerToPeerBrowser == nil {
            Annex.log(.debug, "[network] looking for Macs on peer-to-peer Wi-Fi")
            peerToPeerBrowser = PeerToPeerBrowser(known: { [weak self] in
                Dictionary((self?.trusted ?? []).map { ($0.fingerprint, $0.name) }, uniquingKeysWith: { a, _ in a })
            }) { [weak self] hosts in
                guard let self else { return }
                let own = self.core?.fingerprint
                self.peerToPeerHosts = hosts.filter { $0.fingerprint != own }
                self.rebuildNearby()
            }
        } else if !wanted, let b = peerToPeerBrowser {
            Annex.log(.debug, "[network] stopped looking on peer-to-peer Wi-Fi (\(on ? "page closed" : "off or in a session"))")
            b.stop()
            peerToPeerBrowser = nil
            peerToPeerHosts = []
            rebuildNearby()
        }
    }

    func quit() {
        Annex.log(.info, "[app] quit")
        disconnectViewer()
        stopHosting()
        // Let viewers get the bye before the process ends.
        HostController.teardown.sync {}
        NSApp.terminate(nil)
    }
}

/// The "What's new" dialog's notes, newest release first.
struct WhatsNew: Identifiable {
    let id = UUID()
    let releases: [ReleaseNotes]
    /// Every release (About → What's New) rather than the ones since the last update.
    let all: Bool
}

/// This Mac's part in a live session. Every status says it from this Mac's side, so it reads
/// right even when both computers have the same name.
enum SessionRole: Equatable {
    /// This Mac's screen (or a display it added, with Extend) shows on `with`.
    case sharing(with: String, extend: Bool)
    /// This Mac shows `from`'s screen, or the extra display `from` added.
    case viewing(from: String, extend: Bool)
}
