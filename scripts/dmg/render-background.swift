// Renders the background of the Annex disk image window, so it can be regenerated and tweaked in code.
//
//   swift scripts/dmg/render-background.swift <out.png> [scale]
//
// The window is 660 x 360 points; the image is taller (420) so nothing below it ever shows if
// Finder adds its path and status bars (global settings). Scale 2 renders the Retina version. Positions must match
// scripts/dmg/settings.py. Finder draws the icon labels itself, in the system appearance's text
// colour… which is always black over a background picture, so the picture is light.
import AppKit
import CoreGraphics

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write("usage: render-background.swift <out.png> [scale]\n".data(using: .utf8)!)
    exit(2)
}
let out = URL(fileURLWithPath: args[1])
let scale = args.count >= 3 ? CGFloat(Double(args[2]) ?? 1) : 1
let (width, height): (CGFloat, CGFloat) = (660, 420)
let iconY: CGFloat = 170

func rgb(_ hex: UInt32, _ a: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255, alpha: a)
}

let space = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CGContext(data: nil, width: Int(width * scale), height: Int(height * scale), bitsPerComponent: 8,
                    bytesPerRow: 0, space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
// Points with a top-left origin, like Finder's icon positions.
ctx.translateBy(x: 0, y: height * scale)
ctx.scaleBy(x: scale, y: -scale)

// Background: a soft off-white gradient, a touch darker towards the bottom.
let background = CGGradient(colorsSpace: space, colors: [rgb(0xFBFBFD), rgb(0xEAEAEE)] as CFArray,
                            locations: [0, 1])!
ctx.drawLinearGradient(background, start: CGPoint(x: 0, y: 0), end: CGPoint(x: 0, y: height), options: [])

// A faint pool of shade behind each icon, so the two read as a pair.
for x: CGFloat in [180, 480] {
    let glow = CGGradient(colorsSpace: space, colors: [rgb(0x000000, 0.045), rgb(0x000000, 0)] as CFArray,
                          locations: [0, 1])!
    ctx.drawRadialGradient(glow, startCenter: CGPoint(x: x, y: iconY), startRadius: 0,
                           endCenter: CGPoint(x: x, y: iconY), endRadius: 100, options: [])
}

// Arrow from the app to Applications.
ctx.setStrokeColor(rgb(0x000000, 0.32))
ctx.setLineWidth(2.5)
ctx.setLineCap(.round)
ctx.setLineJoin(.round)
ctx.setLineDash(phase: 0, lengths: [1, 9])
ctx.move(to: CGPoint(x: 272, y: iconY))
ctx.addLine(to: CGPoint(x: 380, y: iconY))
ctx.strokePath()
ctx.setLineDash(phase: 0, lengths: [])
ctx.move(to: CGPoint(x: 377, y: iconY - 10))
ctx.addLine(to: CGPoint(x: 388, y: iconY))
ctx.addLine(to: CGPoint(x: 377, y: iconY + 10))
ctx.strokePath()

// Caption. AppKit text drawing wants a flipped NSGraphicsContext over our top-left canvas.
NSGraphicsContext.current = NSGraphicsContext(cgContext: ctx, flipped: true)
func text(_ s: String, size: CGFloat, weight: NSFont.Weight, color: NSColor, y: CGFloat) {
    let style = NSMutableParagraphStyle()
    style.alignment = .center
    let attrs: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: size, weight: weight), .foregroundColor: color, .paragraphStyle: style,
    ]
    NSAttributedString(string: s, attributes: attrs).draw(in: CGRect(x: 0, y: y, width: width, height: size * 1.6))
}
text("Drag Annex to Applications", size: 17, weight: .semibold, color: NSColor(white: 0.11, alpha: 1), y: 34)
text("Then open it from your Applications folder.", size: 12, weight: .regular,
     color: NSColor(white: 0, alpha: 0.5), y: 60)

let image = ctx.makeImage()!
let rep = NSBitmapImageRep(cgImage: image)
rep.size = NSSize(width: width, height: height)  // 72 dpi at 1x, 144 dpi at 2x
try! rep.representation(using: .png, properties: [:])!.write(to: out)
