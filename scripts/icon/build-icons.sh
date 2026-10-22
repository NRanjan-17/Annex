#!/usr/bin/env bash
# Regenerates every app icon from scripts/icon/render-icon.swift (macOS only; outputs are committed):
#   assets/icon-<size>.png                         README / docs
#   apps/macos/Annex/Assets.xcassets/AppIcon...    macOS app icon
#   apps/windows/src/Annex.App/Assets/Annex.ico    Windows app, tray and installer icon
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RENDER="$ROOT/scripts/icon/render-icon.swift"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Compile once; the script is rendered many times.
swiftc -O -o "$TMP/render" "$RENDER"

echo "==> PNGs for docs"
mkdir -p "$ROOT/assets"
for s in 16 24 32 48 64 128 256 512 1024; do
  "$TMP/render" "$ROOT/assets/icon-$s.png" "$s"
done

echo "==> macOS asset catalog"
CAT="$ROOT/apps/macos/Annex/Assets.xcassets"
SET="$CAT/AppIcon.appiconset"
mkdir -p "$SET"
cat > "$CAT/Contents.json" <<'JSON'
{
  "info" : { "author" : "xcode", "version" : 1 }
}
JSON
images=""
for s in 16 32 128 256 512; do
  "$TMP/render" "$SET/icon_${s}.png" "$s"
  "$TMP/render" "$SET/icon_${s}@2x.png" "$((s * 2))"
  images+="    { \"filename\" : \"icon_${s}.png\", \"idiom\" : \"mac\", \"scale\" : \"1x\", \"size\" : \"${s}x${s}\" },"$'\n'
  images+="    { \"filename\" : \"icon_${s}@2x.png\", \"idiom\" : \"mac\", \"scale\" : \"2x\", \"size\" : \"${s}x${s}\" },"$'\n'
done
cat > "$SET/Contents.json" <<JSON
{
  "images" : [
${images%,$'\n'}
  ],
  "info" : { "author" : "xcode", "version" : 1 }
}
JSON

echo "==> Menu bar glyph (template image)"
GLYPH="$CAT/MenuBarIcon.imageset"
mkdir -p "$GLYPH"
"$TMP/render" "$GLYPH/menubar.png" 18 --glyph
"$TMP/render" "$GLYPH/menubar@2x.png" 36 --glyph
cat > "$GLYPH/Contents.json" <<'JSON'
{
  "images" : [
    { "filename" : "menubar.png", "idiom" : "mac", "scale" : "1x" },
    { "filename" : "menubar@2x.png", "idiom" : "mac", "scale" : "2x" }
  ],
  "info" : { "author" : "xcode", "version" : 1 },
  "properties" : { "template-rendering-intent" : "template" }
}
JSON

echo "==> Windows .ico"
for s in 16 20 24 32 40 48 64 128 256; do
  "$TMP/render" "$TMP/win-$s.png" "$s" --flat
done
# An .ico is a directory of embedded PNGs (supported since Windows Vista).
python3 - "$TMP" "$ROOT/apps/windows/src/Annex.App/Assets/Annex.ico" <<'PY'
import struct, sys
tmp, out = sys.argv[1], sys.argv[2]
sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
blobs = [open(f"{tmp}/win-{s}.png", "rb").read() for s in sizes]
header = struct.pack("<HHH", 0, 1, len(sizes))
offset = 6 + 16 * len(sizes)
entries = b""
for s, b in zip(sizes, blobs):
    entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(b), offset)
    offset += len(b)
open(out, "wb").write(header + entries + b"".join(blobs))
PY
echo "Done."
