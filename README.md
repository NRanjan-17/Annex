<p align="center">
  <img src="assets/icon-512.png" width="128" alt="Annex icon">
</p>

<h1 align="center">Annex</h1>

<p align="center">
  Use another computer as a second display, or mirror its screen.<br>
  Mac, Windows and Linux, in any combination. Over your network or a cable, encrypted end to end.<br>
  Open source (MIT). No accounts, no telemetry.
</p>

<p align="center">
  <a href="https://github.com/NRanjan-17/Annex/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/NRanjan-17/Annex?label=release"></a>
  <a href="https://github.com/NRanjan-17/Annex/releases"><img alt="Downloads" src="https://img.shields.io/github/downloads/NRanjan-17/Annex/total?label=downloads"></a>
  <a href="https://github.com/NRanjan-17/Annex/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/NRanjan-17/Annex/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/github/license/NRanjan-17/Annex"></a>
</p>

---

| | |
|---|---|
| **Mirror** | See and use another computer's screen on this one. |
| **Extend display** | Make this computer a **second screen** for the other one: drag windows onto it like a real monitor. |
| **Control** *(optional)* | Use this computer's mouse and keyboard on the other one. Off by default, allowed per device. |

## Install

One command; it downloads the latest release and checks it is genuine before installing.

**macOS** (14 Sonoma or later) and **Linux** (Ubuntu 22.04+, Debian 12+, Raspberry Pi OS, Fedora, JetPack, x86_64 or arm64), in Terminal:

```sh
curl -fsSL https://raw.githubusercontent.com/NRanjan-17/Annex/main/scripts/install.sh | sh
```

**Windows** (10 version 2004 or 11, x64 or ARM), in PowerShell:

```powershell
irm https://raw.githubusercontent.com/NRanjan-17/Annex/main/scripts/install.ps1 | iex
```

Or download it yourself from **[Releases](https://github.com/NRanjan-17/Annex/releases/latest)**:

| System | File |
|---|---|
| macOS | `Annex-<version>-macos-universal.dmg`: open it and drag Annex to Applications |
| Windows | `Annex-Setup-<version>-x64.exe` (`-arm64.exe` for ARM PCs) |
| Ubuntu, Debian, Raspberry Pi OS, JetPack | `Annex-<version>-linux-<arch>.deb`: `sudo apt install ./Annex-*.deb` |
| Fedora, RHEL, openSUSE | `Annex-<version>-linux-<arch>.rpm`: `sudo dnf install ./Annex-*.rpm` |
| Other Linux | `Annex-<version>-linux-<arch>.tar.gz`: unpack and run `bin/annex` |

Linux packages start with the release after 0.2.0. Mac and Windows keep themselves up to date.
Step-by-step notes, permissions and what each check does: [docs/GUIDE.md](docs/GUIDE.md#installing-by-hand).

## Quick start

1. Open Annex on both computers, on the **same network** (or joined by a cable). Each shows the
   other under **Nearby** within a few seconds.
2. On the computer whose screen you want to use: **Share** → **Start Sharing**.
3. On the other: pick it under **Nearby**, then **Show its screen here** or **Extend display**.
4. The first time only, type the **6-digit PIN** the sharing computer shows.

To leave, press **Esc**, or **⌃⌥⇧Q** (Mac) / **Ctrl+Alt+Shift+Q** (Windows, Linux) while
controlling the other computer. Not listed under Nearby? Use **Connect by address**.

## Downloads

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/NRanjan-17/Annex/stats/downloads-dark.svg">
  <img alt="Downloads per version, by system" src="https://raw.githubusercontent.com/NRanjan-17/Annex/stats/downloads-light.svg">
</picture>

## More

- [Guide](docs/GUIDE.md): every feature, cables and Wi-Fi, a computer with no one at its screen,
  uninstalling, troubleshooting.
- [Security](docs/SECURITY.md): TLS 1.3 with mutual authentication, PIN pairing, signed updates.
  Everything stays on your network; the only internet traffic of its own is the update check.
- [Linux](docs/LINUX.md) · [Building from source](docs/BUILDING.md) · [Protocol](docs/PROTOCOL.md) ·
  [Roadmap](docs/PLAN.md)

Found a bug? Annex always keeps a log, `annex.log` (**Settings → Developer → Log file**). It
stays on your computer; Annex never sends it anywhere. Attach it to
[an issue](https://github.com/NRanjan-17/Annex/issues).

Made by Nalinish Ranjan and [Saidev Dhal](https://saidev.codes). MIT licensed: see [LICENSE](LICENSE).
