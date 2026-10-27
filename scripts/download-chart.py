#!/usr/bin/env python3
"""Draws Annex's downloads per version, by system, as SVG charts for the README.

    scripts/download-chart.py OUT_DIR

Writes OUT_DIR/downloads-light.svg and OUT_DIR/downloads-dark.svg (for GitHub's light and dark
themes) from the GitHub releases API. Only installers count: the .dmg, the Windows setup .exe
files and the Linux packages, not checksums or SBOMs. Pre-releases count towards the version they
lead to (0.2.0-pre.3 → 0.2.0). Uses GITHUB_TOKEN if set (higher rate limit).
"""

import json
import os
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
LAST = 8  # versions shown


def system_of(name):
    if name.endswith(".dmg"):
        return "macOS"
    if name.startswith("Annex-Setup-") and name.endswith(".exe"):
        return "Windows"
    if re.search(r"-linux-[^.]+\.(deb|rpm|tar\.gz)$", name):
        return "Linux"
    return None


def releases():
    out, page = [], 1
    token = os.environ.get("GITHUB_TOKEN")
    while True:
        req = urllib.request.Request(f"https://api.github.com/repos/{REPO}/releases?per_page=100&page={page}",
                                     headers={"Accept": "application/vnd.github+json", "User-Agent": "annex-chart"})
        if token:
            req.add_header("Authorization", f"Bearer {token}")
        with urllib.request.urlopen(req, timeout=30) as r:
            batch = json.load(r)
        out += batch
        if len(batch) < 100:
            return out
        page += 1


def tally(rels):
    """{version: {system: downloads}}, oldest version first."""
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


def svg(data, theme):
    t = THEMES[theme]
    data = data[-LAST:]
    total = sum(sum(v.values()) for _, v in data)
    w, h = 720, 300
    left, right, top, bottom = 48, 16, 72, 36
    plot_w, plot_h = w - left - right, h - top - bottom
    peak = max([sum(v.values()) for _, v in data] + [1])
    step = nice_step(peak)
    ymax = step * -(-peak // step)
    slot = plot_w / max(len(data), 1)
    bar = min(48, slot * 0.6)
    font = "font-family=\"-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif\""
    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" {font} role="img">',
             f"<title>Annex downloads per version, by system: {total} in total</title>",
             f'<rect width="{w}" height="{h}" rx="6" fill="{t["surface"]}"/>',
             f'<text x="{left}" y="28" font-size="15" font-weight="600" fill="{t["text"]}">Downloads per version</text>',
             f'<text x="{w - right}" y="28" font-size="13" text-anchor="end" fill="{t["muted"]}">{total:,} in total</text>']
    # Legend: swatch + name, in series order.
    x = left
    for i, s in enumerate(SYSTEMS):
        parts.append(f'<rect x="{x}" y="42" width="10" height="10" rx="2" fill="{t["series"][i]}"/>')
        parts.append(f'<text x="{x + 15}" y="51" font-size="12" fill="{t["muted"]}">{s}</text>')
        x += 15 + 6.6 * len(s) + 20
    # Recessive grid and axis labels.
    for k in range(0, int(ymax // step) + 1):
        v = k * step
        y = top + plot_h - plot_h * v / ymax
        parts.append(f'<line x1="{left}" x2="{w - right}" y1="{y:.1f}" y2="{y:.1f}" stroke="{t["grid"]}" stroke-width="1"/>')
        parts.append(f'<text x="{left - 8}" y="{y + 4:.1f}" font-size="11" text-anchor="end" fill="{t["muted"]}">{v:,}</text>')
    for j, (version, per) in enumerate(data):
        cx = left + slot * (j + 0.5)
        x0 = cx - bar / 2
        y = top + plot_h
        stack = [(i, per[s]) for i, s in enumerate(SYSTEMS) if per[s] > 0]
        for n, (i, v) in enumerate(stack):
            hgt = plot_h * v / ymax
            gap = 2 if n < len(stack) - 1 else 0  # surface gap between segments
            seg_h = max(hgt - gap, 1)
            y -= hgt
            tip = escape(f"{version} {SYSTEMS[i]}: {v:,}")
            if n == len(stack) - 1:
                # Rounded data end on the top segment only.
                r = min(4, seg_h)
                parts.append(f'<path d="M{x0:.1f},{y + seg_h + gap:.1f} V{y + r:.1f} Q{x0:.1f},{y:.1f} {x0 + r:.1f},{y:.1f} '
                             f'H{x0 + bar - r:.1f} Q{x0 + bar:.1f},{y:.1f} {x0 + bar:.1f},{y + r:.1f} V{y + seg_h + gap:.1f} Z" '
                             f'fill="{t["series"][i]}"><title>{tip}</title></path>')
            else:
                parts.append(f'<rect x="{x0:.1f}" y="{y + gap:.1f}" width="{bar:.1f}" height="{seg_h:.1f}" '
                             f'fill="{t["series"][i]}"><title>{tip}</title></rect>')
        total_v = sum(per.values())
        parts.append(f'<text x="{cx:.1f}" y="{y - 6:.1f}" font-size="11" text-anchor="middle" fill="{t["text"]}">{total_v:,}</text>')
        parts.append(f'<text x="{cx:.1f}" y="{h - 14}" font-size="11" text-anchor="middle" fill="{t["muted"]}">{escape(version)}</text>')
    parts.append("</svg>")
    return "\n".join(parts) + "\n"


def nice_step(peak):
    for s in [1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 2500, 5000, 10000]:
        if peak / s <= 5:
            return s
    return 10 ** len(str(peak))


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "."
    os.makedirs(out, exist_ok=True)
    data = tally(releases())
    for theme in THEMES:
        with open(os.path.join(out, f"downloads-{theme}.svg"), "w") as f:
            f.write(svg(data, theme))
    for version, per in data[-LAST:]:
        print(version, per)


if __name__ == "__main__":
    main()
