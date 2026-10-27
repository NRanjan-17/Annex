#!/bin/bash
# Builds the Linux app and packages it as .deb, .rpm and .tar.gz for this machine's CPU.
#   scripts/package-linux.sh 1.2.3
# Output: dist/Annex-<version>-linux-<arch>.{deb,rpm,tar.gz} (arch: x86_64 or arm64).
#
# Build on the oldest supported system (Ubuntu 22.04: glibc 2.35, GTK 4.6, GStreamer 1.20) so the
# binary runs on everything newer. Needs: dpkg-deb, rpmbuild (package `rpm`), the GTK and
# GStreamer development packages (docs/LINUX.md, "Building").
set -euo pipefail
VERSION="$1"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || { echo "bad version: $VERSION" >&2; exit 1; }

case "$(uname -m)" in
  x86_64) ARCH=x86_64; DEB_ARCH=amd64; RPM_ARCH=x86_64 ;;
  aarch64 | arm64) ARCH=arm64; DEB_ARCH=arm64; RPM_ARCH=aarch64 ;;
  *) echo "unsupported CPU: $(uname -m)" >&2; exit 1 ;;
esac
# Debian and RPM versions can't contain '-' (pre-releases: 1.2.3-pre.1 → 1.2.3~pre.1).
PKG_VERSION="${VERSION//-/\~}"
NAME="Annex-$VERSION-linux-$ARCH"
DIST="$ROOT/dist"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$DIST"

# The app's version follows the release.
awk -v v="$VERSION" '!done && /^version = "/ { print "version = \"" v "\""; done = 1; next } { print }' \
  "$ROOT/apps/linux/Cargo.toml" > "$WORK/Cargo.toml" && cp "$WORK/Cargo.toml" "$ROOT/apps/linux/Cargo.toml"
cargo update --manifest-path "$ROOT/apps/linux/Cargo.toml" -p annex-linux --offline 2>/dev/null \
  || cargo update --manifest-path "$ROOT/apps/linux/Cargo.toml" -p annex-linux
cargo build --release --locked --manifest-path "$ROOT/apps/linux/Cargo.toml"
BIN="$ROOT/apps/linux/target/release/annex"

# The installed tree, under /usr.
TREE="$WORK/tree"
install -Dm755 "$BIN" "$TREE/usr/bin/annex"
strip "$TREE/usr/bin/annex"
install -Dm644 "$ROOT/packaging/linux/dev.getannex.Annex.desktop" "$TREE/usr/share/applications/dev.getannex.Annex.desktop"
for size in 16 24 32 48 64 128 256 512; do
  install -Dm644 "$ROOT/assets/icon-$size.png" "$TREE/usr/share/icons/hicolor/${size}x${size}/apps/annex.png"
done
install -Dm644 "$ROOT/LICENSE" "$TREE/usr/share/doc/annex/copyright"

# .tar.gz: the same tree, to unpack anywhere (bin/annex runs in place).
mkdir -p "$WORK/$NAME"
cp -a "$TREE/usr/." "$WORK/$NAME/"
cp "$ROOT/LICENSE" "$WORK/$NAME/"
tar -C "$WORK" -czf "$DIST/$NAME.tar.gz" "$NAME"

# .deb (Debian, Ubuntu, Raspberry Pi OS, JetPack, Mint, Pop!_OS).
mkdir -p "$TREE/DEBIAN"
SIZE_KB="$(du -sk "$TREE/usr" | cut -f1)"
cat > "$TREE/DEBIAN/control" <<EOF
Package: annex
Version: $PKG_VERSION
Architecture: $DEB_ARCH
Maintainer: Annex contributors <https://github.com/NRanjan-17/Annex/issues>
Installed-Size: $SIZE_KB
Depends: libc6 (>= 2.35), libgtk-4-1 (>= 4.6), libgstreamer1.0-0 (>= 1.20), libgstreamer-plugins-base1.0-0 (>= 1.20), gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-libav, gstreamer1.0-pipewire, libxkbcommon0
Recommends: gstreamer1.0-plugins-ugly, wlr-randr
Suggests: gstreamer1.0-vaapi
Section: net
Priority: optional
Homepage: https://github.com/NRanjan-17/Annex
Description: Show another computer's screen, share this one, or extend it
 Annex connects computers on the same network, a cable, or the internet: view
 another computer's screen, share this one, or use this computer as an extra
 display. Video uses the system's H.264 encoders and decoders through GStreamer.
EOF
dpkg-deb --root-owner-group --build "$TREE" "$DIST/$NAME.deb"
rm -rf "$TREE/DEBIAN"

# .rpm (Fedora, RHEL and its rebuilds, openSUSE). H.264 comes from OpenH264 (Fedora's Cisco
# repository) or the libav/ugly plugins (RPM Fusion, Packman).
mkdir -p "$WORK/rpm/SPECS"
cat > "$WORK/rpm/SPECS/annex.spec" <<EOF
Name: annex
Version: ${PKG_VERSION}
Release: 1
Summary: Show another computer's screen, share this one, or extend it
License: MIT
URL: https://github.com/NRanjan-17/Annex
Requires: gtk4 >= 4.6, gstreamer1 >= 1.20, gstreamer1-plugins-base >= 1.20, gstreamer1-plugins-good, pipewire-gstreamer, libxkbcommon
Recommends: gstreamer1-plugins-bad-free, gstreamer1-plugin-openh264, wlr-randr
Suggests: gstreamer1-plugin-libav
AutoReqProv: no

%description
Annex connects computers on the same network, a cable, or the internet: view
another computer's screen, share this one, or use this computer as an extra
display. Video uses the system's H.264 encoders and decoders through GStreamer.

%install
cp -a $TREE/. %{buildroot}/

%files
/usr/bin/annex
/usr/share/applications/dev.getannex.Annex.desktop
/usr/share/icons/hicolor/*/apps/annex.png
/usr/share/doc/annex/copyright
EOF
rpmbuild --quiet -bb --target "$RPM_ARCH" --define "_topdir $WORK/rpm" --define "_build_id_links none" \
  "$WORK/rpm/SPECS/annex.spec"
cp "$WORK"/rpm/RPMS/*/annex-*.rpm "$DIST/$NAME.rpm"

ls -la "$DIST"/"$NAME".*
