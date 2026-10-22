#!/usr/bin/env python3
"""Renders the Annex icon (two overlapping displays) without third-party libraries.

Writes assets/icon-<size>.png and apps/windows/src/Annex.App/Assets/Annex.ico.
Run: python3 scripts/make-icons.py
"""
import os, struct, zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BG = (37, 99, 235)        # blue tile
FG = (255, 255, 255)      # white display outlines
DIM = (191, 219, 254)     # back display


def render(n):
    px = [[(0, 0, 0, 0)] * n for _ in range(n)]
    r = n * 0.22  # tile corner radius

    def inside_round(x, y, x0, y0, x1, y1, rad):
        cx = min(max(x, x0 + rad), x1 - rad)
        cy = min(max(y, y0 + rad), y1 - rad)
        return (x - cx) ** 2 + (y - cy) ** 2 <= rad * rad

    def rect_outline(x, y, x0, y0, x1, y1, t):
        inner = x0 + t <= x <= x1 - t and y0 + t <= y <= y1 - t
        return x0 <= x <= x1 and y0 <= y <= y1 and not inner

    t = max(1.0, n * 0.06)
    for j in range(n):
        for i in range(n):
            # 4x supersampling for smooth edges
            acc = [0, 0, 0, 0]
            for sy in range(4):
                for sx in range(4):
                    x, y = (i + (sx + 0.5) / 4) / n, (j + (sy + 0.5) / 4) / n
                    c = None
                    if inside_round(x * n, y * n, 0, 0, n, n, r):
                        c = BG
                        if rect_outline(x, y, 0.30, 0.22, 0.86, 0.60, t / n):
                            c = DIM
                        if rect_outline(x, y, 0.14, 0.36, 0.70, 0.74, t / n):
                            c = FG
                        if 0.36 <= x <= 0.48 and 0.74 <= y <= 0.82:
                            c = FG
                        if 0.28 <= x <= 0.56 and 0.80 <= y <= 0.80 + t / n:
                            c = FG
                    if c:
                        acc[0] += c[0]; acc[1] += c[1]; acc[2] += c[2]; acc[3] += 255
            a = acc[3] // 16
            px[j][i] = (acc[0] // 16 * 16 // max(1, acc[3] // 255) if a else 0,
                        acc[1] // 16 * 16 // max(1, acc[3] // 255) if a else 0,
                        acc[2] // 16 * 16 // max(1, acc[3] // 255) if a else 0, a)
    return px


def png(px):
    n = len(px)
    raw = b"".join(b"\x00" + bytes(v for p in row for v in p) for row in px)
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    sizes = [16, 24, 32, 48, 64, 128, 256, 512, 1024]
    images = {s: png(render(s)) for s in sizes}
    for s, data in images.items():
        with open(os.path.join(ROOT, "assets", f"icon-{s}.png"), "wb") as f:
            f.write(data)
    ico_sizes = [16, 24, 32, 48, 64, 256]
    header = struct.pack("<HHH", 0, 1, len(ico_sizes))
    offset = 6 + 16 * len(ico_sizes)
    entries, blobs = b"", b""
    for s in ico_sizes:
        d = images[s]
        entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(d), offset)
        blobs += d
        offset += len(d)
    with open(os.path.join(ROOT, "apps/windows/src/Annex.App/Assets/Annex.ico"), "wb") as f:
        f.write(header + entries + blobs)


if __name__ == "__main__":
    main()
