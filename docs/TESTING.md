# Testing

## Automated (runs in CI and locally)

| Suite | Command | Covers |
|---|---|---|
| Rust core | `cd core && cargo test --workspace` | protocol (incl. proptest fuzzing), crypto, trust store, sessions over real TLS, tamper/replay attacks, FFI ABI, discovery parsing |
| Live mDNS | `cd core && cargo test -p annex-discovery -- --ignored` | advertise + browse on this machine (needs multicast) |
| macOS app | `xcodebuild test -project apps/macos/Annex.xcodeproj -scheme Annex -destination 'platform=macOS'` | Annex B ↔ AVCC, VideoToolbox encode → decode round trip (HW and SW), Swift ↔ core loopback session |
| .NET interop | `ANNEX_NATIVE_DIR=core/target/debug dotnet test apps/windows/tests/Annex.Interop.Tests` | event parsing, full session through P/Invoke |
| Benchmark | `cd core && cargo run --release -p annex-bench` | loopback capture→display time over TLS (no GPU) |

### Measured on the development Mac

macOS 27.0.1 (Apple Silicon), Xcode 27.1, Rust 1.99.1:

- Loopback benchmark, 1080p-sized frames at 60 fps: **p50 0.4 ms, p95 0.5 ms** (transport + session only).
- VideoToolbox H.264 encode, 640×360: hardware low-latency **2–4 ms/frame**; Apple's
  **software** encoder buffers ~17 frames (**~280 ms**) even in real-time mode, and
  `MaxFrameDelayCount` is unsupported. That's why the host defaults to the hardware encoder.
- Real H.264 (VideoToolbox) streamed CLI host → CLI viewer: 300/300 received pictures decode.

## Tested OS versions

| Component | Tested on | Not yet tested |
|---|---|---|
| Rust core | macOS 27.0.1 (local); Ubuntu 24.04, macOS 26, Windows Server 2025 (CI) | |
| macOS app | macOS 27.0.1 build + tests + launch | Live screen capture with a real viewer (needs Screen Recording approval), macOS 14–26 |
| Windows app | C# compile-checked on macOS; CI builds it | **Running it on Windows 10/11** |

## Manual tests: M1 (Mac host → Windows viewer, LAN, encrypted)

These need two machines (or a Windows VM with network bridging). Put both on the same network.

### 1. Install

- **Mac:** open `Annex-<version>-macos-universal.dmg`, drag Annex to Applications, launch it.
  An unsigned build is blocked by Gatekeeper the first time: right-click Annex → **Open** →
  Open. A display icon appears in the menu bar.
- **Windows:** run `Annex-Setup-<version>-x64.exe`. On the folder page choose any folder,
  for example on `D:\`. Pick "Install for me only" (no admin) or "all users" (adds firewall
  rules). An unsigned build triggers SmartScreen: **More info → Run anyway**. After install,
  Annex starts and an icon appears in the **system tray** (it may be in the overflow ^ area).

### 2. Mirror the Mac to Windows

1. On the Mac, click the menu bar icon and turn on **Share this screen (Mirror)**. macOS asks
   for **Screen & System Audio Recording** permission. Allow it in System Settings, then
   toggle sharing off and on again. The menu shows "Waiting for a viewer on port 47800…".
   If macOS asks to find devices on the **local network**, allow it.
2. On Windows, open Annex from the tray icon. Within ~5 s the Mac appears under "Hosts on
   this network". (Windows may ask to allow Annex through the firewall: allow **Private**.)
   Alternatively type the Mac's IP address and press **Connect**.
3. **Pairing:** the Mac shows a floating panel with a 6-digit PIN; Windows asks for it.
   Type the PIN on Windows and click **Pair**. (Try a wrong PIN once first: both sides must
   report that pairing failed, and nothing is added to *Trusted devices*.)
4. Expected: a full-screen window on Windows shows the Mac screen within ~1 s. Move a window
   on the Mac; it moves on Windows. The Windows app shows "Viewing <Mac> — W×H @ 60 fps"
   and a latency line (p50/p95).
5. Press **Esc** on Windows: the viewer closes, and the Mac menu returns to "Waiting for a viewer".
6. Connect again: no PIN this time (the devices are pinned).
7. On the **Mac**, click **Remove** next to the PC under *Trusted devices*, then connect from
   Windows again: a new PIN must be shown and pairing repeats. Click **Don't Allow** on the
   Mac: Windows reports "pairing_cancelled" and no session starts.
8. Lockout: enter a wrong PIN 5 times in a row. The 6th attempt must fail immediately with
   "pairing_locked" and no PIN panel on the Mac (it unlocks after 10 minutes).

### 3. Tray behaviour (Windows)

- Close the main window (X): Annex stays in the tray. Left-click the tray icon: the window
  comes back. Right-click: the menu lists discovered hosts ("View <name>"), "Disconnect"
  while viewing, and "Quit Annex". Quit removes the icon.

### 4. Uninstall cleanup (Windows)

1. Note the install folder you chose (e.g. `D:\Apps\Annex`).
2. Settings → Apps → Annex → Uninstall (or Start → Annex → Uninstall).
3. Expected: the install folder is gone, `%LOCALAPPDATA%\Annex` (identity key, trusted
   devices) is gone, and for all-users installs the "Annex" firewall rule is gone
   (`netsh advfirewall firewall show rule name=Annex` reports no rule).

### 5. Uninstall cleanup (macOS)

Open the DMG and run **Uninstall Annex.command** (right-click → Open the first time). Expected:
`/Applications/Annex.app` and `~/Library/Application Support/Annex` are removed and Annex no
longer appears under Privacy & Security → Screen & System Audio Recording.

### 6. Latency measurement (glass to glass)

`annex-bench` measures transport only. For capture→display between machines, read the
latency line in the Windows app (it uses the clock-synchronised capture timestamp), or set
`ANNEX_LATENCY_CSV=C:\temp\latency.csv` before starting the Windows app for a per-frame log.
For true glass-to-glass, film both screens with a phone at 240 fps showing a stopwatch page
on the Mac and count frames.

### Headless alternative (no GUI)

On any two machines with Rust installed:
```
annex host --replay clip.h264 --width 1280 --height 720   # machine A
annex viewer <A's IP> --dump received.h264                # machine B
```
