// Renders the Windows installer's wizard images from the app icon (see render-icon.swift):
//
//   swift scripts/icon/render-installer-art.swift <icon-1024.png> <flat-icon-256.png> <out-dir>
//
// wizard.bmp / wizard-2x.bmp: the tall panel on the welcome and finish pages (164 x 314 at 100 %),
// black like the icon's tile, with the icon in the middle.
// wizard-small.bmp / wizard-small-2x.bmp: the image at the top right of the other pages (55 x 55),
// the icon on white. Opaque 24-bit-style bitmaps, which every Inno Setup version reads.
import AppKit

let args = CommandLine.arguments
guard args.count == 4 else {
    FileHandle.standardError.write("usage: render-installer-art.swift <icon-1024.png> <flat-icon-256.png> <out-dir>\n".data(using: .utf8)!)
    exit(2)
}
func load(_ path: String) -> CGImage {
    NSImage(contentsOfFile: path)!.cgImage(forProposedRect: nil, context: nil, hints: nil)!
}
let icon = load(args[1])
let flat = load(args[2])
let outDir = URL(fileURLWithPath: args[3])
let space = CGColorSpace(name: CGColorSpace.sRGB)!

func rgb(_ hex: UInt32) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
}

// Written by hand as a classic bottom-up 24-bit BMP: AppKit saves top-down BMPs (negative
// height), which Inno Setup shows upside down.
func write(_ name: String, width: Int, height: Int, draw: (CGContext, CGFloat) -> Void) {
    let ctx = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                        space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
    ctx.interpolationQuality = .high
    draw(ctx, CGFloat(width))
    let px = ctx.data!.assumingMemoryBound(to: UInt8.self)  // RGBX, first row = top of the image
    let rowBytes = (width * 3 + 3) & ~3
    var bmp = Data()
    func le<T: FixedWidthInteger>(_ v: T) { withUnsafeBytes(of: v.littleEndian) { bmp.append(contentsOf: $0) } }
    bmp.append(contentsOf: [0x42, 0x4D])  // "BM"
    le(UInt32(54 + rowBytes * height)); le(UInt32(0)); le(UInt32(54))
    le(UInt32(40)); le(Int32(width)); le(Int32(height)); le(UInt16(1)); le(UInt16(24)); le(UInt32(0))
    le(UInt32(rowBytes * height)); le(Int32(2835)); le(Int32(2835)); le(UInt32(0)); le(UInt32(0))
    for y in (0..<height).reversed() {  // bottom row first
        var row = [UInt8](repeating: 0, count: rowBytes)
        for x in 0..<width {
            let p = px + (y * width + x) * 4
            row[x * 3] = p[2]; row[x * 3 + 1] = p[1]; row[x * 3 + 2] = p[0]  // BGR
        }
        bmp.append(contentsOf: row)
    }
    try! bmp.write(to: outDir.appendingPathComponent(name))
}

// Welcome / finish panel: the icon's black gradient, with the icon a little above centre.
for (name, scale) in [("wizard.bmp", 1), ("wizard-2x.bmp", 2)] {
    let (w, h) = (164 * scale, 314 * scale)
    write(name, width: w, height: h) { ctx, _ in
        let g = CGGradient(colorsSpace: space, colors: [rgb(0x2C2C2E), rgb(0x0B0B0C)] as CFArray, locations: [0, 1])!
        // Core Graphics' origin is bottom-left: lighter at the top.
        ctx.drawLinearGradient(g, start: CGPoint(x: 0, y: h), end: CGPoint(x: 0, y: 0), options: [])
        // The 1024 icon has transparent margins (Apple's grid); draw it so the tile is ~110 pt wide.
        let side = CGFloat(140 * scale)
        ctx.draw(icon, in: CGRect(x: (CGFloat(w) - side) / 2, y: CGFloat(h) * 0.52 - side / 2, width: side, height: side))
    }
}

// Top-right page image: the flat icon on white with a small margin.
for (name, scale) in [("wizard-small.bmp", 1), ("wizard-small-2x.bmp", 2)] {
    let s = 55 * scale
    write(name, width: s, height: s) { ctx, side in
        ctx.setFillColor(rgb(0xFFFFFF))
        ctx.fill(CGRect(x: 0, y: 0, width: side, height: side))
        let inset = side * 0.08
        ctx.draw(flat, in: CGRect(x: inset, y: inset, width: side - 2 * inset, height: side - 2 * inset))
    }
}
