#!/bin/bash
# Stamps a release version into the Rust workspace (and refreshes Cargo.lock entries for the
# workspace crates only). App versions are passed to xcodebuild / dotnet at build time.
#   scripts/stamp-version.sh 1.2.3
set -euo pipefail
VERSION="$1"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || { echo "bad version: $VERSION" >&2; exit 1; }
# Only the [workspace.package] version line (first `version = ` in the file).
awk -v v="$VERSION" '!done && /^version = "/ { print "version = \"" v "\""; done = 1; next } { print }' \
  "$ROOT/core/Cargo.toml" > "$ROOT/core/Cargo.toml.tmp" && mv "$ROOT/core/Cargo.toml.tmp" "$ROOT/core/Cargo.toml"
cargo update --manifest-path "$ROOT/core/Cargo.toml" --workspace --offline 2>/dev/null \
  || cargo update --manifest-path "$ROOT/core/Cargo.toml" --workspace
grep -m1 '^version = ' "$ROOT/core/Cargo.toml"
