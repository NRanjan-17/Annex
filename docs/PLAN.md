# Annex: Implementation Plan

Status: **decisions locked (section 9); nothing built yet.** Items marked **[VERIFY]** are facts I am fairly but not fully sure of; each one gets checked before we depend on it.

Dev machine this was planned on: macOS 27.0.1 (26A434), Xcode 27.1, Apple Silicon. Rust and .NET are not installed yet.

---

## 1. Architecture overview

```
┌──────────────────────── HOST ────────────────────────┐        ┌─────────────────────── VIEWER ───────────────────────┐
│  Native UI (SwiftUI menu bar / WinUI 3 tray)          │        │  Native UI + full-screen renderer                     │
│        │ C ABI (annex-ffi)                            │        │        │ C ABI                                        │
│  ┌─────▼──────────── Rust core ─────────────────┐     │        │  ┌─────▼──────────── Rust core ──────────────────┐   │
│  │ session · pairing/trust · discovery · proto  │     │ TLS1.3 │  │ session · pairing/trust · discovery · proto   │   │
│  │ packetizer · FEC · congestion/ABR · transport│◄────┼─QUIC──►┼──│ reassembly · FEC · jitter/latency · transport │   │
│  └─────▲────────────────────────────────────────┘     │        │  └─────┬─────────────────────────────────────────┘   │
│        │ encoded NAL units (H.264/HEVC)               │        │        │ encoded NAL units                            │
│  Capture → Encode  (platform media layer)             │        │  Decode → Render  (platform media layer)              │
│  Virtual display manager (Extend mode)                │        │                                                       │
└───────────────────────────────────────────────────────┘        └───────────────────────────────────────────────────────┘
```

Key design rule: **only compressed bitstreams and control messages cross the FFI boundary.** Raw frames stay on the GPU inside the platform media layer: IOSurface/CVPixelBuffer on macOS, D3D11 textures on Windows. That keeps FFI cheap and the Rust core GPU-agnostic, so it can be tested on Ubuntu.

### Repository layout

```
/core                     Rust workspace (rust-toolchain.toml pinned)
  annex-proto             wire format, message types, versioning (no I/O)
  annex-crypto            identity keys, SPAKE2 pairing, trust store, cert pinning verifiers
  annex-transport         TCP+TLS (M1), QUIC (M5), shared Transport trait
  annex-session           state machine: hello → auth → stream config → streaming → teardown
  annex-discovery         mDNS advertise/browse + manual peers; backend trait
  annex-media             platform-neutral media helpers: Annex B, letterboxing, pointer coordinates, NV12 copies, stream sizes
  annex-media-win         Windows capture/encode/decode via `windows` crate (WGC, D3D11, Media Foundation)
  annex-ffi               C ABI (cbindgen header) used by Swift and C#
  annex-cli               headless host/viewer for testing (synthetic frames, file dump)
  annex-bench             loopback latency benchmark (no GPU/display)
/apps/macos               Xcode project: Annex.app (SwiftUI menu bar), AnnexMedia (SCK/VT/Metal),
                          AnnexVirtualDisplay (thin ObjC wrapper around private CGVirtualDisplay)
/apps/windows             Annex.sln: Annex.App (WinUI 3), Annex.Service (Windows service),
                          Annex.Interop (P/Invoke to annex-ffi), tests
/apps/linux               GTK4 app in Rust: GStreamer media, Mutter/portals, input (docs/LINUX.md)
/drivers/windows/AnnexIdd IddCx UMDF driver (C++), INF, test-sign scripts
/installer/windows        Inno Setup script (driver + service + app), optional MSIX for app-only
/docs                     PROTOCOL.md, CICD.md, SECURITY.md, CABLES.md, TESTING.md, DEPENDENCIES.md
/.github                  workflows/ci.yml, workflows/release.yml, dependabot.yml
```

### Language split

| Component | Language | Why |
|---|---|---|
| Protocol, crypto, session, discovery, transport, FEC, ABR | Rust | One implementation, fully unit-testable on all three OSes |
| macOS capture/encode/decode/render | Swift | ScreenCaptureKit, VideoToolbox, Metal are first-class in Swift |
| macOS virtual display | ObjC header + Swift wrapper | Private API needs a hand-written `@interface` declaration and stays isolated in one file |
| Windows capture/encode/decode/render | Rust (`windows` crate) | Avoids heavy COM interop in C#, testable from `annex-cli` and `cargo test`, reuses one D3D11 device |
| Windows UI and service | C# (.NET 8 LTS or later, WinUI 3) | As requested. Stays thin: settings, pairing UI, tray, SwapChainPanel host |
| Windows IddCx driver | C++ | IddCx headers are C/C++. Rust UMDF bindings for IddCx are immature |

**FFI choice:** a single hand-written C ABI in `annex-ffi`. `cbindgen` generates the header. Swift uses it through a module map, and C# uses `csbindgen`-generated `[LibraryImport]` stubs. I picked this over UniFFI because UniFFI has no first-party C# backend, and one ABI for both platforms is easier to version.

---

## 2. Tech choices and trade-offs

### 2.1 Security: **[DECIDE] TLS 1.3 / QUIC (recommended) vs Noise**

| | TLS 1.3 over TCP (M1) → QUIC (M5), via rustls + quinn | Noise_XX (snow) over TCP → custom encrypted UDP |
|---|---|---|
| One crypto stack across milestones | **Yes.** quinn uses rustls, so the same pinned-key verifier works for both | Noise over TCP is easy. UDP needs our own record layer: nonces, replay window, rekey (snow's `StatelessTransportState` helps) |
| Replay protection | Built in (TLS sequence numbers, QUIC packet numbers). 0-RTT disabled | We implement a sliding replay window per packet |
| Key rotation | TLS 1.3 KeyUpdate / QUIC key update, plus fresh ECDHE per session | Noise `rekey()` on a schedule we design |
| Congestion control, datagrams, streams | QUIC gives us these (RFC 9221 datagrams for video, streams for control) | Everything custom |
| Identity format | Self-signed Ed25519 certs (rcgen) with SPKI pinned via custom `ServerCertVerifier` / `ClientCertVerifier`. No CA and no web PKI | Raw X25519 static keys pinned directly (simpler) |

**Recommendation: TLS 1.3 + QUIC with mutual auth.** Most of the security-critical surface (record layer, replay, rekey, congestion) is then well-reviewed library code instead of ours.

**Pairing (applies either way).** A 6-digit PIN cannot be used directly as a Noise PSK or a TLS password: an active attacker who sees one handshake can brute-force 10⁶ PINs offline. A naive "compare 6 digits derived from the handshake hash" is also unsafe, because a MITM can grind ephemeral keys until the codes collide. The plan:

- **PIN flow:** The host shows a random 6-digit PIN that is valid for 2 minutes. The two sides open a TLS connection that is encrypted but not yet trusted, then run **SPAKE2** (RustCrypto `spake2` crate) with the PIN, bound to the TLS exporter (RFC 5705/8446 `export_keying_material`). Both sides then exchange key-confirmation MACs over (both cert fingerprints and the exporter). An attacker gets one online guess per attempt. Attempts are rate-limited (3 tries, then a new PIN).
- **QR flow:** The host shows a QR code containing the host fingerprint, its address, and a 128-bit one-time secret. The viewer scans it with a webcam or phone camera. The secret replaces the PIN in the same SPAKE2 step, so the flow is identical but at full entropy. Useful mainly for laptop viewers that have a camera.
- **After pairing:** each side stores the peer's SPKI hash and a user-chosen name in the trust store, after an explicit "Trust 'Alice's MacBook'? Fingerprint ab12…" confirmation (TOFU with confirmation). Later connections are rejected unless the pinned key matches. **Revocation** deletes the trust entry on this side and sends a best-effort `Unpair` to the peer. Keys are stored in the macOS Keychain and in Windows DPAPI-protected storage (CNG key storage for the private key).
- No accounts or telemetry. Discovery and connections stay on local links unless the user turns on connecting over the internet (planned, opt-in, off by default; see INTERNET-CONNECT.md): then a rendezvous server and relay see addresses and timing, never content, and LAN use never contacts them. Otherwise the only internet traffic of its own is the update check: GitHub's releases list at launch and once a day, on by default and one switch to turn off (About → Updates), as README and SECURITY.md say.

### 2.2 Capture

| Platform | Mirror | Extend | Notes |
|---|---|---|---|
| macOS | ScreenCaptureKit `SCStream` filtered by `SCDisplay` | Same API, using the CGVirtualDisplay's display ID | Gives IOSurface-backed CVPixelBuffers that go straight to VideoToolbox. Supports cursor capture and 60/120 fps |
| Windows | Windows Graphics Capture (`CreateForMonitor`), with no yellow border on Win11 | **[DECIDE]** WGC on the IddCx monitor first. Swap-chain → shared texture later as an optimization | See 2.6. DXGI Desktop Duplication is the fallback for older builds and for secure-desktop capture from the service |

### 2.3 Encode and decode

| | macOS | Windows |
|---|---|---|
| HW encode | VideoToolbox (`kVTCompressionPropertyKey_RealTime`, no B-frames, `AllowFrameReordering=false`, low-latency rate control where available) | Media Foundation encoder MFTs: NVENC / QuickSync / AMF exposed as hardware MFTs, `CODECAPI_AVLowLatencyMode`, D3D11 device manager for zero-copy |
| HW decode | VideoToolbox `VTDecompressionSession` → CVPixelBuffer → Metal texture | MF H.264 decoder with D3D11VA → NV12 texture → shader → swapchain |
| **SW fallback** | VideoToolbox's software encoder, by disabling `EnableHardwareAcceleratedVideoEncoder` | Microsoft's built-in software H.264 MFT (in all editions except N/KN, which need the Media Feature Pack) |
| Test-only codec | `raw+lz4` codec, plus optional `openh264` built from source behind a dev feature, for CI/Ubuntu | same |

**Software codec choice for M1.** I suggest the **OS-provided** software codecs. They ship with patent licensing covered by Apple and Microsoft and need no extra dependencies. The alternatives have problems:
- x264 is GPL, which is incompatible with distributing Annex as MIT.
- OpenH264's patent coverage only applies to Cisco's prebuilt binaries, so shipping it would mean a runtime download (the Firefox approach).

The `openh264` crate built from source is fine for tests but must never ship.

**HEVC:** Negotiated by capability; H.264 is the default. On Windows, HEVC decode may need the paid *HEVC Video Extensions* store package, so it can't be assumed.

**Low-latency settings:** No B-frames, an infinite GOP with periodic intra-refresh (or IDR on demand when the viewer reports loss), VBV buffer of about one frame, and slice-based encode where the encoder supports it.

### 2.4 Transport

- **M1:** TCP + TLS 1.3 (tokio-rustls). Length-prefixed frames, one connection, with control and video multiplexed by message type. TCP_NODELAY is on, and the viewer drops frames that arrive late. This is the baseline that proves the pipeline.
- **M5:** QUIC (quinn).
  - Control runs on a reliable bidirectional stream.
  - Video uses **unreliable datagrams**, with each frame fragmented to path MTU. MTU discovery is on, and we start at 1200.
  - Loss recovery uses **Reed-Solomon FEC** (`reed-solomon-simd`, MIT) per frame. Selective NACK covers keyframes only, and a frame that misses its deadline is abandoned (we request a keyframe or rely on intra-refresh).
  - **ABR:** viewer receive reports (delivery rate, one-way delay gradient, loss) drive a GCC-like delay-based controller in `annex-media`. It adjusts encoder bitrate and optionally resolution or fps. Quinn's own congestion controller is set to BBR. If it fights our controller, we use a custom quinn `Controller`.
- The wired link uses the same protocol. Path selection prefers a link-local wired interface when both peers advertise one.

### 2.5 Discovery

- Service type `_annex._udp` (QUIC) / `_annex._tcp` (M1). TXT records carry protocol version, device ID (fingerprint prefix), roles, and capability flags. **No PIN or secret is ever put in TXT.**
- Backend trait. macOS uses the native `dns_sd` API (libSystem), which plays well with mDNSResponder and Local Network privacy. Windows and Linux use the `mdns-sd` crate.
- Manual entry: IP or hostname plus port, including IPv6 link-local with scope ID (`fe80::1%en5`).

### 2.6 Virtual display (Extend)

#### macOS: CGVirtualDisplay (private API)

- `AnnexVirtualDisplay/` holds one ObjC header declaring `CGVirtualDisplayDescriptor`, `CGVirtualDisplaySettings`, `CGVirtualDisplayMode`, `CGVirtualDisplay`, plus a Swift facade (`VirtualDisplay.create(spec) / .apply(modes) / .destroy()`). Nothing else in the app touches the private symbols. Availability is checked at runtime with `NSClassFromString`, so a missing class disables Extend gracefully instead of crashing.
- Descriptor:
  - name "Annex Display"
  - vendorID: a fixed Annex value
  - productID: a fixed value
  - serialNum: derived from the viewer's pinned fingerprint, for a stable identity per viewer
  - sizeInMillimeters: computed from the viewer's reported physical size or DPI
  - maxPixelsWide/High
  - color primaries/white point (sRGB / P3)
  - terminationHandler
- Settings: `hiDPI = 1`. Modes are the viewer's native resolution (marked as the default), standard scaled modes at the same aspect ratio, and the requested refresh rates (60, plus 120 if the viewer supports it).
- macOS remembers arrangement and scaling by vendor/product/serial, which gives the "restored on reconnect" behavior.
- Mode changes made in System Settings are observed with `CGDisplayRegisterReconfigurationCallback`. The SCStream is reconfigured to the new size, and the viewer gets a `StreamConfig` message. Target is under 1 s.
- Lifetime: the display exists only while the `CGVirtualDisplay` object is alive in our process. On disconnect we release it, and macOS moves windows back. **Crash safety is free**: if the process dies, the display disappears.
- **Correction to the brief: the macOS helper cannot be a LaunchDaemon.** CGVirtualDisplay and ScreenCaptureKit must run inside the user's GUI login session (WindowServer connection plus the TCC Screen Recording grant). A root LaunchDaemon has neither. **[DECIDE] Proposal:** no privileged helper on macOS at all. The menu bar app owns the virtual display, capture, and encode. If you want Annex to keep hosting after the UI quits, I can add an optional **LaunchAgent** registered via `SMAppService.agent` (macOS 13+) that is embedded in the app bundle, so TCC attributes it to Annex. Neither option needs root.
- Studied for behavior only, not copied: **DeskPad** (MIT). Tested-versions matrix in `docs/TESTING.md`, starting with macOS 27.0.1. **[DECIDE]** minimum macOS: I propose **14 Sonoma**, for ScreenCaptureKit maturity.
- Consequences: no Mac App Store. The app is distributed with Developer ID signing, Hardened Runtime, and notarization. Private API use does not block notarization.

#### Windows: IddCx UMDF driver

- `AnnexIdd` is written fresh against the IddCx docs, using Microsoft's IndirectDisplay sample as the architectural reference.
- **[VERIFY] Licensing:** the Windows-driver-samples repo is MS-PL, not MIT. Code copied from it must keep MS-PL headers, which is allowed alongside an MIT project but has to be marked. I'll keep any sample-derived files clearly labeled, or write clean-room code.
- **virtual-display-rs** (MIT) is a reference for the control-plane design.
- The adapter is created at device add. **Monitors are created and removed dynamically** with `IddCxMonitorCreate` / `IddCxMonitorArrival` / `IddCxMonitorDeparture`, triggered by an IOCTL from the Annex service. Up to N monitors run at once (N=4 initially).
- Per-monitor EDID:
  - manufacturer ID "ANX" (needs a PNP ID; see risks)
  - monitor name descriptor "Annex Display"
  - serial derived from the viewer fingerprint
  - preferred timing equal to the viewer's native resolution
  - monitor range limits
- Each viewer gets a **stable connector index and ContainerId** that the service persists. Windows keys its display configuration on these together with the EDID, so they are what makes position, resolution, and scale survive reconnects and reboots.
- Mode list: implement `EvtIddCxParseMonitorDescription`, `MonitorGetDefaultModes`, `MonitorQueryTargetModes`. When the user changes resolution in Settings, `EvtIddCxMonitorAssignSwapChain` is called again with the new mode. The swap-chain processor thread always drains frames, because DWM stalls if it doesn't.
- **Frame path [DECIDE]:**
  - Phase A (M4): the driver only drains the swap-chain. The host app captures the Annex monitor with WGC like any other monitor. This is simple and robust, at a cost of roughly one frame of extra latency.
  - Phase B (optional, after M5): the driver copies frames into a D3D11 shared texture that the host app opens through the service (WUDFHost runs in a different session, so handle brokering is needed). This saves about one frame and avoids WGC quirks.
- Orientation: Settings handles rotation for indirect displays. **[VERIFY]** whether IddCx needs explicit rotation support. If it does, the driver also reports portrait modes and the host rotates before encoding.
- **Windows service** (`Annex.Service`, LocalSystem):
  - The only process allowed to open the driver's control device (device interface ACL: SYSTEM + Administrators).
  - Exposes a named pipe with an ACL for interactive users.
  - Validates requests and persists viewer-to-connector mapping.
  - **Watchdog:** if the UI app's pipe drops, it removes that app's monitors, so windows are never stranded. The driver also tears down monitors on file-handle cleanup as a second layer.
- GPU choice on hybrid laptops: use `IddCxAdapterSetRenderAdapter` to pin rendering to the GPU we encode on.
- **Signing:**
  - Dev: `bcdedit /set testsigning on`, a self-signed test cert in the Root and TrustedPublisher stores, `Inf2Cat`, and `signtool`. Scripted in `drivers/windows/scripts/`.
  - Distribution: **attestation signing** through Microsoft Partner Center (Hardware program). This requires an **EV code-signing certificate** to register the account. It's a manual, documented step.
- Install and uninstall: `pnputil /add-driver AnnexIdd.inf /install`, root-enumerated device creation (`devgen` / SwDeviceCreate from the service), and `pnputil /delete-driver /uninstall /force` on uninstall.

### 2.7 Viewer render

- macOS: `CAMetalLayer` with `displaySyncEnabled` tuned for latency. CVPixelBuffer → `CVMetalTextureCache` → one draw. Full screen on a chosen display, letterboxed if the aspect ratio differs.
- Windows: WinUI 3 `SwapChainPanel` with a flip-model swapchain, `DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT`, max frame latency 1, and tearing allowed when full screen.

### 2.8 Input forwarding (M6, off by default)

- Viewer events become an `Input` message on the control stream, normalized to stream coordinates.
- macOS host: `CGEventPost`. Requires the **Accessibility** permission.
- Windows host: `SendInput`. It **can't inject into elevated or UAC windows** unless the process has `uiAccess` (signed, installed in Program Files). Documented as a limitation.
- It's a per-pairing toggle that is also negotiated per session. The host must have enabled it locally, so a viewer can never turn it on unilaterally.

### 2.9 Dependencies (initial; full list with licenses kept in `docs/DEPENDENCIES.md`, enforced by `cargo deny`)

| Crate / package | Purpose | License |
|---|---|---|
| tokio | async runtime | MIT |
| rustls (ring provider) | TLS 1.3 | Apache-2.0 / ISC / MIT |
| tokio-rustls | TLS over TCP | MIT / Apache-2.0 |
| quinn (M5) | QUIC | MIT / Apache-2.0 |
| rcgen | self-signed Ed25519 certs | MIT / Apache-2.0 |
| spake2, sha2, hkdf, hmac, subtle (RustCrypto) | pairing | MIT / Apache-2.0 |
| mdns-sd | discovery (Win/Linux) | MIT / Apache-2.0 |
| ciborium | CBOR control messages | Apache-2.0 |
| reed-solomon-simd (M5) | FEC | MIT |
| tracing | logging (local only) | MIT |
| windows | Win32/WinRT bindings | MIT / Apache-2.0 |
| thiserror, anyhow | errors | MIT / Apache-2.0 |
| proptest (dev) | protocol fuzz/property tests | MIT / Apache-2.0 |
| CommunityToolkit.Mvvm (C#) | UI MVVM | MIT |
| Microsoft.WindowsAppSDK | WinUI 3 | MIT |
| Inno Setup (build tool) | Windows installer | Inno Setup License (permissive) |

No Swift package dependencies are planned on macOS; system frameworks are enough.

**[DECIDE] Windows installer tool:** I recommend **Inno Setup** for the `.exe` installer. MSIX **cannot install drivers**, so MSIX would only cover a "Mirror/Viewer only" edition without Extend. WiX v6's new maintenance-fee EULA makes it a worse fit for an open-source project.

---

## 3. Wire protocol (to be specified in `docs/PROTOCOL.md`)

- **ALPN** `annex/1`. The major version is in the ALPN string. Minor version and capabilities go in `Hello`, and unknown CBOR map keys are ignored.
- **Control messages:** CBOR maps with a 1-byte type and a length prefix.
  - `Hello{proto_minor, device_name, roles, caps}`
  - `PairStart / PairMsg / PairConfirm`
  - `SessionRequest{mode: mirror|extend, viewer_display{px, mm, hz[], hdr}, codecs[], input_wanted}`
  - `StreamConfig{codec, w, h, fps, color}`
  - `KeyframeRequest`, `ReceiverReport{...}`, `BitrateHint`
  - `ClockPing/Pong` (for latency measurement), `Input{...}`, `Unpair`, `Bye{reason}`
- **Video:** fixed binary header containing `{frame_id u32, capture_ts_us u64, flags (key, last), frag_idx u16, frag_cnt u16, fec_k/m}` + payload. AEAD comes from TLS/QUIC, so the header doesn't need its own MAC.
- **Clock sync:** NTP-style offset estimation over `ClockPing`, used to report capture→display latency across two machines.

---

## 4. Permission, driver, and signing hurdles

### macOS
| Hurdle | Plan |
|---|---|
| **Screen Recording** (TCC) | Requested on first host use, with an onboarding screen explaining it. macOS 15+ periodically re-prompts ScreenCaptureKit apps; documented. **[VERIFY]** how macOS 27 behaves |
| **Local Network privacy** (macOS 15+) | `NSLocalNetworkUsageDescription` and `NSBonjourServices` (`_annex._tcp`, `_annex._udp`) in Info.plist. Without these, discovery silently fails |
| Incoming connections | Application firewall prompt on first listen. A signed app keeps its allow decision |
| **Accessibility** (M6 input) | Requested only when input forwarding is enabled |
| Camera (QR pairing on the viewer) | `NSCameraUsageDescription`, requested only when scanning |
| Private API | Runtime availability check. Extend is disabled with a clear message if the API is missing. Not App Store eligible |
| Signing | Developer ID Application cert, Hardened Runtime, notarytool (App Store Connect API key), stapled .dmg. Requires an Apple Developer Program membership ($99/yr) |
| TCC and dev builds | TCC grants are tied to the code signature. Ad-hoc dev builds lose the permission on every rebuild, so I'll use a stable local "Apple Development" signing identity |

### Windows
| Hurdle | Plan |
|---|---|
| Firewall | The installer adds inbound rules for the app (UDP/TCP port + mDNS 5353). Discovery fails on "Public" network profiles; documented, with an in-app hint |
| Driver loading | Dev: test-signing mode + test cert. Release: attestation signing via Partner Center (EV cert required), done manually |
| Secure Boot | Test-signing mode requires Secure Boot to be off, or a VM. Attestation-signed drivers work with Secure Boot on |
| Service install | Needs admin. Done by the installer |
| UAC / secure desktop | WGC shows nothing on the UAC prompt or lock screen. A frozen frame there is documented as expected behavior |
| HEVC | Optional store extension; H.264 is the default |
| N editions | Need the Media Feature Pack for MF codecs. Detected and explained |
| App signing | signtool with an OV/EV cert, or Azure Trusted Signing (eligibility depends on region and entity type **[VERIFY]**). Unsigned installers trigger SmartScreen |

---

## 5. CI/CD design

### `ci.yml` (push + pull_request)

`concurrency: group: ci-${{ github.ref }}, cancel-in-progress: true`. `permissions: contents: read`. All third-party actions are pinned by full SHA, with a version comment.

| Job | Runner | Steps |
|---|---|---|
| `core` (matrix) | ubuntu / macos / windows -latest | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace --locked`. Windows-only crates are excluded on other OSes via `cfg`/workspace exclude. Cargo cache via `Swatinem/rust-cache` |
| `bench` | ubuntu-latest | `annex-bench --loopback --codec raw --frames 600`, uploads JSON + CSV as an artifact. Fails only if p95 regresses more than X% against a committed baseline (warn-only at first) |
| `macos-app` | macos-latest | Pin Xcode via `xcode-select` (version from `.xcode-version`). Build the Rust static lib, then `xcodebuild build test` (unsigned, `CODE_SIGNING_ALLOWED=NO`). SwiftPM cache |
| `windows-app` | windows-latest | `actions/setup-dotnet` (version from `global.json`), build the Rust DLL, `dotnet build` + `dotnet test`. NuGet cache |
| `windows-driver` | windows-latest | msbuild with the **WDK via NuGet** (`Microsoft.Windows.WDK.x64/ARM64`, versions pinned) so the build doesn't depend on a preinstalled WDK. x64 + ARM64. Build only |
| `audit` | ubuntu-latest | `cargo audit`, `cargo deny check advisories licenses bans sources` |
| `codeql` | separate workflow, on push/PR + weekly | Languages: rust, swift (macOS runner, manual build), csharp, c-cpp (driver) |

`dependabot.yml` covers cargo (`/core`), nuget (`/apps/windows`), swift (`/apps/macos`, if any packages are added), and github-actions. Updates run weekly and are grouped.

### `release.yml` (tags `v*.*.*` + `workflow_dispatch` with `dry_run: true`)

1. **`version`**: parse the tag and fail on mismatch. Stamp the version into the workspace `Cargo.toml` (`[workspace.package] version`), Xcode `MARKETING_VERSION`/`CURRENT_PROJECT_VERSION` (passed as build settings), .NET `/p:Version`, and the INF `DriverVer` (stampinf).
2. **`build-macos`** (environment `release`, needs approval):
   1. Build Rust for aarch64 and x86_64, then `lipo`.
   2. `xcodebuild archive` with a universal build, Developer ID, and Hardened Runtime.
   3. Import the cert into a temporary keychain, which is deleted in an `always()` step.
   4. `notarytool submit --wait` using an App Store Connect API key.
   5. `stapler staple`, then `create-dmg` / `hdiutil`, then sign, notarize, and staple the dmg.
3. **`build-windows`** (environment `release`):
   1. Matrix x64 + ARM64. Build the Rust DLL, the .NET app (self-contained Windows App SDK), and the service.
   2. Sign with signtool or Azure Trusted Signing.
   3. Package with Inno Setup (`Annex-Setup-x64.exe`, `-arm64.exe`), then sign the installer.
4. **`build-driver`**: build AnnexIdd x64/ARM64, then `Inf2Cat`. CI produces a **test-signed** package (`AnnexIdd-testsigned-<ver>.zip`) and an **unsigned CAB ready for Partner Center submission**. Release notes say which one is included.
5. **`publish`** (`permissions: contents: write`, only in this job):
   1. `sha256sum` over all artifacts.
   2. SBOM per artifact with syft (SPDX JSON).
   3. `gh release create --draft --generate-notes`.
   4. Skipped when `dry_run`. Dry runs upload everything as workflow artifacts and skip notarization and signing unless asked.

Secrets live only in the `release` environment, which has required reviewers:
- `MACOS_CERT_P12_BASE64`, `MACOS_CERT_PASSWORD`
- `ASC_API_KEY_ID`, `ASC_API_ISSUER_ID`, `ASC_API_KEY_P8`
- `WIN_SIGN_*` or `AZURE_TRUSTED_SIGNING_*` (via OIDC federated credentials, so no long-lived secret)

Cert files are written only to `$RUNNER_TEMP` and deleted. They are never echoed or uploaded.

**Reproducibility, stated plainly:**
- Releases are *rebuildable from the tag* with a locked `Cargo.lock`, `rust-toolchain.toml`, `.xcode-version`, `global.json`, pinned WDK NuGet versions, and SHA-pinned actions.
- Bit-for-bit reproducibility of the Rust staticlib is a goal I'll check (`SOURCE_DATE_EPOCH`, `--remap-path-prefix`).
- Signed or notarized outputs and MSVC/Xcode binaries won't be byte-identical, and I won't claim they are.

**Not verifiable on hosted runners** (documented in `docs/CICD.md`): real screen capture, virtual display creation, driver load, hardware encoders, mDNS across machines, and Wi-Fi/wired latency. These are covered by manual test scripts in `docs/TESTING.md`, plus an optional `self-hosted` job label for later.

---

## 6. Milestones (refined)

Every milestone ends with: build, tests green locally and in CI, a README "How to run" section, `docs/TESTING.md` updated with manual steps for what I couldn't run, and the latency benchmark updated.

| | Scope | Exit criteria |
|---|---|---|
| **M1** | Rust core (proto, TLS-over-TCP transport, session, discovery). **Temporary pairing:** manual fingerprint confirmation on both screens. Mac host (SCK + VT software encode). Windows viewer (MF software decode + D3D11 render). `annex-cli` + `annex-bench`. ci.yml + dependabot + codeql | Mac → Windows mirror at 1080p60 on LAN, encrypted. Mac→Mac loopback on this machine (host + viewer on 127.0.0.1). CI green |
| **M2** | HW encode/decode on both sides. PIN + QR pairing (SPAKE2), trust store, revoke UI | p95 glass-to-glass under 80 ms Wi-Fi at 1080p60. Pairing unit tests, including MITM/wrong-PIN tests |
| **M3** | Windows host (WGC + MF encode in Rust). Mac viewer (VT decode + Metal). All four directions | Matrix of all four directions passes manual tests |
| **M4** | CGVirtualDisplay wrapper. IddCx driver + service + test-sign scripts. Viewer-native default mode, live mode changes, multi-display, stable identity, clean teardown | Your four Extend acceptance tests pass on both OSes |
| **M5** | QUIC transport, datagram video + FEC + NACK, ABR, wired-link detection and preference, `docs/CABLES.md` | Under 50 ms p95 wired, under 80 ms on good 5 GHz Wi-Fi, 4K60 on wired. Loss injection tests (netem / Network Link Conditioner) |
| **M6** | Input forwarding (toggle, off by default), settings UI, dmg + Inno installer (+ optional MSIX viewer-only), release.yml | Dry-run release produces all artifacts with checksums and SBOMs. Draft release created from a test tag |

---

## 7. Testing strategy

- **Unit tests (Rust):**
  - Protocol encode/decode roundtrips, plus proptest fuzzing of decoders (they must never panic on arbitrary bytes).
  - Version negotiation.
  - Pairing: correct PIN, wrong PIN, MITM simulation with two separate handshakes, rate limiting, and exporter binding.
  - Trust store pin mismatch → reject.
  - Revoked peer → reject.
  - Replay of captured records → rejected by TLS. Explicit tests on our framing layer cover truncation and reordering.
  - FEC recovery under loss patterns.
  - ABR controller on synthetic traces.
- **Integration (Rust):** `annex-cli host --synthetic` ↔ `annex-cli viewer --null` over loopback, with a loss/latency shim for M5.
- **Latency benchmark `annex-bench`:**
  - Loopback mode (CI) timestamps capture→encode→send→recv→decode→"present" per frame and reports p50/p95/p99 as JSON/CSV.
  - Cross-machine mode uses the clock-synced `capture_ts` and logs capture→display on the viewer.
  - A visual "glass-to-glass" mode renders a frame counter on the host, for checking with a phone slow-mo camera.
- **macOS:** XCTest for the Swift layers (mode list generation, display spec from viewer metrics, FFI wrapper).
- **Windows:** MSTest/xUnit for the C# view models and service pipe protocol. Driver EDID generation is extracted into a pure C++ unit that is testable off-device.

### What I can't test on this machine
Everything Windows: the viewer, host, driver, service, installer, and WGC/MF. Also cross-machine Mac↔Mac, since I can only loop back on one Mac. For each milestone I'll give exact manual steps.

**[DECIDE]** Do you have a Windows PC available? Failing that, a **Windows 11 ARM64 VM** (UTM/Parallels/VMware Fusion) on this Mac is a good dev target for the driver: test-signing is safe there, and the ARM64 build gets exercised. GPU encode can't be tested in a VM.

---

## 8. Risk list

| # | Risk | Likelihood / Impact | Mitigation |
|---|---|---|---|
| 1 | Apple changes or removes CGVirtualDisplay in a macOS update | Med / High | Isolated wrapper, runtime check, graceful fallback to Mirror, tested-versions matrix, quick patch releases |
| 2 | Windows driver attestation signing blocked (no EV cert / Partner Center account) | Med / High | Ship Mirror-only without the driver. Extend needs test-signing until signed. Budget for EV cert |
| 3 | Can't test Windows locally | High / High | Windows VM + your manual test runs. Keep Windows media code in Rust so most logic is unit-tested on Mac/Linux |
| 4 | Latency targets missed on Wi-Fi (power save, contention, AWDL on macOS) | Med / Med | ABR, intra-refresh, measure early in M2. Document that AWDL (AirDrop/Continuity) causes periodic jitter spikes on macOS Wi-Fi |
| 5 | Pairing protocol mistake | Low / Critical | Use SPAKE2 + TLS exporter binding (standard composition), no invented primitives, adversarial tests, request external review before 1.0 |
| 6 | Mac↔Windows Thunderbolt/USB4 networking interop is unreliable | High / Med | Treat USB-Ethernet adapters as the universally supported wired option. TB/USB4 bridge documented per tested pair in CABLES.md |
| 7 | WGC capture of the IddCx display adds latency or has quirks | Med / Med | Phase B shared-texture path. DXGI duplication fallback |
| 8 | HEVC unavailable on Windows viewers | High / Low | H.264 default, HEVC by capability |
| 9 | macOS TCC re-prompts annoy users | Med / Low | Document. Use SCContentSharingPicker if it helps on newer macOS **[VERIFY]** |
| 10 | EDID manufacturer PNP ID "ANX" may be assigned to someone else | Med / Low | Check the UEFI PNP ID registry. Use an unassigned code, or register one |
| 11 | Windows display config not persisting if connector index or ContainerId drift | Med / Med | Service persists a stable mapping. Explicit acceptance test |
| 12 | Media Foundation hardware MFT quirks per vendor (NVENC/AMF/QSV) | High / Med | Capability probing, per-vendor workarounds, SW fallback |
| 13 | Codec patent / license contamination (x264, openh264 source builds) | Low / High | OS codecs only in shipped builds. cargo-deny bans GPL |
| 14 | Hosted CI images change (Xcode/WDK) | Med / Low | Pinned versions, WDK via NuGet, Dependabot for actions |
| 15 | Scope: this is a very large project for one dev loop | High / Med | Strict milestone gates. M1 kept deliberately thin |

---

## 8b. Changes made during implementation

- **Crate layout:** M1 merged `annex-transport` into `annex-session`. FEC/ABR (`annex-media`)
  arrive with QUIC in M5.
- **Mac host encoder:** hardware VideoToolbox from M1 on. Apple's software H.264 encoder adds
  ~280 ms of buffering (measured), which defeats the purpose. Software stays as a fallback only.
  The Windows viewer uses Microsoft's software H.264 decoder in M1.
- **Minimal Mac viewer in M1** (AVSampleBufferDisplayLayer) so the pipeline can be verified
  end to end on a single Mac. The full Mac viewer work stays in M3.
- **Windows tray app and installers** (Inno Setup, any install drive, full uninstall cleanup;
  macOS .dmg with uninstaller) were pulled forward from M6 at the user's request.

## 9. Decisions (made on the recommended defaults)

1. **Crypto:** TLS 1.3 over TCP (M1), then QUIC (M5), via rustls/quinn, with SPAKE2 PIN/QR pairing and pinned keys.
2. **macOS:** no helper process. The menu bar app owns the virtual display, capture, and encode.
3. **Windows Extend frame path:** WGC capture of the IddCx monitor first. The shared-texture swap-chain path is a later optimization.
4. **Windows media layer** is in Rust (`windows` crate). C# is used for the UI and service only.
5. **Minimum OS:** macOS 14+, Windows 10 22H2+ / Windows 11.
6. **Installer:** Inno Setup `.exe` (includes the driver), plus an optional MSIX viewer-only build.
7. **Still open (needed by M1 for the Windows viewer):** what Windows hardware is available for testing. Signing accounts (Apple Developer, Windows code-signing cert) are only needed by M4/M6.
