#!/bin/bash
# Builds a universal (arm64 + x86_64) Release Annex.app and packages it as a .dmg.
#
#   scripts/package-macos.sh [version]
#
# Signing and notarization are optional and driven by environment variables:
#   DEVELOPER_ID_APPLICATION  e.g. "Developer ID Application: Jane Doe (TEAMID1234)"
#   DEVELOPMENT_TEAM          Apple team ID (TEAMID1234)
#   NOTARY_KEY_PATH, NOTARY_KEY_ID, NOTARY_ISSUER   App Store Connect API key for notarytool
# Without them the app is ad-hoc signed: fine on the machine that built it, but Gatekeeper
# blocks it on other Macs (see docs/CICD.md).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="${1:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/core/Cargo.toml" | head -1)}"
BUILD="$ROOT/apps/macos/build/release"
DIST="$ROOT/dist"
STAGE="$BUILD/dmg-stage"
DMG="$DIST/Annex-$VERSION-macos-universal.dmg"

SIGN_ARGS=(CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual)
if [ -n "${DEVELOPER_ID_APPLICATION:-}" ]; then
  SIGN_ARGS=(CODE_SIGN_IDENTITY="$DEVELOPER_ID_APPLICATION" CODE_SIGN_STYLE=Manual
             DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:-}" OTHER_CODE_SIGN_FLAGS="--timestamp --options=runtime")
fi

echo "==> Building Annex $VERSION (universal, Release)"
xcodebuild build \
  -project "$ROOT/apps/macos/Annex.xcodeproj" -scheme Annex -configuration Release \
  -destination 'generic/platform=macOS' -derivedDataPath "$BUILD/DerivedData" \
  ARCHS="arm64 x86_64" ONLY_ACTIVE_ARCH=NO \
  MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="${BUILD_NUMBER:-1}" \
  "${SIGN_ARGS[@]}" | grep -E "error:|warning: |BUILD (SUCCEEDED|FAILED)" || true

APP="$BUILD/DerivedData/Build/Products/Release/Annex.app"
[ -d "$APP" ] || { echo "error: build failed, $APP missing"; exit 1; }
lipo -info "$APP/Contents/MacOS/Annex"
codesign --verify --strict --verbose=1 "$APP"

echo "==> Creating $DMG"
rm -rf "$STAGE"
mkdir -p "$STAGE" "$DIST"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
cp "$ROOT/installer/macos/Uninstall Annex.command" "$STAGE/"
rm -f "$DMG"
hdiutil create -volname "Annex $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null

if [ -n "${DEVELOPER_ID_APPLICATION:-}" ]; then
  codesign --sign "$DEVELOPER_ID_APPLICATION" --timestamp "$DMG"
  if [ -n "${NOTARY_KEY_PATH:-}" ]; then
    echo "==> Notarizing"
    xcrun notarytool submit "$DMG" --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" --wait
    xcrun stapler staple "$DMG"
    xcrun stapler validate "$DMG"
  fi
fi

shasum -a 256 "$DMG" | tee "$DMG.sha256"
echo "==> Done: $DMG"
