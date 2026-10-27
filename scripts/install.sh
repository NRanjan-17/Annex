#!/bin/sh
# Installs the latest Annex on macOS or Linux:
#   curl -fsSL https://raw.githubusercontent.com/NRanjan-17/Annex/main/scripts/install.sh | sh
#
# macOS: copies Annex.app to /Applications (or ~/Applications). Linux: installs the .deb or .rpm
# with apt, dnf or zypper (asks for your password), or unpacks the .tar.gz into ~/.local.
# Before anything is installed, the download is checked: on Linux against the release's
# SHA256SUMS.txt and its Ed25519 signature (the key the in-app updater trusts; needs OpenSSL 3), on
# macOS against SHA256SUMS.txt and Apple's notarized Developer ID signature for Annex's team.
#
# The package manager installs what Annex needs with it (GStreamer, wl-clipboard, …).
#
# Uninstall (removes Annex, its settings and identity, and the packages installed only for it;
# ANNEX_YES=1 skips the question):
#   curl -fsSL https://raw.githubusercontent.com/NRanjan-17/Annex/main/scripts/install.sh | sh -s -- --uninstall
#
# ANNEX_VERSION=1.2.3 picks a version; ANNEX_DRY_RUN=1 downloads and checks but installs nothing.
# ANNEX_BASE_URL: where the release files are, for testing.
set -eu

REPO="NRanjan-17/Annex"
# Signs every release's SHA256SUMS.txt (core/annex-update UPDATE_PUBLIC_KEY), as an SPKI PEM.
PUBLIC_KEY="MCowBQYDK2VwAyEAlevTBcRpQctbXdMPj5wdPCx0VDJ/Kf2SWpHyBEIF3DY="
# Apple team that signs and notarizes the Mac app.
APPLE_TEAM="667V7CCT79"
say() { printf '%s\n' "$*"; }
fail() { printf 'Annex: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "this needs $1"; }
run() {
  if [ "${ANNEX_DRY_RUN:-}" = 1 ]; then say "would run: $*"; else "$@"; fi
}
sudo_run() {
  if [ "$(id -u)" = 0 ]; then run "$@"; else need sudo; run sudo "$@"; fi
}

uninstall_linux() {
  say "This removes Annex, its settings, its identity and the list of trusted devices,"
  say "and the packages that were installed only for Annex."
  if [ "${ANNEX_YES:-}" = 1 ]; then
    answer=y
  elif (: < /dev/tty) 2>/dev/null; then
    printf 'Continue? [y/N] '
    read -r answer < /dev/tty || answer=
  else
    fail "run this in a terminal to confirm, or set ANNEX_YES=1"
  fi
  case "$answer" in [Yy]*) ;; *) say "Cancelled."; exit 0 ;; esac
  pkill -x annex >/dev/null 2>&1 || true
  if command -v dpkg >/dev/null 2>&1 && dpkg -s annex >/dev/null 2>&1; then
    # Only what Annex pulled in: packages that become unneeded once it's gone, not ones that
    # were already unneeded before.
    orphans() { apt-get -s autoremove 2>/dev/null | awk '/^Remv /{ print $2 }' | sort; }
    orphans > "$TMP/before"
    sudo_run apt-get purge -y annex
    extra=$(orphans | comm -13 "$TMP/before" -)
    if [ -n "$extra" ]; then
      say "Removing what was installed for Annex: $(echo $extra)"
      # shellcheck disable=SC2086
      sudo_run apt-get purge -y $extra
    fi
  elif command -v rpm >/dev/null 2>&1 && rpm -q annex >/dev/null 2>&1; then
    # Both remove the dependencies nothing else needs, and only those.
    if command -v dnf >/dev/null 2>&1; then
      sudo_run dnf remove -y annex
    else
      sudo_run zypper --non-interactive remove --clean-deps annex
    fi
  fi
  run rm -rf "$HOME/.local/opt/annex"
  [ -L "$HOME/.local/bin/annex" ] && run rm -f "$HOME/.local/bin/annex"
  run rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/annex" "${XDG_CONFIG_HOME:-$HOME/.config}/annex" \
    "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/annex.desktop"
  say "Annex has been removed."
}

if [ "${1:-}" = --uninstall ]; then
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT
  case "$(uname -s)" in
    Linux) uninstall_linux ;;
    Darwin) fail "on a Mac, use Uninstall Annex in Annex's Settings" ;;
    *) fail "this script is for macOS and Linux" ;;
  esac
  exit 0
fi

need curl
need uname

# The newest release: GitHub redirects /releases/latest to /releases/tag/v<version>.
VERSION="${ANNEX_VERSION:-}"
if [ -z "$VERSION" ]; then
  tag_url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
    || fail "couldn't reach GitHub"
  VERSION="${tag_url##*/v}"
fi
case "$VERSION" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) fail "couldn't find the latest version (got '$VERSION')" ;;
esac
# ANNEX_BASE_URL: a folder with the release files instead of GitHub (for testing).
BASE="${ANNEX_BASE_URL:-https://github.com/$REPO/releases/download/v$VERSION}"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# The release's checksums, once: on Linux only if their signature verifies.
sums() {
  [ -f "$TMP/SHA256SUMS.txt" ] && return
  curl -fsSL -o "$TMP/SHA256SUMS.txt" "$BASE/SHA256SUMS.txt" || fail "couldn't download SHA256SUMS.txt"
  [ "$(uname -s)" = Linux ] || return 0
  curl -fsSL -o "$TMP/SHA256SUMS.txt.sig" "$BASE/SHA256SUMS.txt.sig" || fail "couldn't download SHA256SUMS.txt.sig"
  command -v openssl >/dev/null 2>&1 || fail "checking the signature needs openssl (OpenSSL 3)"
  printf -- '-----BEGIN PUBLIC KEY-----\n%s\n-----END PUBLIC KEY-----\n' "$PUBLIC_KEY" > "$TMP/annex.pem"
  openssl pkeyutl -verify -pubin -inkey "$TMP/annex.pem" -rawin -in "$TMP/SHA256SUMS.txt" \
    -sigfile "$TMP/SHA256SUMS.txt.sig" >/dev/null 2>&1 \
    || fail "the release's signature doesn't verify (or this openssl is older than 3.0); nothing was installed"
  say "Checked the release's signature"
}

# Downloads one release file into $TMP and checks it against the release's checksums.
fetch() {
  curl -fsSL -o "$TMP/$1" "$BASE/$1" 2>/dev/null || return 1
  sums
  want=$(awk -v f="$1" '$2 == f || $2 == "*" f { print $1 }' "$TMP/SHA256SUMS.txt")
  [ -n "$want" ] || fail "$1 isn't listed in SHA256SUMS.txt"
  if command -v sha256sum >/dev/null 2>&1; then
    got=$(sha256sum "$TMP/$1" | awk '{ print $1 }')
  else
    got=$(shasum -a 256 "$TMP/$1" | awk '{ print $1 }')
  fi
  [ "$got" = "$want" ] || fail "$1 doesn't match its checksum; nothing was installed"
  say "Downloaded and checked $1"
}

install_macos() {
  need hdiutil
  dmg="Annex-$VERSION-macos-universal.dmg"
  fetch "$dmg" || fail "couldn't download $dmg"
  dest=/Applications
  [ -w "$dest" ] || { dest="$HOME/Applications"; mkdir -p "$dest"; }
  mnt="$TMP/mnt"
  mkdir -p "$mnt"
  hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mnt" "$TMP/$dmg" >/dev/null 2>&1 \
    || fail "couldn't open the disk image"
  app="$mnt/Annex.app"
  unmount() { hdiutil detach "$mnt" -force >/dev/null 2>&1; }
  [ -d "$app" ] || { unmount; fail "the disk image has no Annex.app"; }
  # Signed by Annex's Apple team and notarized by Apple.
  team=$(codesign -dv "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p')
  if ! codesign --verify --deep --strict "$app" 2>/dev/null || [ "$team" != "$APPLE_TEAM" ] \
    || ! spctl -a -t exec "$app" 2>/dev/null; then
    unmount
    fail "Annex.app isn't signed and notarized by Annex's developer; nothing was installed"
  fi
  say "Checked Apple's signature (team $team)"
  if pgrep -x Annex >/dev/null 2>&1; then
    say "Quitting the running Annex"
    run osascript -e 'quit app "Annex"' || true
    sleep 2
  fi
  run rm -rf "$dest/Annex.app"
  run ditto "$app" "$dest/Annex.app"
  unmount
  run open "$dest/Annex.app"
  say "Annex $VERSION is in $dest."
}

install_linux() {
  case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *) fail "there's no package for $(uname -m) yet; build it from source: https://github.com/$REPO/blob/main/docs/LINUX.md" ;;
  esac
  name="Annex-$VERSION-linux-$arch"
  if command -v apt-get >/dev/null 2>&1 && command -v dpkg >/dev/null 2>&1; then
    fetch "$name.deb" || no_linux_package
    # apt reads the file as its own user: keep it somewhere it can.
    chmod 755 "$TMP" && chmod 644 "$TMP/$name.deb"
    sudo_run apt-get install -y "$TMP/$name.deb"
  elif command -v dnf >/dev/null 2>&1; then
    fetch "$name.rpm" || no_linux_package
    sudo_run dnf install -y "$TMP/$name.rpm"
  elif command -v zypper >/dev/null 2>&1; then
    fetch "$name.rpm" || no_linux_package
    sudo_run zypper --non-interactive install --allow-unsigned-rpm "$TMP/$name.rpm"
  else
    fetch "$name.tar.gz" || no_linux_package
    run mkdir -p "$HOME/.local/opt" "$HOME/.local/bin"
    run rm -rf "$HOME/.local/opt/annex"
    run tar -xzf "$TMP/$name.tar.gz" -C "$HOME/.local/opt"
    run mv "$HOME/.local/opt/$name" "$HOME/.local/opt/annex"
    run ln -sf "$HOME/.local/opt/annex/bin/annex" "$HOME/.local/bin/annex"
    say "Installed in ~/.local/opt/annex; run 'annex' (~/.local/bin must be on your PATH)."
    say "Annex needs GTK 4.6+ and GStreamer 1.20+ with its good, bad and libav plugins."
    return
  fi
  say "Annex $VERSION is installed. Open it from the app menu, or run 'annex'."
}

no_linux_package() {
  fail "Annex $VERSION has no Linux package for this computer. Linux packages start with the release after 0.2.0; until then, build from source: https://github.com/$REPO/blob/main/docs/LINUX.md"
}

case "$(uname -s)" in
  Darwin) install_macos ;;
  Linux) install_linux ;;
  *) fail "this script is for macOS and Linux; on Windows run: irm https://raw.githubusercontent.com/$REPO/main/scripts/install.ps1 | iex" ;;
esac
