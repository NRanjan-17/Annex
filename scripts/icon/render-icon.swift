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
    ctx.setShadow(offset: CGSize(width: 0, height: 10), blur: 22, color: rgb(0x000000, 0.30))
    ctx.addPath(tilePath)
    ctx.setFillColor(rgb(0x111113))
    ctx.fillPath()
    ctx.restoreGState()
}
ctx.saveGState()
ctx.addPath(tilePath)
ctx.clip()
// Black, a touch lighter at the top, with a soft sheen: quiet and monochrome.
ctx.drawLinearGradient(gradient([rgb(0x2C2C2E), rgb(0x0B0B0C)], [0, 1]),
                       start: CGPoint(x: tile.midX, y: tile.minY), end: CGPoint(x: tile.midX, y: tile.maxY), options: [])
ctx.drawRadialGradient(gradient([rgb(0xFFFFFF, 0.10), rgb(0xFFFFFF, 0)], [0, 1]),
                       startCenter: CGPoint(x: tile.midX, y: tile.minY), startRadius: 0,
                       endCenter: CGPoint(x: tile.midX, y: tile.minY), endRadius: tile.width * 0.75, options: [])
ctx.restoreGState()
// A fine light edge, like glass.
ctx.saveGState()
ctx.addPath(tilePath)
ctx.setStrokeColor(rgb(0xFFFFFF, 0.10))
ctx.setLineWidth(3)
ctx.strokePath()
ctx.restoreGState()

// Artwork on an 824-unit grid inside the tile.
let k = tile.width / 824
func R(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat) -> CGRect {
    CGRect(x: tile.minX + x * k, y: tile.minY + y * k, width: w * k, height: h * k)
}
func rounded(_ r: CGRect, _ rad: CGFloat) -> CGPath {
    CGPath(roundedRect: r, cornerWidth: rad * k, cornerHeight: rad * k, transform: nil)
}

// Two displays, one in front of the other: this computer and the screen Annex adds. The same
// shapes as the menu bar glyph, in white on black.
let white = rgb(0xFFFFFF)
let back = R(170, 230, 380, 260)
let front = R(290, 330, 380, 260)
// The back display is an outline that stops short of the front one.
ctx.saveGState()
ctx.addRect(CGRect(x: 0, y: 0, width: 1024, height: 1024))
ctx.addPath(rounded(front.insetBy(dx: -26 * k, dy: -26 * k), 58))
ctx.clip(using: .evenOdd)
ctx.setStrokeColor(white)
ctx.setLineWidth(30 * k)
ctx.addPath(rounded(back, 40))
ctx.strokePath()
ctx.restoreGState()
// The front display: a solid white bezel around a dark screen, on a stand.
ctx.addPath(rounded(front, 40))
ctx.setFillColor(white)
ctx.fillPath()
ctx.addPath(rounded(front.insetBy(dx: 26 * k, dy: 26 * k), 20))
ctx.setFillColor(rgb(0x1C1C1E))
ctx.fillPath()
ctx.setFillColor(white)
ctx.fill(R(458, 590, 44, 50))
ctx.addPath(rounded(R(392, 630, 176, 32), 16))
ctx.fillPath()

let image = ctx.makeImage()!
let rep = NSBitmapImageRep(cgImage: image)
try! rep.representation(using: .png, properties: [:])!.write(to: out)
