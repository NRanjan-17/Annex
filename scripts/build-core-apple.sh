#!/bin/bash
# Builds the Rust core (annex-ffi) as a static library for the macOS app.
# Called from the Xcode "Build Rust core" phase; also usable by hand:
#   ARCHS="arm64 x86_64" CONFIGURATION=Release scripts/build-core-apple.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="$ROOT/apps/macos/AnnexFFI/lib"
ARCHS="${ARCHS:-$(uname -m)}"
CONFIGURATION="${CONFIGURATION:-Debug}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"

# Xcode runs scripts with a minimal PATH; pick up rustup's cargo if present.
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi
command -v cargo >/dev/null 2>&1 || { echo "error: cargo not found (install Rust via rustup)"; exit 1; }

if [ "$CONFIGURATION" = "Release" ]; then PROFILE=release; FLAG=--release; else PROFILE=debug; FLAG=; fi

LIBS=()
for arch in $ARCHS; do
  case "$arch" in
    arm64) TRIPLE=aarch64-apple-darwin ;;
    x86_64) TRIPLE=x86_64-apple-darwin ;;
    *) echo "error: unsupported arch $arch"; exit 1 ;;
  esac
  rustup target add "$TRIPLE" >/dev/null 2>&1 || true
  cargo build --manifest-path "$ROOT/core/Cargo.toml" -p annex-ffi --locked --target "$TRIPLE" $FLAG
  LIBS+=("$ROOT/core/target/$TRIPLE/$PROFILE/libannex_ffi.a")
done

mkdir -p "$OUT_DIR"
if [ "${#LIBS[@]}" -gt 1 ]; then
  lipo -create "${LIBS[@]}" -output "$OUT_DIR/libannex_ffi.a.tmp"
else
  cp "${LIBS[0]}" "$OUT_DIR/libannex_ffi.a.tmp"
fi
# Only replace the library when it changed, so Xcode doesn't relink needlessly.
if ! cmp -s "$OUT_DIR/libannex_ffi.a.tmp" "$OUT_DIR/libannex_ffi.a" 2>/dev/null; then
  mv "$OUT_DIR/libannex_ffi.a.tmp" "$OUT_DIR/libannex_ffi.a"
else
  rm "$OUT_DIR/libannex_ffi.a.tmp"
fi
