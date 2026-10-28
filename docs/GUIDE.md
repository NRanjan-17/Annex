# Annex guide

Everything beyond the [README](../README.md): installing by hand, every feature, uninstalling and fixing problems.

## Installing by hand

Download the files from **[Releases](https://github.com/NRanjan-17/Annex/releases/latest)**. After that, Annex keeps itself up to date (on Mac and Windows).

The one-line installers in the README do the same steps for you, and refuse to install anything
that fails a check:

| | Checks before installing |
|---|---|
| `install.sh` on macOS | SHA-256 against the release's `SHA256SUMS.txt`, then Apple's signature: Developer ID from Annex's team, notarized |
| `install.sh` on Linux | `SHA256SUMS.txt`'s Ed25519 signature (the key the in-app updater trusts; needs OpenSSL 3), then SHA-256 |
| `install.ps1` on Windows | The same Ed25519 signature, then SHA-256 |

The scripts themselves come from `main` over HTTPS, so you trust this repository the same way as when
you download a release by hand. To read one before running it, save it first:
`curl -fsSLO https://raw.githubusercontent.com/NRanjan-17/Annex/main/scripts/install.sh`.

### macOS

1. Download **`Annex-<version>-macos-universal.dmg`** and open it.
2. Drag **Annex** onto **Applications**, then open it.
3. Allow **Local Network** when macOS asks, so Annex can find and reach your other computers.

The Mac app is signed and notarized by Apple, so it opens without warnings.

The first time you **share** this Mac, macOS asks for **Screen Recording**: allow it in
System Settings → Privacy & Security → Screen & System Audio Recording, then turn sharing off and
on again. Letting another computer **control** this Mac also needs **Accessibility** (Annex asks
when you turn it on).

### Windows

1. Download **`Annex-Setup-<version>-x64.exe`** (or `-arm64.exe` for ARM PCs) and run it.
2. The installer isn't code-signed yet, so SmartScreen may say *Windows protected your PC*:
   click **More info → Run anyway**.
3. Choose **just for me** or **all users**, and any folder or drive. Optional: *Install the
   Extend-mode display driver* (see [Extend on Windows](#extend-on-windows)).
4. Annex opens and adds an icon to the system tray, next to the clock.

When Windows asks whether to allow Annex on **private networks**, click **Allow**, or other
computers can't find this PC.

### Linux

Download the package for your system and CPU (`x86_64` for PCs, `arm64` for Raspberry Pi 4/5 on
64-bit Raspberry Pi OS, NVIDIA Jetson and DGX Spark):

```sh
sudo apt install ./Annex-<version>-linux-arm64.deb    # Ubuntu, Debian, Raspberry Pi OS, JetPack, DGX OS
sudo dnf install ./Annex-<version>-linux-x86_64.rpm   # Fedora, RHEL, openSUSE
```

Other distributions: unpack the `.tar.gz` and run `bin/annex`. Open **Annex** from the app menu; it
adds a tray icon where the desktop has a tray (KDE, Raspberry Pi OS, Ubuntu's GNOME).

View and Share are tested on GNOME (NVIDIA Jetson) and Raspberry Pi OS's labwc; KDE, Sway and X11
are built but not yet tested. Extend works on labwc; GNOME, KDE and Sway are built and still being
checked, and X11 has none yet. Details:
[docs/LINUX.md](LINUX.md).

## Using it

Preferences are in **Settings** (General, Sharing, Viewing and Developer; on a Mac, press **⌘,**).
**Paired devices** and **About**, with updates, have their own pages at the bottom of the sidebar.

### Extend display

Choose **Extend display** (or turn on *Extend display by default* in
**Settings → Viewing**). The sharing computer gets a new display called **Annex Display**:

- **Mac:** arrange it in System Settings → Displays, and pick its scaling there, from *Larger Text*
  to *More Space* (the same steps as Windows' 100–200 %), and its refresh rate: the same rates
  the other computer's screen supports, so a 144 Hz laptop gets 144 Hz. It starts at the sharpest
  scaling (100 %, or 200 % on a high-resolution screen) and the other computer's refresh rate. To
  change the scaling from Annex, use **Settings → Sharing → Extended display scaling**: it applies
  at once and every time that computer extends this Mac. The steps between 100 % and 200 % give
  bigger text but look a little soft.
- **Windows:** arrange it, and pick its scale and refresh rate, in Settings → System → Display.
  Needs the Extend-mode driver (below).

Neither computer's screen goes to sleep while a session runs.

### Control the other computer

Off by default, and both sides have to agree:

1. On the **sharing** computer: **Paired devices**, turn on control for the device
   you'll use it from. On a Mac, allow **Accessibility** when asked.
2. On the computer you're **sitting at**: **Settings → Viewing → Control the other computer**.

Windows doesn't let any app type into administrator windows or UAC prompts, so those ignore
remote input.

### Paired devices

Every device you pair with a PIN is listed under **Paired devices**. Remove one to make it
enter a PIN again next time. Rename this computer in **Settings → General → Device name**.

### Smoother on Wi-Fi

- **Quality** (Settings → Viewing): *Fast (Wi-Fi)* keeps the picture moving on a busy network;
  *Best* suits a cable or a fast network. Annex also lowers quality by itself as soon as the
  network starts to lag.
- **Direct Wi-Fi** (Mac ↔ Windows, or between PCs): the Windows PC runs its own Mobile hotspot, as
  set up in Windows Settings, and hands its name and password over inside the encrypted session; the other computer joins it and Annex
  reconnects. Video then goes straight between the two, without the router in the middle.
  - **Before you start:** turn on *Connect over direct Wi-Fi* (Windows: Settings → Viewing; Mac:
    the computer's page under Nearby).
  - **During a session:** *Switch to direct Wi-Fi* on the viewing computer, or next to the viewer
    on the sharing computer.
  - **On its own:** *Share this PC → Direct Wi-Fi* runs the hotspot and shows its name and password.

  Needs a PC whose Wi-Fi can run Mobile hotspot.
- On Windows, Annex turns on Wi-Fi's *media streaming mode* (no background scans, no power saving)
  while a session runs.

### A sharper picture: HEVC

When both computers support it, Annex sends the picture as HEVC instead of H.264: sharper at
the same speed. It needs nothing from you; **Settings → HEVC** turns it off.

| | Sending HEVC | Receiving HEVC |
|---|---|---|
| Mac | Apple silicon, and Intel Macs with an HEVC encoder | Macs that decode HEVC in hardware |
| Windows | A graphics card with an HEVC encoder (NVIDIA, Intel, AMD) | Windows' HEVC decoder: the *HEVC Video Extensions*, built into many PCs, otherwise from the Microsoft Store |
| Linux | An HEVC encoder on the graphics chip (Intel and AMD through VA-API, NVIDIA, Jetson) | A GStreamer HEVC decoder (`gstreamer1.0-libav` has one) |

If either side lacks it, that session uses H.264. When several computers watch one screen, it's
HEVC only while all of them support it, and switches by itself as they come and go.

### Fastest connection: a cable

Annex runs over any cable that makes a **network link** between the two computers, and prefers it
automatically when one is plugged in (shown as *Cable*):

| Computers | Cable |
|---|---|
| Mac ↔ Mac | Thunderbolt or USB4 cable (macOS sets up *Thunderbolt Bridge* by itself) |
| Mac ↔ Windows, Windows ↔ Windows | An Ethernet cable, with a USB-C or USB Ethernet adapter on any computer without a port. Thunderbolt/USB4 networking also works between PCs that support it. |

A plain USB charging or data cable between two computers is **not** a network link, so it doesn't
work. Details: [docs/CABLES.md](CABLES.md).

### A computer somewhere else

**With a code (no setup):**

1. On the computer to share, under **Over the internet**, click **Get a Code**. The code is 6
   characters, works once, and lasts 10 minutes.
2. On the other computer, choose **Connect by Code or Address…** and type it.

Annex connects the two computers **directly** whenever their networks allow it. Some don't: an
internet provider that shares one address among many homes, or a college or office network that
blocks it. Only then does the session go through **our relay**. It stays end to end encrypted (the
relay only forwards packets it can't read) and runs at the same quality as a direct connection. Each
sharing computer can use it for **10 sessions a day** (reset at midnight UTC), each up to 24 hours.

Direct connections, and your own TURN relay (**Settings → Internet**), have no limits.

**With a private network:** on Tailscale or ZeroTier, install it on both computers, then
*Connect by address* with the other computer's VPN address. It's direct far more often and has no
relay limits. Annex also lists Tailscale computers under Nearby. Details:
[docs/REMOTE.md](REMOTE.md).

### Your own code server and relay

Annex's code server (`connect.getannex.dev`) only swaps the two computers' addresses, and its
relay is only used when a direct connection is blocked. You can use your own instead, in
**Settings → Internet**:

| Setting | What to enter |
|---|---|
| **Server** | Your code server, e.g. `wss://rendezvous.example.com`. Leave it empty for Annex's. **Both computers must use the same server**, or a code from one won't be found by the other. |
| **Your relay** | A TURN server, e.g. `turn:relay.example.com:3478` (UDP). Then fill in **Relay username** and **Relay password** (kept in the Keychain on a Mac, encrypted for your account on Windows). Leave it empty to use Annex's relay. |

Your own relay has no daily limit, and the relay only needs to be set on the computer that
shares its screen. On Linux the same settings work from a terminal too:

```sh
annex set server wss://rendezvous.example.com
annex set relay turn:relay.example.com:3478
annex set relay-username me
annex set relay-password          # asks for it
annex set server default          # back to Annex's
```

**Running your own:**

- **Code server:** it's a small Cloudflare Worker in this repository; the free plan is enough.
  Deploy it with `npx wrangler deploy` from `server/rendezvous`, then enter its address with
  `wss://` in front. Step by step: [server/rendezvous/README.md](../server/rendezvous/README.md).
- **Relay:** any standard TURN server that speaks UDP, such as [coturn](https://github.com/coturn/coturn)
  on a small cloud server, or a hosted TURN service. Open its UDP port (3478) and the relay
  port range in the server's firewall. The session stays end to end encrypted through it: the
  relay only forwards packets it can't read.

### A computer with no one at its screen

A Raspberry Pi, a Jetson or a Mac mini you reach only over SSH can share its screen too. Annex
shares a **desktop**, so the computer must log in to its desktop by itself (auto-login); without a
logged-in desktop there's nothing to share. Raspberry Pi OS does this out of the box, even with no
monitor attached.

1. Turn on **No one at this screen** in Settings, or over SSH:

   | | Command |
   |---|---|
   | Linux | `annex set headless on`, or `annex start --headless --share` (the setting stays on) |
   | Mac | `defaults write com.nranjan-17.annex headless -bool true` |
   | Windows | `"Headless": true` in `%LOCALAPPDATA%\Annex\settings.json` |

   On a Mac or Windows, change it while Annex is closed (quit it first), or it won't notice. On
   Linux, `annex set` works whether Annex is running or not; `annex help` lists every command
   ([LINUX.md](LINUX.md#command-line)).

2. Turn on **Open at login** and **Start sharing when Annex opens**, so it starts by itself.
3. The first time another computer connects, read the PIN over SSH and type it there:

   | | PIN file |
   |---|---|
   | Linux | `annex pin` (or `cat ~/.local/share/annex/pairing-pin.txt`) |
   | Mac | `cat ~/Library/Application\ Support/Annex/pairing-pin.txt` |
   | Windows | `type %LOCALAPPDATA%\Annex\pairing-pin.txt` |

The file exists only while pairing (2 minutes) and only your user can read it. Paired computers
can then use the mouse and keyboard without anyone clicking **Allow**. On a Mac, grant **Screen
Recording** once in person or over Screen Sharing; macOS doesn't allow that over SSH.

### Extend on Windows

Windows has no built-in way for an app to add a display, so when a **Windows PC is sharing**,
Extend uses Annex's own display driver. Tick *Install the Extend-mode display driver* in the
installer (an all-users install). The installer signs the driver **on your PC** with a certificate
made just for it and deletes that certificate's private key straight away, so Windows loads the
driver without test signing and nothing else can ever be signed with it. Uninstalling removes the
driver and the certificate.

**Share this PC** shows whether Extend display is ready. Mirroring, and extending a **Mac**'s display
onto a Windows PC, don't need the driver.

### Updates

Annex checks GitHub at launch and once a day and installs new versions for you (**About →
Updates**). It always installs the newest version, skipping any in between, and looks again when
you click **Install** in case a newer one came out since the check. Turn on **Beta updates** to get
pre-releases early; turn it off to go back to the latest normal release. Every update is checked against a signature made when the release is built,
so a modified download is never installed.

On Linux, Annex doesn't update itself yet: download the new package from the releases page and
install it the same way (`sudo apt install ./Annex-<version>-linux-<arch>.deb`).

## Uninstall

- **macOS:** **About → Uninstall…**. It removes the app, its identity key (Keychain),
  paired devices, settings and permissions.
- **Windows:** Settings → Apps → Annex → Uninstall. It removes the app, the display driver, the
  firewall rule, *Start with Windows*, and all settings, keys and paired devices.
- **Linux:** `sudo apt remove annex` (or `sudo dnf remove annex`). Settings, keys and paired devices
  stay in `~/.config/annex` and `~/.local/share/annex`; delete those folders to remove them too.

## Troubleshooting

| Problem | Try this |
|---|---|
| The other computer isn't under **Nearby** | Same network (not guest Wi-Fi or a VPN)? Click **Refresh** next to Nearby. On Windows: **Connect → Not showing up? → Allow** (firewall) and set the network to *Private*. On a Mac: allow Annex in **Privacy & Security → Local Network**. Or use **Connect by address**. |
| It's listed but greyed out | It isn't sharing: click **Start Sharing** on that computer. |
| "Extend display" is greyed out | A sharing Windows PC needs the Extend-mode display driver (installer option). **Share this PC** on that PC says what's missing. |
| Black screen when mirroring a Mac | Allow Screen Recording for Annex (System Settings → Privacy & Security), then turn sharing off and on. |
| Mouse and keyboard do nothing on the other computer | It must allow your device under **Paired devices** (and on a Mac, have Accessibility). Turn on **Settings → Viewing → Control the other computer** on your side. |
| The PIN keeps failing | After 5 wrong PINs, pairing pauses for 10 minutes. Type the PIN shown on the *sharing* computer. |
| Laggy picture | Use a cable or Ethernet instead of Wi-Fi, or try *Direct Wi-Fi*. Keep *Hardware video encoder* on (Settings → Sharing). |

**Reporting a bug?** Annex always keeps a log, `annex.log` (**Settings → Developer → Log file**
opens it). It stays on your computer; Annex never sends it anywhere. Attach it to
[an issue](https://github.com/NRanjan-17/Annex/issues) so we can see what went wrong.
**Show technical details** in the same place reveals identity codes, addresses and connection
figures.
