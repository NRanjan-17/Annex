# Annex

Use another computer as an extra or mirrored display, over your local network, end-to-end
encrypted. Mac and Windows, in any combination. Open source (MIT), no cloud, no accounts,
no telemetry.

> **Status: M3 done**: mirroring works in all four directions (Mac↔Windows, Mac↔Mac,
> Windows↔Windows) with hardware codecs and PIN pairing. See [docs/PLAN.md](docs/PLAN.md) for
> the roadmap (real virtual displays, wired/QUIC, input forwarding).

| | Host (shares its screen) | Viewer (shows the stream) |
|---|---|---|
| macOS 14+ | ✅ menu bar app | ✅ window / full screen |
| Windows 10 2004+ / 11 | ✅ tray app | ✅ full screen |

## How it works

A Rust core (`core/`) does the protocol, encryption, discovery and sessions; native apps do
capture, encoding, decoding and UI: SwiftUI menu bar app on macOS (`apps/macos`), WinUI 3
tray app on Windows (`apps/windows`). Devices find each other with mDNS, connect over
**TLS 1.3 with mutual authentication**, and must be explicitly trusted once. After that their
key fingerprints are pinned. Details: [PROTOCOL](docs/PROTOCOL.md) · [SECURITY](docs/SECURITY.md).

## Install

Download from [Releases](https://github.com/NRanjan-17/Annex/releases):

- **macOS:** `Annex-<version>-macos-universal.dmg` → drag to Applications. The DMG also
  contains *Uninstall Annex.command*, which removes the app, its keys and its settings.
- **Windows:** `Annex-Setup-<version>-x64.exe` (or `-arm64`). You can install to any folder
  or drive, for just you or for all users. Uninstalling removes the app **and** all settings,
  identity keys and trusted devices.

## Use it

1. On the computer whose screen you want to share (the **host**):
   - **Mac:** menu bar icon → **Share this screen (Mirror)**. Allow Screen Recording when asked.
   - **Windows:** tray icon → **Share this screen** (or open Annex and turn on *Mirror to
     another computer*).
2. On the other computer (the **viewer**), click the host under *Hosts on this network* (or
   type its IP address).
3. The first time, the host shows a 6-digit PIN. Type it on the viewer (once per pair of devices).
4. The host's screen appears full screen on the viewer. Press **Esc** to disconnect.

Step-by-step manual test plans: [docs/TESTING.md](docs/TESTING.md).

## Build from source

Prerequisites: [Rust via rustup](https://rustup.rs) (the toolchain version is pinned in
`rust-toolchain.toml`), plus Xcode 16+ on macOS or the .NET 10 SDK on Windows.

```sh
# Rust core: tests and the latency benchmark
cd core && cargo test --workspace && cargo run --release -p annex-bench

# macOS app (Xcode builds the Rust core automatically)
xcodebuild test -project apps/macos/Annex.xcodeproj -scheme Annex -destination 'platform=macOS'
scripts/package-macos.sh            # → dist/Annex-<version>-macos-universal.dmg

# Windows app (PowerShell). Inno Setup 6 is needed for the installer
cargo build --manifest-path core/Cargo.toml -p annex-ffi --target x86_64-pc-windows-msvc
dotnet build apps/windows/src/Annex.App -p:Platform=x64
scripts/package-windows.ps1 -Arch x64   # → dist/Annex-Setup-<version>-x64.exe

# Headless CLI for testing (host/viewer/discovery/trust management)
cd core && cargo run -p annex-cli -- help
```

CI/CD, signing and releases: [docs/CICD.md](docs/CICD.md). Dependencies and licenses:
[docs/DEPENDENCIES.md](docs/DEPENDENCIES.md).

## License

MIT. See [LICENSE](LICENSE).
