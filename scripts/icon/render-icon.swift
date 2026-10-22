// Renders the Annex app icon with Core Graphics, so it can be regenerated and tweaked in code.
//
//   swift scripts/icon/render-icon.swift <out.png> [size] [--flat]
//
// The default is the macOS icon (1024 px canvas, squircle inset per Apple's grid). `--flat` fills
// the whole canvas with a rounded square, for Windows icons and the tray.
import AppKit
import CoreGraphics

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write("usage: render-icon.swift <out.png> [size] [--flat]\n".data(using: .utf8)!)
    exit(2)
}
let out = URL(fileURLWithPath: args[1])
let size = args.count >= 3 ? CGFloat(Double(args[2]) ?? 1024) : 1024
let flat = args.contains("--flat")

func rgb(_ hex: UInt32, _ a: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255, alpha: a)
}

let space = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CGContext(data: nil, width: Int(size), height: Int(size), bitsPerComponent: 8, bytesPerRow: 0,
                    space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
// Work in a 1024-unit canvas with a top-left origin.
ctx.translateBy(x: 0, y: size)
ctx.scaleBy(x: size / 1024, y: -size / 1024)

func gradient(_ colors: [CGColor], _ locations: [CGFloat]) -> CGGradient {
    CGGradient(colorsSpace: space, colors: colors as CFArray, locations: locations)!
}

// Background tile. macOS: 824 px squircle centred on the 1024 canvas, with a soft drop shadow.
let tile = flat ? CGRect(x: 0, y: 0, width: 1024, height: 1024) : CGRect(x: 100, y: 100, width: 824, height: 824)
let radius = tile.width * 0.2237
let tilePath = CGPath(roundedRect: tile, cornerWidth: radius, cornerHeight: radius, transform: nil)
if !flat {
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: 12), blur: 28, color: rgb(0x000000, 0.35))
    ctx.addPath(tilePath)
    ctx.setFillColor(rgb(0x2A2F8F))
    ctx.fillPath()
    ctx.restoreGState()
}
ctx.saveGState()
ctx.addPath(tilePath)
ctx.clip()
// Deep indigo → violet → cyan, top-left to bottom-right.
ctx.drawLinearGradient(gradient([rgb(0x312E81), rgb(0x5B4BDB), rgb(0x1FB6E0)], [0, 0.55, 1]),
                       start: CGPoint(x: tile.minX, y: tile.minY), end: CGPoint(x: tile.maxX, y: tile.maxY), options: [])
// A soft light from the top for depth.
ctx.drawRadialGradient(gradient([rgb(0xFFFFFF, 0.28), rgb(0xFFFFFF, 0)], [0, 1]),
                       startCenter: CGPoint(x: tile.midX, y: tile.minY + tile.height * 0.05), startRadius: 0,
                       endCenter: CGPoint(x: tile.midX, y: tile.minY + tile.height * 0.05), endRadius: tile.width * 0.75,
                       options: [])
ctx.restoreGState()

// Scale the artwork to the tile.
let k = tile.width / 824
func R(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat) -> CGRect {
    CGRect(x: tile.minX + x * k, y: tile.minY + y * k, width: w * k, height: h * k)
}
func rounded(_ r: CGRect, _ rad: CGFloat) -> CGPath {
    CGPath(roundedRect: r, cornerWidth: rad * k, cornerHeight: rad * k, transform: nil)
}

// Back display: the "other" screen, frosted, up and to the left.
let back = R(150, 166, 404, 270)
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: 10 * k), blur: 30 * k, color: rgb(0x0B0A3A, 0.30))
ctx.addPath(rounded(back, 34))
ctx.setFillColor(rgb(0xFFFFFF, 0.34))
ctx.fillPath()
ctx.restoreGState()
let backScreen = back.insetBy(dx: 18 * k, dy: 18 * k)
ctx.addPath(rounded(backScreen, 18))
ctx.setFillColor(rgb(0xFFFFFF, 0.18))
ctx.fillPath()

// Front display, solid, overlapping down and to the right.
let front = R(270, 318, 404, 270)
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: 18 * k), blur: 44 * k, color: rgb(0x0B0A3A, 0.50))
ctx.addPath(rounded(front, 34))
ctx.setFillColor(rgb(0xFFFFFF))
ctx.fillPath()
ctx.restoreGState()
let screen = front.insetBy(dx: 18 * k, dy: 18 * k)
ctx.saveGState()
ctx.addPath(rounded(screen, 18))
ctx.clip()
ctx.drawLinearGradient(gradient([rgb(0x4338CA), rgb(0x22D3EE)], [0, 1]),
                       start: CGPoint(x: screen.minX, y: screen.minY), end: CGPoint(x: screen.maxX, y: screen.maxY), options: [])
ctx.restoreGState()

// Stand.
ctx.setFillColor(rgb(0xE0E7FF))
ctx.fill(R(442, 588, 60, 52))
ctx.addPath(rounded(R(372, 632, 200, 30), 15))
ctx.setFillColor(rgb(0xFFFFFF))
ctx.fillPath()

// A window being dragged from the back screen onto the front one, floating over both.
let win = R(196, 352, 252, 150)
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: 14 * k), blur: 30 * k, color: rgb(0x0B0A3A, 0.45))
ctx.addPath(rounded(win, 16))
ctx.setFillColor(rgb(0xFFFFFF))
ctx.fillPath()
ctx.restoreGState()
ctx.saveGState()
ctx.addPath(rounded(win, 16))
ctx.clip()
ctx.setFillColor(rgb(0xE0E7FF))
ctx.fill(CGRect(x: win.minX, y: win.minY, width: win.width, height: 34 * k))
// Content lines.
for (i, w) in [0.62, 0.44, 0.54].enumerated() {
    ctx.addPath(rounded(CGRect(x: win.minX + 22 * k, y: win.minY + (56 + CGFloat(i) * 26) * k,
                               width: win.width * CGFloat(w), height: 12 * k), 6))
    ctx.setFillColor(rgb(i == 0 ? 0x818CF8 : 0xC7D2FE))
    ctx.fillPath()
}
ctx.restoreGState()
for (i, c) in [0xF87171, 0xFBBF24, 0x34D399].enumerated() {
    ctx.addEllipse(in: CGRect(x: win.minX + (16 + CGFloat(i) * 22) * k, y: win.minY + 11 * k, width: 13 * k, height: 13 * k))
    ctx.setFillColor(rgb(UInt32(c)))
    ctx.fillPath()
}

let image = ctx.makeImage()!
let rep = NSBitmapImageRep(cgImage: image)
try! rep.representation(using: .png, properties: [:])!.write(to: out)
