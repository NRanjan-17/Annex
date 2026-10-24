import SwiftUI

// MARK: Look
//
// Black and glass. A near-black backdrop with a little slow-moving light, so the Liquid Glass has
// something to bend; white for emphasis; the only colour is the small green dot of something live.
// On macOS before 26 the glass falls back to the system's thin material, so the layout is the same.

extension Color {
    /// Emphasis: selection, the main action, this Mac while it shares.
    static let annex = Color.white
    /// A live connection: a session, a computer that's sharing.
    static let annexLive = Color(red: 0.24, green: 0.82, blue: 0.52)
}

/// Black, with two faint pools of light drifting very slowly behind the glass.
struct Backdrop: View {
    /// A touch brighter while a session is live.
    var live = false

    var body: some View {
        TimelineView(.animation(minimumInterval: 1 / 20)) { context in
            let t = context.date.timeIntervalSinceReferenceDate
            GeometryReader { geo in
                let w = geo.size.width, h = geo.size.height
                ZStack {
                    Color(white: 0.02)
                    light(at: CGPoint(x: w * (0.30 + 0.08 * sin(t / 11)), y: h * (0.20 + 0.06 * cos(t / 13))),
                          radius: max(w, h) * 0.55, strength: live ? 0.16 : 0.11)
                    light(at: CGPoint(x: w * (0.78 + 0.06 * cos(t / 9)), y: h * (0.85 + 0.05 * sin(t / 12))),
                          radius: max(w, h) * 0.45, strength: live ? 0.10 : 0.07)
                }
            }
        }
        .ignoresSafeArea()
        .accessibilityHidden(true)
    }

    private func light(at p: CGPoint, radius: CGFloat, strength: Double) -> some View {
        RadialGradient(colors: [Color.white.opacity(strength), .clear], center: .center, startRadius: 0, endRadius: radius)
            .frame(width: radius * 2, height: radius * 2)
            .position(p)
    }
}

extension View {
    /// A pane of Liquid Glass in a rounded rectangle (material on older macOS).
    @ViewBuilder func glassPanel(radius: CGFloat = 20, tint: Color? = nil, interactive: Bool = false) -> some View {
        let shape = RoundedRectangle(cornerRadius: radius, style: .continuous)
        if #available(macOS 26.0, *) {
            self.glassEffect(Glass.regular.tint(tint).interactive(interactive), in: shape)
        } else {
            self.background(.ultraThinMaterial, in: shape)
                .overlay(shape.strokeBorder(Color.white.opacity(0.18)))
        }
    }

    /// A glass capsule (pills, chips, small controls).
    @ViewBuilder func glassCapsule(tint: Color? = nil, interactive: Bool = false) -> some View {
        if #available(macOS 26.0, *) {
            self.glassEffect(Glass.regular.tint(tint).interactive(interactive), in: Capsule())
        } else {
            self.background(.ultraThinMaterial, in: Capsule())
                .overlay(Capsule().strokeBorder(Color.white.opacity(0.18)))
        }
    }

    /// A glass circle (device nodes on the radar).
    @ViewBuilder func glassCircle(tint: Color? = nil) -> some View {
        if #available(macOS 26.0, *) {
            self.glassEffect(Glass.regular.tint(tint).interactive(), in: Circle())
        } else {
            self.background(.ultraThinMaterial, in: Circle())
                .overlay(Circle().strokeBorder(Color.white.opacity(0.2)))
        }
    }

    /// The main action on a screen: glass, filled with the accent.
    @ViewBuilder func primaryGlassButton() -> some View {
        if #available(macOS 26.0, *) {
            self.buttonStyle(.glassProminent).tint(Color.white.opacity(0.92)).foregroundStyle(.black).controlSize(.large)
        } else {
            self.buttonStyle(.borderedProminent).tint(.white).foregroundStyle(.black).controlSize(.large)
        }
    }

    /// Every other action: clear glass.
    @ViewBuilder func secondaryGlassButton() -> some View {
        if #available(macOS 26.0, *) {
            self.buttonStyle(.glass).controlSize(.large)
        } else {
            self.buttonStyle(.bordered).controlSize(.large)
        }
    }
}

/// Groups glass shapes so neighbours blend and morph into each other (macOS 26).
struct GlassGroup<Content: View>: View {
    var spacing: CGFloat = 16
    @ViewBuilder var content: Content

    var body: some View {
        if #available(macOS 26.0, *) {
            GlassEffectContainer(spacing: spacing) { content }
        } else {
            content
        }
    }
}

/// A small uppercase label over a group, like a section of a control panel.
struct Eyebrow: View {
    let text: String

    var body: some View {
        Text(text.uppercased())
            .font(.system(size: 10.5, weight: .semibold))
            .tracking(1.2)
            .foregroundStyle(.secondary)
    }
}

/// The app's own icon, wherever the UI names Annex or this computer.
struct AppIcon: View {
    var size: CGFloat = 32

    var body: some View {
        Image(nsImage: NSApp.applicationIconImage)
            .resizable()
            .interpolation(.high)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

/// A dot that breathes while something is live.
struct LiveDot: View {
    var color: Color = .annexLive
    var size: CGFloat = 8
    @State private var on = false

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: size, height: size)
            .background(Circle().fill(color.opacity(0.35)).scaleEffect(on ? 2.6 : 1).opacity(on ? 0 : 1))
            .onAppear { withAnimation(.easeOut(duration: 1.6).repeatForever(autoreverses: false)) { on = true } }
            .accessibilityHidden(true)
    }
}
