import AVFoundation
import AppKit

/// Shows a received H.264 stream in a window (full screen by default). Decoding and display
/// are done by AVSampleBufferDisplayLayer (VideoToolbox underneath). While the host allows
/// input, the mouse and keyboard over the video control the host.
final class ViewerWindowController: NSWindowController, NSWindowDelegate {
    private let videoView = VideoView()
    private let queue = DispatchQueue(label: "annex.viewer.render", qos: .userInteractive)
    private var builder = H264SampleBuilder()
    private var waitingForKeyframe = true
    private var viewer: ViewerHandle?
    // For annex.log (render queue only): a black window otherwise leaves no trace on the viewer.
    private var framesReceived = 0
    private var loggedFirstFrame = false
    private var loggedBadFrame = false
    private var sized = false
    /// Full screen, Annex's own: the window covers its screen with the menu bar and Dock hidden.
    /// macOS full screen moves the window to a Space of its own, and closing it slid back to the
    /// desktop for over half a second; this opens and closes at once.
    private(set) var covering = false
    private var windowedFrame: NSRect?
    private static let windowedStyle: NSWindow.StyleMask = [.titled, .closable, .resizable, .miniaturizable]

    var onClose: (() -> Void)?
    /// The window became the one taking this Mac's keyboard.
    var onFocus: (() -> Void)?
    var isFocused: Bool { window?.isKeyWindow == true }

    /// `title` names the other computer (or the code, until it's known).
    init(title: String) {
        // The title bar sits above the picture, not over it: the other computer's top edge (its
        // menu bar, its title bars) stays visible and clickable.
        let window = ViewerWindow(contentRect: NSRect(x: 0, y: 0, width: 1280, height: 720),
                                  styleMask: Self.windowedStyle, backing: .buffered, defer: false)
        window.title = title
        window.titlebarAppearsTransparent = true
        window.appearance = NSAppearance(named: .darkAqua)
        window.contentView = videoView
        window.collectionBehavior = [.fullScreenPrimary]
        window.backgroundColor = .black
        window.isReleasedWhenClosed = false
        // No fade: opening and closing are instant (the fade alone was over a quarter of a second).
        window.animationBehavior = .none
        window.acceptsMouseMovedEvents = true
        window.center()
        super.init(window: window)
        window.toggle = { [weak self] in self.map { $0.setCovering(!$0.covering) } }
        window.delegate = self
        window.addTitlebarAccessoryViewController(disconnectButton)
        videoView.onLeave = { [weak window] in window?.performClose(nil) }
    }

    private lazy var disconnectButton = disconnectAccessory()

    private func disconnectAccessory() -> NSTitlebarAccessoryViewController {
        let button = PointingButton(title: "Disconnect", target: self, action: #selector(disconnect))
        button.bezelStyle = .push
        button.controlSize = .small
        button.font = .systemFont(ofSize: NSFont.smallSystemFontSize, weight: .medium)
        button.toolTip = "Close this window and end the session"
        let holder = NSView()
        holder.translatesAutoresizingMaskIntoConstraints = false
        button.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(button)
        NSLayoutConstraint.activate([
            button.centerYAnchor.constraint(equalTo: holder.centerYAnchor),
            button.leadingAnchor.constraint(equalTo: holder.leadingAnchor),
            button.trailingAnchor.constraint(equalTo: holder.trailingAnchor, constant: -10),
        ])
        holder.frame.size = NSSize(width: button.fittingSize.width + 10, height: 28)
        let accessory = NSTitlebarAccessoryViewController()
        accessory.view = holder
        accessory.layoutAttribute = .trailing
        return accessory
    }

    @objc private func disconnect() { window?.performClose(nil) }

    /// What the window shows ("Viewing Name"), and how it's reached ("Over the internet").
    func setPeer(_ title: String, detail: String) {
        window?.title = title
        window?.subtitle = detail
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func attach(_ viewer: ViewerHandle) {
        videoView.viewer = viewer
        queue.async { self.viewer = viewer }
    }

    func show(fullScreen: Bool) {
        // Become a regular app first: switching while the borderless window is up hides it.
        if NSApp.activationPolicy() != .regular { NSApp.setActivationPolicy(.regular) }
        if fullScreen { setCovering(true) }
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
        window?.makeFirstResponder(videoView)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// In and out of full screen (the green button, ⌃⌘F and View → Enter Full Screen come here).
    func setCovering(_ on: Bool) {
        guard let window, on != covering else { return }
        covering = on
        if on {
            windowedFrame = window.frame
            window.styleMask = [.borderless]
            window.setFrame((window.screen ?? NSScreen.main)?.frame ?? window.frame, display: true)
            // There's no title bar, so no Disconnect button: say how to leave.
            if window.isVisible {
                videoView.showHUD("Full screen", detail: videoView.controlling ? "Press ⌃⌥⇧Q to disconnect" : "Press Esc to disconnect")
            }
        } else {
            window.styleMask = Self.windowedStyle
            window.titlebarAppearsTransparent = true
            // Going borderless drops the title bar's Disconnect button.
            if !window.titlebarAccessoryViewControllers.contains(disconnectButton) {
                window.addTitlebarAccessoryViewController(disconnectButton)
            }
            if let windowedFrame { window.setFrame(windowedFrame, display: true) }
        }
        window.makeFirstResponder(videoView)
        updatePresentation()
    }

    /// Hides the menu bar and Dock while the covering window is in front; they come back on hover.
    /// Auto-hide alone leaves the menu bar showing for a few seconds, so it's hidden first.
    private func updatePresentation() {
        guard covering && window?.isKeyWindow == true else {
            NSApp.presentationOptions = []
            return
        }
        NSApp.presentationOptions = [.hideDock, .hideMenuBar]
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in
            guard let self, self.covering, self.window?.isKeyWindow == true else { return }
            NSApp.presentationOptions = [.autoHideDock, .autoHideMenuBar]
        }
    }

    /// The stream's size, for mapping the pointer through the letterbox.
    func setStreamSize(width: Int, height: Int) {
        videoView.streamSize = CGSize(width: width, height: height)
        guard let window, width > 0, height > 0, !covering else { return }
        // Shape the window to the picture, so there are no black bars to resize away. Sized once:
        // a later resolution change keeps the size the user picked.
        let size = NSSize(width: width, height: height)
        window.contentAspectRatio = size
        guard !sized else { return }
        sized = true
        let visible = (window.screen ?? NSScreen.main)?.visibleFrame.size ?? NSSize(width: 1440, height: 900)
        let scale = min(window.contentLayoutRect.width / size.width,
                        (visible.height - 60) / size.height, (visible.width - 40) / size.width)
        window.setContentSize(NSSize(width: (size.width * scale).rounded(), height: (size.height * scale).rounded()))
    }

    /// Extend to a host that understands pointer movement: send this Mac's mouse movement, so the
    /// host's pointer can leave this screen for its own, like a real monitor.
    func setRelativePointer(_ on: Bool) {
        videoView.relative = on
    }

    /// Scroll the other way on the host, for Windows and Linux, which don't scroll like a Mac.
    func setReverseScroll(_ on: Bool) {
        videoView.reverseScroll = on
    }

    /// The host allowed (or revoked) control by this Mac's mouse and keyboard.
    func setControlling(_ on: Bool, hostName: String) {
        videoView.controlling = on
        if on {
            videoView.showHUD("Controlling \(hostName)", detail: "Press ⌃⌥⇧Q to disconnect")
        } else {
            videoView.showHUD("View only", detail: "\(hostName) isn't sharing its keyboard and mouse. Esc disconnects.")
        }
    }

    /// Called from the core's thread with a copy of the access unit.
    func enqueue(_ data: Data, frameID: UInt32, keyframe: Bool) {
        queue.async { [self] in
            framesReceived += 1
            if waitingForKeyframe && !keyframe { return }
            guard let sample = builder.sampleBuffer(from: data) else {
                if !loggedBadFrame {
                    loggedBadFrame = true
                    Annex.log("viewer: frame \(frameID) (\(data.count) bytes, key \(keyframe)) could not be read as H.264", error: true)
                }
                viewer?.requestKeyframe()
                return
            }
            waitingForKeyframe = false
            if !loggedFirstFrame {
                loggedFirstFrame = true
                Annex.log("viewer: first picture after \(framesReceived) frames received")
            }
            let renderer = videoView.displayLayer.sampleBufferRenderer
            if renderer.status == .failed || renderer.requiresFlushToResumeDecoding {
                Annex.log("viewer: decoder needs a reset (\(renderer.error?.localizedDescription ?? "no error given")); "
                          + "asking for a keyframe", error: renderer.status == .failed)
                renderer.flush()
                waitingForKeyframe = true
                viewer?.requestKeyframe()
                return
            }
            renderer.enqueue(sample)
            // Approximation: the layer displays immediately-flagged samples on the next refresh.
            viewer?.framePresented(frameID)
        }
    }

    func windowDidBecomeKey(_ notification: Notification) {
        videoView.updatePointerLock()
        updatePresentation()
        onFocus?()
    }

    func windowWillClose(_ notification: Notification) {
        queue.async { [self] in
            if !loggedFirstFrame { Annex.log("viewer: closed without a picture; \(framesReceived) frames received", error: true) }
        }
        videoView.releaseHeldInput()
        videoView.relative = false  // frees this Mac's pointer
        covering = false
        updatePresentation()
        onClose?()
    }

    func windowDidResignKey(_ notification: Notification) {
        // Keys released while another window has focus would otherwise stay down on the host.
        videoView.releaseHeldInput()
        videoView.updatePointerLock()
        updatePresentation()
    }
}

/// The viewer's window. Borderless while covering the screen, so it must say it can take keys,
/// and full screen means Annex's own (see ViewerWindowController.setCovering).
final class ViewerWindow: NSWindow {
    var toggle: (() -> Void)?

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
    override func toggleFullScreen(_ sender: Any?) { toggle?() }
    /// A borderless window has no close button, and AppKit ignores performClose without one.
    override func performClose(_ sender: Any?) {
        if styleMask.contains(.closable) {
            super.performClose(sender)
        } else {
            orderOut(nil)
            close()
        }
    }
}

final class VideoView: NSView {
    let displayLayer = AVSampleBufferDisplayLayer()
    weak var viewer: ViewerHandle?
    var streamSize = CGSize(width: 16, height: 9)
    var controlling = false {
        didSet {
            if !controlling { releaseHeldInput() }
            updatePointerLock()
        }
    }
    /// Flip scroll direction (see ViewerWindowController.setReverseScroll).
    var reverseScroll = false
    /// Send mouse movement instead of positions (see ViewerWindowController.setRelativePointer).
    var relative = false {
        didSet { updatePointerLock() }
    }
    /// This Mac's pointer is held still and hidden while its movement goes to the host.
    private var pointerLocked = false

    /// Locks this Mac's pointer while it sends movement (controlling, relative, window in front);
    /// frees it otherwise.
    func updatePointerLock() {
        let lock = controlling && relative && window?.isKeyWindow == true
        guard lock != pointerLocked else { return }
        pointerLocked = lock
        CGAssociateMouseAndMouseCursorPosition(lock ? 0 : 1)
        if lock { NSCursor.hide() } else { NSCursor.unhide() }
    }

    private func sendMovement(_ event: NSEvent) {
        guard let e = event.cgEvent else { return }
        let dx = Int32(e.getIntegerValueField(.mouseEventDeltaX))
        let dy = Int32(e.getIntegerValueField(.mouseEventDeltaY))
        if dx != 0 || dy != 0 { viewer?.sendMouseDelta(dx: dx, dy: dy) }
    }

    private func pointerMoved(_ event: NSEvent) {
        guard controlling else { return }
        if relative { sendMovement(event) } else { sendPosition(event) }
    }
    var onLeave: (() -> Void)?

    private var heldKeys: Set<UInt16> = []
    private var heldButtons: Set<UInt32> = []
    private let hud = HUDView()
    private var hudTimer: Timer?
    private var scrollRest = CGPoint.zero

    override init(frame: NSRect) {
        super.init(frame: frame)
        displayLayer.videoGravity = .resizeAspect
        displayLayer.backgroundColor = NSColor.black.cgColor
        layer = displayLayer
        wantsLayer = true
        hud.alphaValue = 0
        hud.translatesAutoresizingMaskIntoConstraints = false
        addSubview(hud)
        NSLayoutConstraint.activate([
            hud.centerXAnchor.constraint(equalTo: centerXAnchor),
            hud.topAnchor.constraint(equalTo: topAnchor, constant: 16),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: bounds, options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect], owner: self))
    }

    func showHUD(_ title: String, detail: String) {
        hud.set(title: title, detail: detail)
        NSAnimationContext.runAnimationGroup { $0.duration = 0.25; hud.animator().alphaValue = 1 }
        hudTimer?.invalidate()
        hudTimer = Timer.scheduledTimer(withTimeInterval: 3.5, repeats: false) { [weak self] _ in
            NSAnimationContext.runAnimationGroup { $0.duration = 0.6; self?.hud.animator().alphaValue = 0 }
        }
    }

    // MARK: Pointer

    /// The picture's rectangle inside the view (aspect fit).
    private var videoRect: CGRect {
        AVMakeRect(aspectRatio: streamSize, insideRect: bounds)
    }

    private func sendPosition(_ event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        let r = videoRect
        guard r.width > 1, r.height > 1 else { return }
        let x = min(max((p.x - r.minX) / (r.width - 1), 0), 1)
        // View coordinates grow upwards; the stream's grow downwards.
        let y = min(max(1 - (p.y - r.minY) / (r.height - 1), 0), 1)
        viewer?.sendMouseMove(x: x, y: y)
    }

    private func button(_ b: UInt32, _ event: NSEvent, down: Bool) {
        guard controlling else { return }
        if !relative { sendPosition(event) }
        if down { heldButtons.insert(b) } else { heldButtons.remove(b) }
        viewer?.sendMouseButton(b, down: down)
    }

    override func mouseMoved(with event: NSEvent) { pointerMoved(event) }
    override func mouseDragged(with event: NSEvent) { pointerMoved(event) }
    override func rightMouseDragged(with event: NSEvent) { pointerMoved(event) }
    override func otherMouseDragged(with event: NSEvent) { pointerMoved(event) }
    override func mouseDown(with event: NSEvent) { button(0, event, down: true) }
    override func mouseUp(with event: NSEvent) { button(0, event, down: false) }
    override func rightMouseDown(with event: NSEvent) { button(1, event, down: true) }
    override func rightMouseUp(with event: NSEvent) { button(1, event, down: false) }
    override func otherMouseDown(with event: NSEvent) { button(2, event, down: true) }
    override func otherMouseUp(with event: NSEvent) { button(2, event, down: false) }

    override func scrollWheel(with event: NSEvent) {
        guard controlling else { return super.scrollWheel(with: event) }
        // Precise (trackpad) deltas are pixels; wheel deltas are lines of roughly 10 pixels.
        // Only natural scrolling is reversed; with it off, this Mac already scrolls like the PC.
        let flip = reverseScroll && event.isDirectionInvertedFromDevice
        let scale: CGFloat = (event.hasPreciseScrollingDeltas ? 1 : 10) * (flip ? -1 : 1)
        scrollRest.x += event.scrollingDeltaX * scale
        scrollRest.y += event.scrollingDeltaY * scale
        let dx = scrollRest.x.rounded(.towardZero), dy = scrollRest.y.rounded(.towardZero)
        scrollRest.x -= dx
        scrollRest.y -= dy
        if dx != 0 || dy != 0 { viewer?.sendScroll(dx: Int32(dx), dy: Int32(dy)) }
    }

    // MARK: Keyboard

    private static let leaveKey: UInt16 = 0x0C  // Q

    private func isLeaveShortcut(_ event: NSEvent) -> Bool {
        event.keyCode == Self.leaveKey && event.modifierFlags.isSuperset(of: [.control, .option, .shift])
    }

    private func key(_ code: UInt16, down: Bool) {
        if down { heldKeys.insert(code) } else { heldKeys.remove(code) }
        viewer?.sendKey(macKeyCode: code, down: down)
    }

    override func keyDown(with event: NSEvent) {
        if controlling {
            if isLeaveShortcut(event) { return leave() }
            // Repeats are forwarded too: injected keys don't auto-repeat on the host.
            key(event.keyCode, down: true)
        } else if event.keyCode == 53 { // Esc
            window?.performClose(nil)
        } else {
            super.keyDown(with: event)
        }
    }

    override func keyUp(with event: NSEvent) {
        if controlling { key(event.keyCode, down: false) } else { super.keyUp(with: event) }
    }

    /// ⌘-shortcuts reach the menu before keyDown; send them to the host instead.
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard controlling, window?.isKeyWindow == true, event.type == .keyDown else { return super.performKeyEquivalent(with: event) }
        if isLeaveShortcut(event) {
            leave()
        } else {
            key(event.keyCode, down: true)
            // The matching keyUp isn't delivered for ⌘-combinations; release right away.
            key(event.keyCode, down: false)
        }
        return true
    }

    /// Modifier keys arrive as flag changes; work out which one moved.
    override func flagsChanged(with event: NSEvent) {
        guard controlling else { return super.flagsChanged(with: event) }
        let code = event.keyCode
        let mask: NSEvent.ModifierFlags? = switch code {
        case 0x38, 0x3C: .shift
        case 0x3B, 0x3E: .control
        case 0x3A, 0x3D: .option
        case 0x37, 0x36: .command
        case 0x39: .capsLock
        default: nil
        }
        guard let mask else { return }
        if mask == .capsLock {
            // Caps Lock toggles on each press; send a full press.
            key(code, down: true)
            key(code, down: false)
        } else {
            key(code, down: event.modifierFlags.contains(mask) && !heldKeys.contains(code))
        }
    }

    private func leave() {
        releaseHeldInput()
        onLeave?()
    }

    func releaseHeldInput() {
        for k in heldKeys { viewer?.sendKey(macKeyCode: k, down: false) }
        for b in heldButtons { viewer?.sendMouseButton(b, down: false) }
        heldKeys.removeAll()
        heldButtons.removeAll()
    }
}

/// A rounded, blurred banner shown over the video for a few seconds.
private final class HUDView: NSVisualEffectView {
    private let title = NSTextField(labelWithString: "")
    private let detail = NSTextField(labelWithString: "")

    override init(frame: NSRect) {
        super.init(frame: frame)
        material = .hudWindow
        blendingMode = .withinWindow
        state = .active
        wantsLayer = true
        layer?.cornerRadius = 14
        layer?.masksToBounds = true
        title.font = .systemFont(ofSize: 15, weight: .semibold)
        title.textColor = .white
        detail.font = .systemFont(ofSize: 12)
        detail.textColor = .white.withAlphaComponent(0.75)
        let stack = NSStackView(views: [title, detail])
        stack.orientation = .vertical
        stack.spacing = 2
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -20),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 10),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -10),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func set(title t: String, detail d: String) {
        title.stringValue = t
        detail.stringValue = d
    }
}

/// A button that shows the pointing hand, like every other clickable thing in Annex.
private final class PointingButton: NSButton {
    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .pointingHand)
    }
}
