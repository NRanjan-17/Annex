#!/usr/bin/env python3
"""Draws Annex's downloads per version, by system, as hand-drawn line charts for the README.

    scripts/download-chart.py OUT_DIR

Writes OUT_DIR/downloads-light.svg and OUT_DIR/downloads-dark.svg (for GitHub's light and dark
themes) from the GitHub releases API. Only installers count: the .dmg, the Windows setup .exe
files and the Linux packages, not checksums or SBOMs. Pre-releases count towards the version they
lead to (0.2.0-pre.3 → 0.2.0). Uses GITHUB_TOKEN if set (higher rate limit).

The pencil look: every line is drawn twice with a small wobble, an SVG noise filter roughens the
strokes, and the text uses Patrick Hand (SIL Open Font License), embedded so it shows the same
everywhere; without network access it falls back to the viewer's handwriting fonts. The wobble is
seeded from the data, so the picture only changes when the numbers do.
"""

import base64
import json
import os
import random
import re
import sys
import urllib.request
from html import escape

REPO = "NRanjan-17/Annex"
SYSTEMS = ["macOS", "Windows", "Linux"]
# Categorical slots 1–3, checked for colour-blind separation and contrast on GitHub's backgrounds.
THEMES = {
    "light": {"surface": "#ffffff", "text": "#1f2328", "muted": "#59636e", "grid": "#d1d9e0",
              "series": ["#2a78d6", "#eb6834", "#1baf7a"]},
    "dark": {"surface": "#0d1117", "text": "#f0f6fc", "muted": "#9198a1", "grid": "#3d444d",
             "series": ["#3987e5", "#d95926", "#199e70"]},
}
LAST = 10  # versions shown
FONT_URL = "https://fonts.gstatic.com/s/patrickhand/v25/LDI1apSQOAYtSuYWp8ZhfYe8XsLLubg58w.woff2"
FONTS = "'Patrick Hand','Comic Neue','Comic Sans MS','Chalkboard SE','Bradley Hand',cursive"


def get(url, token=None):
    req = urllib.request.Request(url, headers={"Accept": "application/vnd.github+json", "User-Agent": "annex-chart"})
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read()


def releases():
    out, page = [], 1
    while True:
        batch = json.loads(get(f"https://api.github.com/repos/{REPO}/releases?per_page=100&page={page}",
                               os.environ.get("GITHUB_TOKEN")))
        out += batch
        if len(batch) < 100:
            return out
        page += 1


def system_of(name):
    if name.endswith(".dmg"):
        return "macOS"
    if name.startswith("Annex-Setup-") and name.endswith(".exe"):
        return "Windows"
    if re.search(r"-linux-[^.]+\.(deb|rpm|tar\.gz)$", name):
        return "Linux"
    return None


def tally(rels):
    """[(version, {system: downloads})], oldest version first."""
    counts = {}
    for r in rels:
        if r.get("draft"):
            continue
        m = re.match(r"v?(\d+)\.(\d+)\.(\d+)", r["tag_name"])
        if not m:
            continue
        key = tuple(int(x) for x in m.groups())
        per = counts.setdefault(key, dict.fromkeys(SYSTEMS, 0))
        for a in r.get("assets", []):
            s = system_of(a["name"])
            if s:
                per[s] += a["download_count"]
    return [(".".join(map(str, k)), counts[k]) for k in sorted(counts)]


def nice_step(peak):
    for s in [1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 2500, 5000, 10000]:
        if peak / s <= 5:
            return s
    return 10 ** len(str(peak))


class Pencil:
    """Strokes that look drawn by hand: each one twice, slightly off, bowing a little."""

    def __init__(self, seed):
        self.rnd = random.Random(seed)

    def j(self, amount):
        return self.rnd.uniform(-amount, amount)

    def line(self, x1, y1, x2, y2, color, width=2.0, wobble=1.2, passes=2, opacity=1.0, dash=""):
        out = []
        for p in range(passes):
            w = width if p == 0 else width * 0.55
            a = (x1 + self.j(wobble), y1 + self.j(wobble))
            b = (x2 + self.j(wobble), y2 + self.j(wobble))
            # Bow the middle a little, across the stroke.
            mx, my = (a[0] + b[0]) / 2 + self.j(wobble * 1.5), (a[1] + b[1]) / 2 + self.j(wobble * 1.5)
            out.append(f'<path d="M{a[0]:.1f},{a[1]:.1f} Q{mx:.1f},{my:.1f} {b[0]:.1f},{b[1]:.1f}" fill="none" '
                       f'stroke="{color}" stroke-width="{w:.2f}" stroke-linecap="round" opacity="{opacity}"{dash}/>')
        return out

    def polyline(self, pts, color, width=2.2):
        out = []
        for p in range(2):
            w = width if p == 0 else width * 0.5
            d = []
            for i, (x, y) in enumerate(pts):
                x, y = x + self.j(1.0), y + self.j(1.0)
                if i == 0:
                    d.append(f"M{x:.1f},{y:.1f}")
                else:
                    px, py = pts[i - 1]
                    mx, my = (px + x) / 2 + self.j(1.8), (py + y) / 2 + self.j(1.8)
                    d.append(f"Q{mx:.1f},{my:.1f} {x:.1f},{y:.1f}")
            out.append(f'<path d="{" ".join(d)}" fill="none" stroke="{color}" stroke-width="{w:.2f}" '
                       f'stroke-linecap="round" stroke-linejoin="round"/>')
        return out

    def dot(self, x, y, color, surface, tip):
        # A filled dot ringed in the surface colour, then a loose circle drawn around it.
        r = 4.2
        out = [f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r + 2}" fill="{surface}"/>',
               f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r}" fill="{color}"><title>{tip}</title></circle>']
        rx, ry = r + 2.2 + self.j(0.6), r + 1.8 + self.j(0.6)
        start = self.rnd.uniform(0, 6.28)
        out.append(f'<ellipse cx="{x + self.j(0.6):.1f}" cy="{y + self.j(0.6):.1f}" rx="{rx:.1f}" ry="{ry:.1f}" '
                   f'transform="rotate({start * 57:.0f} {x:.1f} {y:.1f})" fill="none" stroke="{color}" '
                   f'stroke-width="1" opacity="0.7"/>')
        return out


def font_face():
    try:
        b = get(FONT_URL)
    except Exception as e:  # noqa: BLE001 - the chart still draws with fallback fonts
        print(f"no embedded font ({e}); using the viewer's handwriting fonts", file=sys.stderr)
        return ""
    return ("<style>@font-face{font-family:'Patrick Hand';src:url(data:font/woff2;base64,"
            + base64.b64encode(b).decode() + ") format('woff2');}</style>")


def svg(data, theme, face):
    t = THEMES[theme]
    data = data[-LAST:]
    pen = Pencil(json.dumps(data))
    total = sum(sum(v.values()) for _, v in data)
    shown = [s for s in SYSTEMS if any(per[s] for _, per in data)] or SYSTEMS[:1]
    # Under the versions, a row of numbers per system: GitHub shows the README's SVG as an image,
    # so the dots' tooltips never appear.
    row_h = 22
    w, h = 720, 320 + row_h * len(shown)
    left, right, top, bottom = 52, 92, 78, 44 + row_h * len(shown)
    plot_w, plot_h = w - left - right, h - top - bottom
    peak = max([per[s] for _, per in data for s in shown] + [1])
    step = nice_step(peak)
    ymax = step * -(-peak // step)
    n = max(len(data), 2)

    def px(i):
        return left + plot_w * (i / (n - 1) if len(data) > 1 else 0.5)

    def py(v):
        return top + plot_h - plot_h * v / ymax

    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" '
             f'font-family="{FONTS}" role="img">',
             f"<title>Annex downloads per version, by system: {total:,} in total</title>",
             face,
             '<defs><filter id="pencil" x="-5%" y="-5%" width="110%" height="110%">'
             '<feTurbulence type="fractalNoise" baseFrequency="0.035" numOctaves="2" seed="7" result="n"/>'
             '<feDisplacementMap in="SourceGraphic" in2="n" scale="2.2" xChannelSelector="R" yChannelSelector="G"/>'
             "</filter></defs>",
             f'<rect width="{w}" height="{h}" rx="6" fill="{t["surface"]}"/>',
             f'<text x="{left}" y="34" font-size="24" fill="{t["text"]}">Downloads per version</text>',
             f'<text x="{w - 16}" y="34" font-size="18" text-anchor="end" fill="{t["muted"]}">{total:,} in total</text>']
    # Legend in series order (the rows of numbers are labelled too).
    x = left
    for s in shown:
        i = SYSTEMS.index(s)
        parts += pen.line(x, 57, x + 20, 57, t["series"][i], width=2.6, wobble=0.6)
        parts.append(f'<text x="{x + 26}" y="62" font-size="17" fill="{t["muted"]}">{s}</text>')
        x += 26 + 9 * len(s) + 24

    g = ['<g filter="url(#pencil)">']
    # Faint grid and the axes, sketched.
    for k in range(0, int(ymax // step) + 1):
        v = k * step
        y = py(v)
        if k:
            g += pen.line(left, y, left + plot_w, y, t["grid"], width=1.1, wobble=1.0, passes=1)
        parts.append(f'<text x="{left - 10}" y="{y + 5:.1f}" font-size="15" text-anchor="end" fill="{t["muted"]}">{v:,}</text>')
    g += pen.line(left, top + plot_h, left + plot_w + 8, top + plot_h, t["muted"], width=1.6, wobble=0.8)
    g += pen.line(left, top - 6, left, top + plot_h, t["muted"], width=1.6, wobble=0.8)
    axis_y = top + plot_h + 28
    for i, (version, _) in enumerate(data):
        parts.append(f'<text x="{px(i):.1f}" y="{axis_y}" font-size="15" text-anchor="middle" fill="{t["muted"]}">{escape(version)}</text>')
    for r, s in enumerate(shown):
        y = axis_y + row_h * (r + 1)
        parts += pen.line(left - 46, y - 5, left - 28, y - 5, t["series"][SYSTEMS.index(s)], width=2.6, wobble=0.6)
        for i, (_, per) in enumerate(data):
            parts.append(f'<text x="{px(i):.1f}" y="{y}" font-size="15" text-anchor="middle" fill="{t["text"]}">{per[s]:,}</text>')
        parts.append(f'<text x="{px(len(data) - 1) + 24:.1f}" y="{y}" font-size="15" fill="{t["muted"]}">{s}</text>')

    # One line per system, with a dot on every version.
    for s in shown:
        i = SYSTEMS.index(s)
        pts = [(px(j), py(per[s])) for j, (_, per) in enumerate(data)]
        g += pen.polyline(pts, t["series"][i])
    g.append("</g>")
    parts += g
    for s in shown:
        i = SYSTEMS.index(s)
        for j, (version, per) in enumerate(data):
            parts += pen.dot(px(j), py(per[s]), t["series"][i], t["surface"], escape(f"{version} {s}: {per[s]:,}"))
    parts.append("</svg>")
    return "\n".join(parts) + "\n"


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "."
    os.makedirs(out, exist_ok=True)
    data = tally(releases())
    face = font_face()
    for theme in THEMES:
        with open(os.path.join(out, f"downloads-{theme}.svg"), "w") as f:
            f.write(svg(data, theme, face))
    for version, per in data[-LAST:]:
        print(version, per)


if __name__ == "__main__":
    main()
