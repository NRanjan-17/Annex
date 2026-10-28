# Annex for Linux

Status: **The app is built (`apps/linux`): View, Share (Mirror) and Extend. Releases include `.deb`, `.rpm` and `.tar.gz` for x86_64 and arm64.** Tested on a Jetson AGX Thor (GNOME 50) and a Raspberry Pi 5 (labwc); results in section 4a. Scope agreed: View, Share (Mirror, with mouse and keyboard) and Extend, on Wayland and X11, everywhere Tailscale runs on Linux, including NVIDIA (desktop GPUs and Jetson) and Raspberry Pi. Items marked **[VERIFY]** get checked before anything depends on them.

---

## 0. Where it runs

The same Linux systems Tailscale installs on (tailscale.com/docs/install/linux):

| Family | Distributions | Package |
|---|---|---|
| Debian | Ubuntu, Debian, Raspberry Pi OS, Pop!_OS, Mint, NVIDIA JetPack (Ubuntu) | `.deb` |
| Red Hat | Fedora, RHEL, CentOS Stream, Rocky, Alma, Oracle Linux, Amazon Linux | `.rpm` |
| SUSE | openSUSE, SUSE Linux Enterprise | `.rpm` |
| Others | Arch, NixOS, VMware Photon | `.tar.gz` (a `PKGBUILD` and Nix package later) |

| CPU | Builds | Typical computers |
|---|---|---|
| x86_64 | yes | PCs, laptops, NVIDIA desktop GPUs |
| arm64 (aarch64) | yes | Raspberry Pi 3/4/5 (64-bit OS), NVIDIA Jetson (Orin, Thor), NVIDIA DGX Spark, ARM laptops |
| armhf (32-bit ARMv7) | from source (section 6a) | Raspberry Pi 2/3/4/5 on 32-bit Raspberry Pi OS |
| riscv64 | from source, tier 2 | RISC-V boards |

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
│  annex-session · annex-discovery · annex-crypto                     │
│        │ encoded H.264 (Annex B)                                    │
│  GStreamer pipelines, Mutter/portals, wlroots protocols, XTest      │
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
   - NVIDIA Jetson: JetPack 6's plugins (`nvv4l2h264enc`/`nvv4l2decoder`). On Thor (JetPack 7) the GPU is a desktop-style one: `nvh264enc` works and `nvv4l2decoder` doesn't.
   - Raspberry Pi 3/4: the V4L2 codec (`v4l2h264enc`/`v4l2h264dec`), up to 1080p. **The Pi 5 has no H.264 hardware at all**, so it encodes and decodes in software.
2. **Software, if installed:** `openh264enc`/`openh264dec` (Fedora enables Cisco's OpenH264 repository by default), `x264enc`, `avdec_h264`.

Annex links none of these; it loads whatever plugin is there at run time, which is not distribution. `ANNEX_ENCODER` / `ANNEX_DECODER` force one by name.

**Decoding prefers `avdec_h264` (slice threads) over hardware when it's installed:** on the Thor it showed frames in p50 12.5 ms against 33 ms for `nvh264dec`, which holds frames back. Hardware decoders follow, then OpenH264. Software encoders stream at 30 fps.

- Fedora's standard Mesa drivers have H.264 turned off for patent reasons. Without RPM Fusion's drivers, a Fedora computer encodes in software: more processor load, so Share and Extend cap at 60 fps and 1080p-class sizes there.
- Each device gets a size and rate cap from what its encoder manages (for example 1080p30 on a Pi 4 sharing, 1080p60 on a Pi 5 viewing in software). Phase 0 measures these.
- If no H.264 encoder exists at all, Share and Extend are off and Settings says which package to install. Viewing needs only a decoder, which almost every distribution has.
- Encoder settings follow PLAN.md §2.3: no B-frames, infinite GOP with IDR on request, about one frame of VBV, and the same bitrate formula and `bitrate_hint` handling as Windows.

## 3. Sharing and input

| | Wayland: GNOME, KDE | Wayland: wlroots (labwc, Sway) | X11 |
|---|---|---|---|
| Capture | GNOME: Mutter's own ScreenCast D-Bus API (no dialog). KDE and others: ScreenCast portal → PipeWire (`pipewiresrc`) | The compositor's screencopy protocol (`ext-image-copy-capture`, `wlr-screencopy`) for a named output: no picker dialog. The portal (`xdg-desktop-portal-wlr`) only as a fallback | XShm (`ximagesrc`) |
| Input on the shared screen | GNOME: Mutter RemoteDesktop, same session as capture. Others: RemoteDesktop portal | wlroots virtual pointer and virtual keyboard protocols (the portal has no RemoteDesktop there); both present in labwc 0.9 | XTest |
| Permission | GNOME none; portal: one system dialog the first time, then the saved restore token | None | None |

Viewer side, both: keys come from GTK as evdev codes and map to USB HID through a new Linux column in `annex-proto/src/keys.rs`. ⌃⌥⇧Q (Ctrl+Alt+Shift+Q) leaves, as on Mac and Windows.

## 4. Extend: the risky part

Extend needs a virtual monitor at the viewer's size and refresh rate. Linux has no single API for it.

| Desktop | Route | Status |
|---|---|---|
| KDE Plasma 6, Wayland | ScreenCast portal, source type `VIRTUAL` (4) | Built; **[VERIFY]** on a KDE computer |
| GNOME, Wayland | Mutter's `org.gnome.Mutter.ScreenCast` `RecordVirtual` (what GNOME Remote Desktop uses; works outside Flatpak only) | Built. On the Thor's headless GNOME the display is added but Mutter sends no frames; **[VERIFY]** on a logged-in Ubuntu desktop |
| wlroots (labwc, Sway), Wayland | A headless output, sized with `wlr-output-management` and captured with screencopy. Sway: `swaymsg create_output`. labwc has no IPC: its `VirtualOutputAdd`/`VirtualOutputRemove` actions only run from a key binding, so Annex adds two bindings in `~/.config/labwc/rc.xml` (labwc runs with `-m`, which merges it with the system config) and presses them with a virtual keyboard | **Works on labwc 0.9.7** (Pi 5); Sway built, **[VERIFY]** |
| X11 | A real extra output from the `evdi` kernel module (DisplayLink's), set up with xrandr; or a driver's own `VIRTUAL` outputs | **Not built yet**: Share says Extend isn't available |
| Anything else | Extend off, with a message saying why | n/a |

Where Extend isn't available, the Share page says so and viewers get Mirror.

labwc notes: the virtual keyboard must send the modifier state itself (`wl_keyboard.modifiers`) or labwc ignores the chord; Annex reloads labwc with SIGHUP to its PID (`XDG_CURRENT_DESKTOP` is often empty there, so Annex finds labwc by its process); new outputs start at 0,0, so Annex moves `ANNEX-1` to the right with `wlr-randr --pos`. On a Pi with no monitor attached, labwc drops its placeholder `NOOP-1` once `ANNEX-1` exists, so the Annex display becomes the only screen.

Not built yet: pointer lock in the viewer for Extend (the viewer sends absolute positions), and the update notice.

## 4a. Test results

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

**The app on the Pi 5** (labwc, screencopy + x264, virtual pointer and keyboard), viewed from the Mac CLI:

- Mirror: about 30 fps, p50 about 10 ms, p95 60–78 ms (p95 still to tune).
- Extend: `ANNEX-1` added at the viewer's size, streamed and removed.
- Found the Thor over the tailnet.

**Jetson AGX Thor** (Ubuntu 26.04, GNOME 50, JetPack 7):

- Viewing a local CLI host: 60 fps, p50 12.5 ms with `avdec_h264`.
- Mirror through Mutter + `nvh264enc` to the Mac: works, picture checked.
- Extend: see the GNOME row in section 4.

Still to test: Pi 4 (V4L2 encoder), DGX Spark, an x86_64 Ubuntu laptop (VA-API, X11, GNOME Extend on a real desktop), KDE.

## 5. Phases

| # | What | Status |
|---|---|---|
| 0 | Prove Extend and the encoders | Pi 5 and Thor done (section 4a); Pi 4, Spark, laptop, KDE to do |
| 1 | Core prep: evdev key column, Linux Tailscale peers, shared media crate, `annex host --extend` | Done |
| 2 | App shell: Nearby, Share, Paired Devices, Settings, About; tray; pairing; packages | Done |
| 3 | View: decode, window, full screen, input, ⌃⌥⇧Q | Done; pointer lock for Extend to do |
| 4 | Share (Mirror): Mutter, portal, screencopy and X11 capture; encoder choice; input | Done |
| 5 | Extend host + release | labwc works; GNOME, KDE, Sway built; X11 and the update notice to do |

## 6. Releases

Every release (`release.yml`) and every CI run (`ci.yml`) builds on Ubuntu 22.04, the oldest supported system, for x86_64 and arm64:

- `Annex-<version>-linux-<arch>.deb` (Ubuntu, Debian, Raspberry Pi OS, JetPack, DGX OS, Mint, Pop!_OS)
- `Annex-<version>-linux-<arch>.rpm` (Fedora, RHEL and its relatives, openSUSE, Amazon, Oracle)
- `Annex-<version>-linux-<arch>.tar.gz` (Arch, NixOS, Photon, anything else)

CI installs the `.deb` and runs `annex --version` before uploading. The files are listed in the signed `SHA256SUMS.txt` like the Mac and Windows files. `scripts/package-linux.sh <version>` builds all three on the computer it runs on.

### Install

```sh
# Ubuntu, Debian, Raspberry Pi OS, JetPack, DGX OS
sudo apt install ./Annex-<version>-linux-arm64.deb     # or -x86_64
# Fedora, RHEL, openSUSE (H.264: openh264 from Fedora's Cisco repo, or RPM Fusion / Packman)
sudo dnf install ./Annex-<version>-linux-x86_64.rpm
# Anything else: unpack and run bin/annex (needs GTK ≥ 4.6 and GStreamer ≥ 1.20 with plugins)
tar xf Annex-<version>-linux-x86_64.tar.gz
```

apt, dnf and zypper also install what Annex needs (GStreamer, wl-clipboard for the clipboard on Wayland, ALSA and pulseaudio-utils for sound: `parec` records this computer's sound and `pactl`/`pacat` make "Annex Microphone"; they work with PipeWire too). The packages carry gtk4-layer-shell (MIT) in `/usr/lib/annex`, for the drop island at the top of the screen on wlroots desktops and KDE; Annex restarts itself once with it preloaded there, as that library requires. On X11 desktops (Linux Mint, XFCE, MATE) the island needs nothing extra and glows as soon as files are picked up; GNOME on Wayland has no island, so files are dropped on the viewer window there.

### Uninstall

```sh
curl -fsSL https://raw.githubusercontent.com/NRanjan-17/Annex/main/scripts/install.sh | sh -s -- --uninstall
```

It removes Annex, its settings, its identity and trusted devices, and the packages that were installed only for Annex; packages you had before stay. `sudo apt remove annex` removes Annex alone.

### Command line

`annex` is also a command-line tool, so a computer reached only over SSH (a Raspberry Pi, a Jetson) can be run from the terminal: every setting, and everything the window does.

- **Starting** keeps the options it always had (`--background`, `--share`, `--headless`, `--connect <address | code> [--extend]`), so autostart entries and scripts work as before. Given to an Annex that's already running, they now apply to it (`annex --share` turns sharing on) and show its window unless `--background`.
- **Commands** (`annex status`, `annex set headless on`, …) go to the Annex that's running: Annex is a GApplication that handles command lines, so a second `annex` hands its arguments to the running one over the session bus; that one runs the command on its main thread and prints the answer, and the exit status, in the first one's terminal. Commands never open a window (a connection opens the video window, as it does from the window). Commands that wait for something (`connect`, `code`, `pin enter`, `update check`) answer once it's done, or after 20 seconds (40 for updates).
- **Without a running Annex**, `settings`, `get`, `set` and `devices` change the files (`~/.config/annex/settings.json`, the paired devices in `~/.local/share/annex`) and Annex uses them when it starts; `start` starts it in the background on the desktop's display, out of the SSH session; the rest say Annex isn't running.
- Over SSH, log in as the user whose desktop runs Annex. The session bus is found at `/run/user/<uid>/bus`; if it isn't, `DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus` points `annex` to it.
- `set` applies a change the way the Settings page does: `open-at-login` writes the autostart entry, `verbose-logging` switches the log, the sound switches go to the core, `name` renames this computer. The relay password and pairing PINs are never taken on the command line: `annex set relay-password` and `annex pin enter` ask for them without showing what's typed (or read a pipe). `annex pin` prints a PIN because you asked; nothing logs PINs, codes or the relay password. The log records each command as `cli: <command>` (`cli: set headless`, never the value of a secret).
- `--json` on `status`, `settings`, `get`, `devices`, `nearby`, `transfers` and `update check`, for scripts. Exit status: 0 done, 1 failed, 2 wrong usage.

A Raspberry Pi with no one at its screen, from SSH:

```sh
annex start --headless --share     # or, if it's running: annex set headless on && annex share on
annex set open-at-login on         # starts by itself from now on
annex set share-at-launch on
annex pin                          # when another computer pairs: the PIN to type there
annex status
annex code                         # a code for connecting over the internet
annex send ~/report.pdf            # to the computer in the session
annex devices allow-control "Mac Studio" on
annex devices allow-sound "Mac Studio" off   # it no longer hears this computer
annex devices allow-mic "Mac Studio" off     # its microphone no longer plays here
```

On GNOME, a locked screen can't be shared: Mutter refuses new screen casts and closes the running one. So while sharing with *No one at this screen* on, Annex keeps the screen from going idle and locking (anyone at the monitor can use the computer meanwhile). If it was locked anyway, `loginctl unlock-session` unlocks it over SSH; the next viewer gets a new screen cast.

`annex help`:

```text
Annex: use another computer as a display, or show its screen.

Usage: annex [--background] [--share] [--headless] [--connect <address | code> [--extend]]
       annex <command> [options]

Starting Annex
  annex                         Open Annex (or show its window if it's already open)
  annex --background            Start in the tray, without the window
  annex --share                 Share this screen as soon as it starts
  annex --headless              Turn on No one at this screen (for a computer reached over SSH)
  annex --connect <to> [--extend]
                                Connect at once to an address or a code
  annex start [--headless] [--share]
                                Start Annex in the background from a terminal, and wait for it
  annex quit                    Quit Annex (ends every session)

Commands (they talk to the Annex that's running; none opens a window)
  status [--json]               Running?, name, sharing, sessions, viewing, updates
  share on|off                  Share this screen, or stop
  connect <to> [--extend|--mirror]
                                View (or extend to) a computer: an address, a code, or a name in Nearby
  disconnect                    End the session this computer is viewing
  code                          Get a code for connecting over the internet (turns sharing on)
  code stop                     Stop the code
  pin                           Print the pairing PIN another computer is waiting for
  pin enter                     Type the PIN another computer shows (read from the terminal)
  nearby [--json]               Computers with Annex open on the network
  devices [--json]              Paired devices
  devices allow-control <device> on|off
                                Let a paired device use this computer's mouse and keyboard
  devices allow-sound <device> on|off
                                Let a paired device hear this computer's sound (on by default)
  devices allow-mic <device> on|off
                                Play a paired device's microphone here (on by default)
  devices forget <device> [--yes]
                                Forget a paired device (it pairs again with a PIN)
  send <file>...                Send files (or folders) to the computer in the session
  transfers [--json]            Files being sent or received, with their numbers
  pause|resume|unsend <number>  Pause, resume or take back a file being sent
  settings [--json]             Every setting, its value and what it takes
  get <setting> [--json]        One setting's value
  set <setting> <value>         Change a setting (applied at once if Annex is running)
  update check [--json]         Look for a newer version
  logs [--path] [--follow] [-n <lines>]
                                Show the end of the log, its path, or follow it
  version                       Annex's version
  help [command]                This help, or more on one command

<device> is a paired device's name or the start of its identity code.
Without a running Annex, settings and paired devices are changed in their files; other commands
say Annex isn't running. Exit status: 0 done, 1 failed, 2 wrong usage.
```

Every setting (`annex settings` shows their values):

| Setting | Takes | What it is |
|---|---|---|
| `name` | text, or `default` | This computer's name, shown to other computers (default: the computer's own name) |
| `open-at-login` | on, off | Start Annex in the tray when you log in |
| `share-at-launch` | on, off | Share this screen when Annex opens |
| `headless` | on, off | No one at this screen: PINs also go to pairing-pin.txt, paired devices control it without asking |
| `quality` | best, balanced, fast | Picture quality when viewing; fast if it stutters on Wi-Fi |
| `control` | on, off | Viewing: control the other computer with this one's mouse and keyboard, when it allows it |
| `open-full-screen` | on, off | Viewing: open the other screen full screen |
| `extend-by-default` | on, off | Connecting uses the other computer as a display instead of showing its screen |
| `hardware-encoder` | on, off | Sharing: the graphics chip's video encoder (next time sharing turns on) |
| `extend-scale` | auto, 100, 125, 150, 175, 200 | Sharing: text size on a computer extending this one; auto matches its screen (next time sharing turns on) |
| `share-sound` | on, off | Sound: people viewing this computer hear what it plays |
| `hear-sound` | on, off | Sound: play the other computer's sound here |
| `send-mic` | on, off | Sound: send this microphone to the computer it views |
| `virtual-mic` | on, off | Sound: a viewer's microphone becomes Annex Microphone here instead of playing aloud |
| `share-clipboard` | on, off | Copy on one computer, paste on the other, while one controls the other |
| `receive-files` | on, off | Take files the other computer sends, into Downloads |
| `drop-island` | on, off | Drag files to the top of the screen to send them while connected |
| `technical-details` | on, off | Show identity codes, routes, decoder and encoder in the window |
| `verbose-logging` | on, off | Log every step (every input event, never which keys) |
| `check-updates` | on, off | Look for updates on GitHub once a day |
| `beta-updates` | on, off | Include beta versions when looking for updates |
| `server` | text, or `default` | The code server (default: Annex's, wss://connect.getannex.dev) |
| `relay` | text, or `default` | Your own TURN relay (turn:host:port), for networks that block direct connections |
| `relay-username` | text, or `default` | Your relay's user name |
| `relay-password` | typed at the prompt (or piped) | Your relay's password; `annex set relay-password` asks for it (empty clears it) |
| `screen-token` | `clear` | The desktop's saved permission to share the screen; clear it to be asked again |
| `last-seen-version` | read-only | The version whose What's New was last shown |
| `beta-updates-chosen` | read-only | Beta updates was set by you (until then it follows the installed version) |
| `verbose-logging-defaulted` | read-only | Internal: verbose logging was turned on once for installs from before it was the default |

## 6a. Building

```sh
# Ubuntu / Debian
sudo apt install build-essential pkg-config libgtk-4-dev libasound2-dev libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev libxkbcommon-dev libdbus-1-dev \
  gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav gstreamer1.0-pipewire
cd apps/linux && cargo build --release      # → target/release/annex
```

armhf (32-bit Raspberry Pi OS) and riscv64 build the same way on the device itself; there are no prebuilt packages for them yet.

## 7. Risks

| Risk | Effect | Plan |
|---|---|---|
| Virtual monitors not available on a desktop | No Extend there | Phase 0 first; a clear message otherwise |
| No hardware H.264 (Fedora, some VMs) | Software encode: higher processor load | Cap at 60 fps; say so in Settings |
| Pointer lock on Wayland | GTK4 has no API for it; Extend's relative pointer needs the pointer-constraints protocol through `gdk-wayland` | Build it in phase 3; fall back to absolute moves |
| Testing | Headless test computers miss things a real desktop shows (GNOME Extend) | A Lima VM (Ubuntu 22.04) for builds; the Thor and the Pi over Tailscale; a logged-in laptop for sign-off |
| Packaging | System GStreamer makes AppImage a poor fit | `.deb`, `.rpm`, `.tar.gz` for x86_64 and arm64; Flatpak later. Updates: a notice with a download link, no self-install (to do) |
| Raspberry Pi 5 | No H.264 hardware: software encode and decode | Lower caps on the Pi 5; later, HEVC viewing (the Pi 5 decodes HEVC in hardware; the core already negotiates it) |
| 32-bit and RISC-V builds | Slow emulated CI builds; few testers | Build from source for now; riscv64 is tier 2 |
