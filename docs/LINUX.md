# Annex for Linux: Plan

Status: **Phase 1 (core) done; Phase 0 under way (Pi 5 done, section 4a); no Linux app yet, so releases don't include Linux.** Scope agreed: View, Share (Mirror, with mouse and keyboard) and Extend, on Wayland and X11, everywhere Tailscale runs on Linux, including NVIDIA (desktop GPUs and Jetson) and Raspberry Pi. Items marked **[VERIFY]** get checked before anything depends on them.

---

## 0. Where it runs

The same Linux systems Tailscale installs on (tailscale.com/docs/install/linux):

| Family | Distributions | Package |
|---|---|---|
| Debian | Ubuntu, Debian, Raspberry Pi OS, Pop!_OS, Mint, NVIDIA JetPack (Ubuntu) | `.deb` |
| Red Hat | Fedora, RHEL, CentOS Stream, Rocky, Alma, Oracle Linux, Amazon Linux | `.rpm` |
| SUSE | openSUSE, SUSE Linux Enterprise | `.rpm` |
| Others | Arch, NixOS, VMware Photon | Arch `PKGBUILD`, Nix package, `.tar.gz` |

| CPU | Builds | Typical computers |
|---|---|---|
| x86_64 | yes | PCs, laptops, NVIDIA desktop GPUs |
| arm64 (aarch64) | yes | Raspberry Pi 3/4/5 (64-bit OS), NVIDIA Jetson (Orin, Thor), NVIDIA DGX Spark, ARM laptops |
| armhf (32-bit ARMv7) | yes | Raspberry Pi 2/3/4/5 on 32-bit Raspberry Pi OS |
| riscv64 | yes, tier 2 (built and smoke-tested, not hand-tested) | RISC-V boards |

Not targeted: Pi Zero/Pi 1 (ARMv6, too slow for live video) and 32-bit x86.

**Oldest systems:** GTK 4.6 and GStreamer 1.20 (Ubuntu 22.04, JetPack 6, Debian 12, RHEL 9). So the app uses plain GTK4 with Annex's own black-and-white styling, not libadwaita, which isn't on all of these.

**Desktops:** GNOME and KDE (Wayland and X11), wlroots compositors (Raspberry Pi OS's labwc, Sway) and other X11 desktops (Xfce, MATE, Cinnamon, LXQt).

---

**Test computers we have:** NVIDIA DGX Spark (arm64, DGX OS on Ubuntu, NVENC), Jetson AGX Thor (arm64, JetPack), Raspberry Pi 5 and Pi 4, and Ubuntu laptops (x86_64).

---

## 1. Shape

The Linux app is written in **Rust**, so it uses the core crates directly. There is no C layer like the Mac (Swift) and Windows (C#) apps need.

```
┌──────────────────────── apps/linux (Rust) ─────────────────────────┐
│  GTK4 window · tray (StatusNotifierItem)                            │
│        │ Rust calls                                                 │
│  annex-session · annex-discovery · annex-crypto · annex-update      │
│        │ encoded H.264 (Annex B)                                    │
│  annex-media-linux: GStreamer pipelines, portals, input             │
│    Share:  PipeWire (Wayland) / XShm (X11) → H.264 encoder          │
│    View:   H.264 decoder → GTK4 paintable                           │
│    Input:  RemoteDesktop portal (Wayland) / XTest (X11)             │
│    Extend: virtual monitor (see section 4)                          │
└─────────────────────────────────────────────────────────────────────┘
```

| Piece | Choice | Why |
|---|---|---|
| UI | GTK4 ≥ 4.6 (gtk4-rs), no libadwaita | On every system in section 0; mature Rust bindings; Annex draws its own look anyway |
| Media | GStreamer (gstreamer-rs) | One API over VA-API, NVENC and software codecs; renders straight into GTK4 |
| Wayland permissions | xdg-desktop-portal (ashpd) | The only sanctioned way to capture the screen and inject input on Wayland |
| Tray | StatusNotifierItem (ksni) | KDE has it built in; Ubuntu's GNOME ships the AppIndicator extension; stock GNOME needs it installed |

## 2. Video codecs: Annex ships none

`docs/PLAN.md` §2.3 rules out shipping a software H.264 codec (x264 is GPL; OpenH264's patent cover only applies to Cisco's own binaries). On Linux the OS-provided equivalent is the system's GStreamer plugins, so Annex picks the best H.264 encoder and decoder the computer already has, in this order:

1. **Hardware:**
   - Intel and AMD: VA-API (`vah264enc`/`vah264dec`, or the older `vaapih264enc`).
   - NVIDIA desktop GPUs: NVENC/NVDEC (`nvh264enc`/`nvh264dec`; GStreamer loads the driver's libraries at run time).
   - NVIDIA Jetson: JetPack's own plugins (`nvv4l2h264enc`/`nvv4l2decoder`). **[VERIFY]** on Thor (JetPack 7).
   - Raspberry Pi 3/4: the V4L2 codec (`v4l2h264enc`/`v4l2h264dec`), up to 1080p. **The Pi 5 has no H.264 hardware at all**, so it encodes and decodes in software.
2. **Software, if installed:** `openh264enc`/`openh264dec` (Fedora enables Cisco's OpenH264 repository by default), `x264enc`, `avdec_h264`.

Annex links none of these; it loads whatever plugin is there at run time, which is not distribution.

- Fedora's standard Mesa drivers have H.264 turned off for patent reasons. Without RPM Fusion's drivers, a Fedora computer encodes in software: more processor load, so Share and Extend cap at 60 fps and 1080p-class sizes there.
- Each device gets a size and rate cap from what its encoder manages (for example 1080p30 on a Pi 4 sharing, 1080p60 on a Pi 5 viewing in software). Phase 0 measures these.
- If no H.264 encoder exists at all, Share and Extend are off and Settings says which package to install. Viewing needs only a decoder, which almost every distribution has.
- Encoder settings follow PLAN.md §2.3: no B-frames, infinite GOP with IDR on request, about one frame of VBV, and the same bitrate formula and `bitrate_hint` handling as Windows.

## 3. Sharing and input

| | Wayland: GNOME, KDE | Wayland: wlroots (labwc, Sway) | X11 |
|---|---|---|---|
| Capture | ScreenCast portal → PipeWire (`pipewiresrc`) | The compositor's screencopy protocol (`ext-image-copy-capture`, `wlr-screencopy`) for a named output: no picker dialog. The portal (`xdg-desktop-portal-wlr`) only as a fallback | XShm (`ximagesrc`) |
| Input on the shared screen | RemoteDesktop portal, in the **same** session as capture | wlroots virtual pointer and virtual keyboard protocols (the portal has no RemoteDesktop there); both present in labwc 0.9 | XTest |
| Permission | One system dialog the first time; Annex keeps the restore token so it doesn't ask again | None | None |

Viewer side, both: keys come from GTK as evdev codes and map to USB HID through a new Linux column in `annex-proto/src/keys.rs`. ⌃⌥⇧Q (Ctrl+Alt+Shift+Q) leaves, as on Mac and Windows.

## 4. Extend: the risky part

Extend needs a virtual monitor at the viewer's size and refresh rate. Linux has no single API for it.

| Desktop | Route | Status |
|---|---|---|
| KDE Plasma 6, Wayland | ScreenCast portal, source type `VIRTUAL` (4) | **[VERIFY]** |
| GNOME, Wayland | Portal `VIRTUAL` if offered; otherwise Mutter's `org.gnome.Mutter.ScreenCast` `RecordVirtual` (what GNOME Remote Desktop uses; works outside Flatpak only) | **[VERIFY]** |
| wlroots (labwc, Sway), Wayland | A headless output, sized with `wlr-output-management` and captured with screencopy. Sway: `swaymsg create_output`. labwc has no IPC: its `VirtualOutputAdd`/`VirtualOutputRemove` actions only run from a key binding, so Annex adds two bindings in `~/.config/labwc/rc.xml` (labwc runs with `-m`, which merges it with the system config) and presses them with a virtual keyboard | **Works on labwc 0.9.7** (Pi 5); Sway **[VERIFY]** |
| X11 | A real extra output from the `evdi` kernel module (DisplayLink's), set up with xrandr; or a driver's own `VIRTUAL` outputs | **[VERIFY]**; needs `evdi` installed |
| Anything else | Extend off, with a message saying why | n/a |

Phase 0 settles every **[VERIFY]** above before the app is built around it.

## 4a. Phase 0 results so far

**Raspberry Pi 5** (Raspberry Pi OS on Debian 13, labwc 0.9.7 on Wayland, GStreamer 1.26, 16 GB):

- ScreenCast portal (`xdg-desktop-portal-wlr`): `AvailableSourceTypes` = 1 (monitors only, no `VIRTUAL`). No RemoteDesktop portal.
- labwc offers what Annex needs without the portal: screencopy (`ext-image-copy-capture`, `wlr-screencopy`), virtual pointer and keyboard, output management, and pointer constraints and relative pointer (for the viewer's pointer lock in Extend).
- **Extend works:** the key-binding route above created `ANNEX-1`, set it to 2704×1756 at 60 Hz (a MacBook Pro viewer's size) beside the main output, and removed it again.
- No H.264 hardware (the only codec device is `rpi-hevc-dec`). Software, on a moving test pattern (a real screen is somewhat slower):

| Encoder | 720p | 1080p | 1440p |
|---|---|---|---|
| `x264enc` (ultrafast, zero latency) | 134 fps | 71 fps | 44 fps |
| `openh264enc` (low complexity) | 50 fps | 22 fps | n/a |

- Decoding a 1080p60 stream: `avdec_h264` about 275 fps, `openh264dec` about 135 fps. Viewing is no problem.
- So on a Pi 5: View up to 4K; Share and Extend at 1080p60 with x264 installed, 720p30 with OpenH264 only.

Still to measure: Pi 4 (V4L2 hardware encoder), Jetson AGX Thor, DGX Spark, an x86_64 laptop (VA-API, X11), GNOME and KDE virtual monitors.

## 5. Phases

| # | What | Done when | Estimate |
|---|---|---|---|
| 0 | **Prove Extend and the encoders.** In a Linux VM: read `AvailableSourceTypes` on GNOME and KDE, try `RecordVirtual`, a wlroots headless output, `evdi` on X11. On real hardware: the encoder on a Pi 4, a Pi 5 and a Jetson, and the size and rate each sustains | A table of what works where, replacing every **[VERIFY]** | 2–3 days |
| 1 | **Core prep** (testable on the Mac now): evdev key column; let Linux Tailscale peers through (`mesh.rs` `maybe_desktop`); Linux release names in `annex-update`; move `annexb.rs`/`geometry.rs`/`stream_size` out of `annex-media-win` into a shared crate; `annex host --extend` so the CLI can test Extend | `cargo test` passes on all three OSes in CI | 1–2 days |
| 2 | **App shell:** Nearby, This Computer, Paired Devices, Settings, About; tray; pairing; discovery; CI builds for x86_64, arm64, armhf and riscv64 (`.deb`, `.rpm`, `.tar.gz`) | Installs and pairs with a Mac and a PC on x86_64 and a Pi | 4–5 days |
| 3 | **View:** decode, window, full screen, input, ⌃⌥⇧Q, pointer lock for Extend | Views a Mac and a PC, Mirror and Extend | 3–4 days |
| 4 | **Share (Mirror):** portal and X11 capture, encoder choice, input injection | A Mac and a PC view and control it | 4–5 days |
| 5 | **Extend host + release:** virtual monitor per section 4; packages in `release.yml`; update notice | A Mac and a PC extend onto it | 4–6 days |

About 4–5 weeks in total; the extra week is the wider set of CPUs, packages and desktops.

## 6. Releases

Releases build Linux packages from **Phase 2** on, when there is an app to package. Until then
`release.yml` builds only the Mac `.dmg` and the Windows installers; CI already builds and tests the
Rust core on Ubuntu for every push.

From Phase 2, every release adds, for x86_64, arm64, armhf and riscv64:

- `Annex-<version>-linux-<arch>.deb` (Ubuntu, Debian, Raspberry Pi OS, JetPack, DGX OS)
- `Annex-<version>-linux-<arch>.rpm` (Fedora, RHEL and its relatives, openSUSE, Amazon, Oracle)
- `Annex-<version>-linux-<arch>.tar.gz` (Arch `PKGBUILD`, NixOS, Photon, anything else)

They're listed in the signed `SHA256SUMS.txt` like the Mac and Windows files, and the app's update
check (`annex-update`) offers the file for its own package type and CPU.

## 7. Risks

| Risk | Effect | Plan |
|---|---|---|
| Virtual monitors not available on a desktop | No Extend there | Phase 0 first; a clear message otherwise |
| No hardware H.264 (Fedora, some VMs) | Software encode: higher processor load | Cap at 60 fps; say so in Settings |
| Pointer lock on Wayland | GTK4 has no API for it; Extend's relative pointer needs the pointer-constraints protocol through `gdk-wayland` | Build it in phase 3; fall back to absolute moves |
| Testing | No Linux machine here; the Mac can run an arm64 VM (GNOME Shell runs headless with virtual monitors) | VM for daily checks; a real computer for GPU encode, X11 and sign-off |
| Packaging | System GStreamer makes AppImage a poor fit | `.deb`, `.rpm`, `.tar.gz` and an Arch `PKGBUILD` for four CPUs; Flatpak later. Updates: a notice with a download link, no self-install |
| Raspberry Pi 5 | No H.264 hardware: software encode and decode | Lower caps on the Pi 5; later, HEVC viewing (the Pi 5 decodes HEVC in hardware; the core already negotiates it) |
| 32-bit and RISC-V builds | Slow emulated CI builds; few testers | Cross-compile in CI; riscv64 is tier 2 |
