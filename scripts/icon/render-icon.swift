// Renders the Annex app icon with Core Graphics, so it can be regenerated and tweaked in code.
//
//   swift scripts/icon/render-icon.swift <out.png> [size] [--flat]
//
// The default is the macOS icon (1024 px canvas, squircle inset per Apple's grid). `--flat` fills
// the whole canvas with a rounded square, for Windows icons and the tray.
import AppKit
import CoreGraphics
import SwiftUI

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write("usage: render-icon.swift <out.png> [size] [--flat]\n".data(using: .utf8)!)
    exit(2)
}
let out = URL(fileURLWithPath: args[1])
let size = args.count >= 3 ? CGFloat(Double(args[2]) ?? 1024) : 1024
let flat = args.contains("--flat")
/// `--glyph`: the menu bar template image (black shapes on transparent; macOS tints it).
let glyph = args.contains("--glyph")

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

if glyph {
    // Two displays in outline, the front one with a stand; drawn on a 1024 canvas.
    let black = CGColor(gray: 0, alpha: 1)
    ctx.setStrokeColor(black)
    ctx.setFillColor(black)
    ctx.setLineWidth(72)
    ctx.setLineJoin(.round)
    let back = CGRect(x: 76, y: 150, width: 560, height: 390)
    let front = CGRect(x: 388, y: 380, width: 560, height: 390)
    // The back display shows only where the front one doesn't cover it.
    ctx.saveGState()
    ctx.addRect(CGRect(x: 0, y: 0, width: 1024, height: 1024))
    ctx.addPath(CGPath(roundedRect: front.insetBy(dx: -70, dy: -70), cornerWidth: 110, cornerHeight: 110, transform: nil))
    ctx.clip(using: .evenOdd)
    ctx.addPath(CGPath(roundedRect: back, cornerWidth: 70, cornerHeight: 70, transform: nil))
    ctx.strokePath()
    ctx.restoreGState()
    ctx.addPath(CGPath(roundedRect: front, cornerWidth: 70, cornerHeight: 70, transform: nil))
    ctx.strokePath()
    ctx.fill(CGRect(x: 632, y: 770, width: 72, height: 90))
    ctx.addPath(CGPath(roundedRect: CGRect(x: 500, y: 850, width: 336, height: 72), cornerWidth: 36, cornerHeight: 36, transform: nil))
    ctx.fillPath()
    let image = ctx.makeImage()!
    try! NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])!.write(to: out)
    exit(0)
}

// Background tile. macOS: an 824 px continuous-corner squircle centred on the 1024 canvas (Apple's
// grid), with a light drop shadow. --flat fills the canvas (Windows icons, tray).
let tile = flat ? CGRect(x: 0, y: 0, width: 1024, height: 1024) : CGRect(x: 100, y: 100, width: 824, height: 824)

/// Apple's continuous-corner rounded rectangle (the shape of macOS app icons).
func squircle(_ r: CGRect) -> CGPath {
    RoundedRectangle(cornerRadius: r.width * 0.2237, style: .continuous).path(in: r).cgPath
}
let tilePath = flat
    ? CGPath(roundedRect: tile, cornerWidth: tile.width * 0.2, cornerHeight: tile.width * 0.2, transform: nil)
    : squircle(tile)
if !flat {
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: 10), blur: 22, color: rgb(0x0B0A3A, 0.28))
    ctx.addPath(tilePath)
    ctx.setFillColor(rgb(0x2B2A86))
    ctx.fillPath()
    ctx.restoreGState()
}
ctx.saveGState()
ctx.addPath(tilePath)
ctx.clip()
// Deep indigo to blue, top-left to bottom-right, with a soft light from the top.
ctx.drawLinearGradient(gradient([rgb(0x312E81), rgb(0x4F46E5), rgb(0x2196F3)], [0, 0.5, 1]),
                       start: CGPoint(x: tile.minX, y: tile.minY), end: CGPoint(x: tile.maxX, y: tile.maxY), options: [])
ctx.drawRadialGradient(gradient([rgb(0xFFFFFF, 0.22), rgb(0xFFFFFF, 0)], [0, 1]),
                       startCenter: CGPoint(x: tile.midX, y: tile.minY), startRadius: 0,
                       endCenter: CGPoint(x: tile.midX, y: tile.minY), endRadius: tile.width * 0.8, options: [])
ctx.restoreGState()
// A fine light edge, like glass.
ctx.saveGState()
ctx.addPath(tilePath)
ctx.setStrokeColor(rgb(0xFFFFFF, 0.16))
ctx.setLineWidth(3)
ctx.strokePath()
ctx.restoreGState()

// Artwork on an 824-unit grid inside the tile, drawn a little larger than the grid around its centre.
let k = tile.width / 824
ctx.translateBy(x: tile.midX, y: tile.midY + 14 * k)
ctx.scaleBy(x: 1.06, y: 1.06)
ctx.translateBy(x: -tile.midX, y: -tile.midY)
func R(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat) -> CGRect {
    CGRect(x: tile.minX + x * k, y: tile.minY + y * k, width: w * k, height: h * k)
}
func rounded(_ r: CGRect, _ rad: CGFloat) -> CGPath {
    CGPath(roundedRect: r, cornerWidth: rad * k, cornerHeight: rad * k, transform: nil)
}

/// A display: white bezel with a glowing screen.
func display(_ r: CGRect, _ top: UInt32, _ bottom: UInt32, shadow: CGFloat) {
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: 14 * k), blur: 36 * k, color: rgb(0x0B0A3A, shadow))
    ctx.addPath(rounded(r, 30))
    ctx.setFillColor(rgb(0xFFFFFF))
    ctx.fillPath()
    ctx.restoreGState()
    let screen = r.insetBy(dx: 16 * k, dy: 16 * k)
    ctx.saveGState()
    ctx.addPath(rounded(screen, 16))
    ctx.clip()
    ctx.drawLinearGradient(gradient([rgb(top), rgb(bottom)], [0, 1]),
                           start: CGPoint(x: screen.minX, y: screen.minY), end: CGPoint(x: screen.maxX, y: screen.maxY), options: [])
    ctx.restoreGState()
}

// This computer (a laptop) and the display Annex adds next to it, with one window stretched across
// both: one desktop, two computers.
let lid = R(92, 286, 344, 222)
let monitor = R(470, 306, 262, 176)
display(monitor, 0x6366F1, 0x38BDF8, shadow: 0.30)
ctx.addPath(rounded(R(586, 482, 30, 40), 4))
ctx.setFillColor(rgb(0xE0E7FF))
ctx.fillPath()
ctx.addPath(rounded(R(546, 516, 110, 20), 10))
ctx.setFillColor(rgb(0xFFFFFF))
ctx.fillPath()
display(lid, 0x4338CA, 0x22D3EE, shadow: 0.40)
// Laptop base: a thin slab, a little wider than the lid, with a finger notch.
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: 10 * k), blur: 24 * k, color: rgb(0x0B0A3A, 0.35))
ctx.addPath(rounded(R(66, 506, 396, 26), 13))
ctx.setFillColor(rgb(0xFFFFFF))
ctx.fillPath()
ctx.restoreGState()
ctx.addPath(rounded(R(228, 506, 72, 9), 4.5))
ctx.setFillColor(rgb(0xC7D2FE))
ctx.fillPath()

let win = R(324, 330, 236, 122)
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: 12 * k), blur: 28 * k, color: rgb(0x0B0A3A, 0.45))
ctx.addPath(rounded(win, 16))
ctx.setFillColor(rgb(0xFFFFFF))
ctx.fillPath()
ctx.restoreGState()
ctx.saveGState()
ctx.addPath(rounded(win, 16))
ctx.clip()
ctx.drawLinearGradient(gradient([rgb(0x818CF8), rgb(0x60A5FA)], [0, 1]),
                       start: CGPoint(x: win.minX, y: win.minY), end: CGPoint(x: win.maxX, y: win.minY), options: [])
ctx.setFillColor(rgb(0xFFFFFF))
ctx.fill(CGRect(x: win.minX, y: win.minY + 32 * k, width: win.width, height: win.height))
// Two soft content bars, big enough to read as a window at small sizes.
for (i, w) in [0.62, 0.42].enumerated() {
    ctx.addPath(rounded(CGRect(x: win.minX + 22 * k, y: win.minY + (52 + CGFloat(i) * 26) * k,
                               width: win.width * CGFloat(w), height: 13 * k), 6.5))
    ctx.setFillColor(rgb(i == 0 ? 0xA5B4FC : 0xDBEAFE))
    ctx.fillPath()
}
ctx.restoreGState()

let image = ctx.makeImage()!
let rep = NSBitmapImageRep(cgImage: image)
try! rep.representation(using: .png, properties: [:])!.write(to: out)
