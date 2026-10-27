# Building Annex

Prerequisites: [Rust via rustup](https://rustup.rs) (the version is pinned in
`rust-toolchain.toml`), plus Xcode 16+ on macOS, the .NET 10 SDK and Visual Studio Build Tools
on Windows, or the GTK 4 and GStreamer development packages on Linux ([LINUX.md](LINUX.md#6a-building)).

```sh
# Rust core: tests and the latency benchmark
cd core && cargo test --workspace && cargo run --release -p annex-bench

# macOS app (Xcode builds the Rust core automatically)
xcodebuild test -project apps/macos/Annex.xcodeproj -scheme Annex -destination 'platform=macOS'
scripts/package-macos.sh                # → dist/Annex-<version>-macos-universal.dmg

# Windows app (PowerShell). Inno Setup 6 is needed for the installer
cargo build --manifest-path core/Cargo.toml -p annex-ffi --target x86_64-pc-windows-msvc
dotnet build apps/windows/src/Annex.App -p:Platform=x64
scripts/package-windows.ps1 -Arch x64   # → dist/Annex-Setup-<version>-x64.exe

# Linux app
cd apps/linux && cargo build --release  # → target/release/annex
scripts/package-linux.sh 1.2.3          # → dist/Annex-1.2.3-linux-<arch>.{deb,rpm,tar.gz}

# Headless CLI for testing (host, viewer, discovery, trust management)
cd core && cargo run -p annex-cli -- help
```

**How it's built:** a Rust core (`core/`) handles discovery, pairing, encryption, updates and the
streaming protocol (QUIC with forward error correction and adaptive bitrate, TCP fallback). Native
apps handle capture, hardware video encoding and decoding, and the UI: SwiftUI on macOS
(`apps/macos`), WinUI 3 on Windows (`apps/windows`), GTK 4 and GStreamer on Linux (`apps/linux`).

More: [roadmap](PLAN.md) · [manual test plans](TESTING.md) ·
[CI/CD, signing and releases](CICD.md) · [dependencies and licenses](DEPENDENCIES.md).
