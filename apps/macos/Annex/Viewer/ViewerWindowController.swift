import AVFoundation
import AppKit

/// Shows a received H.264 stream in a window (full screen by default). Decoding and display
/// are done by AVSampleBufferDisplayLayer (VideoToolbox underneath).
final class ViewerWindowController: NSWindowController, NSWindowDelegate {
    private let videoView = VideoView()
    private let queue = DispatchQueue(label: "annex.viewer.render", qos: .userInteractive)
    private var builder = H264SampleBuilder()
    private var waitingForKeyframe = true
    private var viewer: ViewerHandle?

    var onClose: (() -> Void)?

    init(title: String) {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1280, height: 720),
                              styleMask: [.titled, .closable, .resizable, .miniaturizable],
                              backing: .buffered, defer: false)
        window.title = title
        window.contentView = videoView
        window.collectionBehavior = [.fullScreenPrimary]
        window.backgroundColor = .black
        window.isReleasedWhenClosed = false
        window.center()
        super.init(window: window)
        window.delegate = self
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func attach(_ viewer: ViewerHandle) {
        queue.async { self.viewer = viewer }
    }

    func show(fullScreen: Bool) {
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
        window?.makeFirstResponder(videoView)
        if fullScreen { window?.toggleFullScreen(nil) }
    }

    /// Called from the core's thread with a copy of the access unit.
    func enqueue(_ data: Data, frameID: UInt32, keyframe: Bool) {
        queue.async { [self] in
            if waitingForKeyframe && !keyframe { return }
            guard let sample = builder.sampleBuffer(from: data) else {
                viewer?.requestKeyframe()
                return
            }
            waitingForKeyframe = false
            let renderer = videoView.displayLayer.sampleBufferRenderer
            if renderer.status == .failed || renderer.requiresFlushToResumeDecoding {
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

    func windowWillClose(_ notification: Notification) {
        onClose?()
    }
}

final class VideoView: NSView {
    let displayLayer = AVSampleBufferDisplayLayer()

    override init(frame: NSRect) {
        super.init(frame: frame)
        displayLayer.videoGravity = .resizeAspect
        displayLayer.backgroundColor = NSColor.black.cgColor
        layer = displayLayer
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override var acceptsFirstResponder: Bool { true }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { // Esc
            window?.performClose(nil)
        } else {
            super.keyDown(with: event)
        }
    }
}
