# Annex for Linux: Plan

Status: **plan, nothing built.** Scope agreed: View, Share (Mirror, with mouse and keyboard) and Extend, on Wayland and X11. Items marked **[VERIFY]** get checked before anything depends on them.

---

## 1. Shape

The Linux app is written in **Rust**, so it uses the core crates directly. There is no C layer like the Mac (Swift) and Windows (C#) apps need.

```
┌──────────────────────── apps/linux (Rust) ─────────────────────────┐
│  GTK4 + libadwaita window · tray (StatusNotifierItem)               │
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
| UI | GTK4 + libadwaita (gtk4-rs) | The native look on GNOME, fine on KDE; mature Rust bindings |
| Media | GStreamer (gstreamer-rs) | One API over VA-API, NVENC and software codecs; renders straight into GTK4 |
| Wayland permissions | xdg-desktop-portal (ashpd) | The only sanctioned way to capture the screen and inject input on Wayland |
| Tray | StatusNotifierItem (ksni) | KDE has it built in; Ubuntu's GNOME ships the AppIndicator extension; stock GNOME needs it installed |

## 2. Video codecs: Annex ships none

`docs/PLAN.md` §2.3 rules out shipping a software H.264 codec (x264 is GPL; OpenH264's patent cover only applies to Cisco's own binaries). On Linux the OS-provided equivalent is the system's GStreamer plugins, so Annex picks the best H.264 encoder and decoder the computer already has, in this order:

1. **Hardware:** VA-API (`vah264enc`/`vah264dec`, Intel and AMD), NVENC/NVDEC (`nvh264enc`/`nvh264dec`, NVIDIA).
2. **Software, if installed:** `openh264enc`/`openh264dec` (Fedora enables Cisco's OpenH264 repository by default), `x264enc`, `avdec_h264`.

Annex links none of these; it loads whatever plugin is there at run time, which is not distribution.

- Fedora's standard Mesa drivers have H.264 turned off for patent reasons. Without RPM Fusion's drivers, a Fedora computer encodes in software: more processor load, so Share and Extend cap at 60 fps and 1080p-class sizes there.
- If no H.264 encoder exists at all, Share and Extend are off and Settings says which package to install. Viewing needs only a decoder, which almost every distribution has.
- Encoder settings follow PLAN.md §2.3: no B-frames, infinite GOP with IDR on request, about one frame of VBV, and the same bitrate formula and `bitrate_hint` handling as Windows.

## 3. Sharing and input

| | Wayland (GNOME, KDE) | X11 |
|---|---|---|
| Capture | ScreenCast portal → PipeWire (`pipewiresrc`) | XShm (`ximagesrc`) |
| Input on the shared screen | RemoteDesktop portal, in the **same** session as capture | XTest |
| Permission | One system dialog the first time; Annex keeps the portal's restore token so it doesn't ask again | None |

Viewer side, both: keys come from GTK as evdev codes and map to USB HID through a new Linux column in `annex-proto/src/keys.rs`. ⌃⌥⇧Q (Ctrl+Alt+Shift+Q) leaves, as on Mac and Windows.

## 4. Extend: the risky part

Extend needs a virtual monitor at the viewer's size and refresh rate. Linux has no single API for it.

| Desktop | Route | Status |
|---|---|---|
| KDE Plasma 6, Wayland | ScreenCast portal, source type `VIRTUAL` (4) | **[VERIFY]** |
| GNOME, Wayland | Portal `VIRTUAL` if offered; otherwise Mutter's `org.gnome.Mutter.ScreenCast` `RecordVirtual` (what GNOME Remote Desktop uses; works outside Flatpak only) | **[VERIFY]** |
| X11 | A real extra output from the `evdi` kernel module (DisplayLink's), set up with xrandr; or a driver's own `VIRTUAL` outputs | **[VERIFY]**; needs `evdi` installed |
| Anything else | Extend off, with a message saying why | n/a |

Phase 0 settles every **[VERIFY]** above before the app is built around it.

## 5. Phases

| # | What | Done when | Estimate |
|---|---|---|---|
| 0 | **Prove Extend.** In a Linux VM: read `AvailableSourceTypes` on GNOME and KDE, try `RecordVirtual`, try `evdi` on X11. Read the portal backends' source. | A table of what works where, replacing section 4's **[VERIFY]**s | 1–2 days |
| 1 | **Core prep** (testable on the Mac now): evdev key column; let Linux Tailscale peers through (`mesh.rs` `maybe_desktop`); Linux release names in `annex-update`; move `annexb.rs`/`geometry.rs`/`stream_size` out of `annex-media-win` into a shared crate; `annex host --extend` so the CLI can test Extend | `cargo test` passes on all three OSes in CI | 1–2 days |
| 2 | **App shell:** Nearby, This Computer, Paired Devices, Settings, About; tray; pairing; discovery; a CI-built `.deb` and `.rpm` | Installs and pairs with a Mac and a PC | 3–4 days |
| 3 | **View:** decode, window, full screen, input, ⌃⌥⇧Q, pointer lock for Extend | Views a Mac and a PC, Mirror and Extend | 3–4 days |
| 4 | **Share (Mirror):** portal and X11 capture, encoder choice, input injection | A Mac and a PC view and control it | 4–5 days |
| 5 | **Extend host + release:** virtual monitor per section 4; packages in `release.yml`; update notice | A Mac and a PC extend onto it | 4–6 days |

About 3–4 weeks in total.

## 6. Risks

| Risk | Effect | Plan |
|---|---|---|
| Virtual monitors not available on a desktop | No Extend there | Phase 0 first; a clear message otherwise |
| No hardware H.264 (Fedora, some VMs) | Software encode: higher processor load | Cap at 60 fps; say so in Settings |
| Pointer lock on Wayland | GTK4 has no API for it; Extend's relative pointer needs the pointer-constraints protocol through `gdk-wayland` | Build it in phase 3; fall back to absolute moves |
| Testing | No Linux machine here; the Mac can run an arm64 VM (GNOME Shell runs headless with virtual monitors) | VM for daily checks; a real computer for GPU encode, X11 and sign-off |
| Packaging | System GStreamer makes AppImage a poor fit | `.deb` and `.rpm` (x86_64, arm64) first; Flatpak later. Updates: a notice with a download link, no self-install |
