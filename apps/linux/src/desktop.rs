//! Which desktop Annex runs on decides how it captures the screen, controls the mouse and
//! keyboard, and adds a display (docs/LINUX.md, "How each part works").

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Session {
    /// GNOME on Wayland (Ubuntu, Fedora, Debian): portals, and Mutter for Extend.
    Gnome,
    /// KDE Plasma on Wayland: portals.
    Kde,
    /// labwc, sway, Wayfire and other wlroots compositors (Raspberry Pi OS): wlr protocols.
    Wlroots(Wlr),
    /// Another Wayland desktop: portals if it has them.
    OtherWayland,
    /// X11 (Linux Mint, older JetPack, XFCE, MATE): XShm and XTest.
    X11,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wlr {
    Labwc,
    Sway,
    Other,
}

pub fn session() -> Session {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t.eq_ignore_ascii_case("wayland"));
    if !wayland {
        return Session::X11;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
    let has = |name: &str| desktop.split(':').any(|d| d == name);
    if has("gnome") || has("ubuntu") || has("unity") {
        Session::Gnome
    } else if has("kde") {
        Session::Kde
    } else if has("labwc") || std::env::var_os("LABWC_PID").is_some() {
        Session::Wlroots(Wlr::Labwc)
    } else if has("sway") || std::env::var_os("SWAYSOCK").is_some() {
        Session::Wlroots(Wlr::Sway)
    } else if has("wayfire") || has("hyprland") || has("river") || has("wlroots") {
        Session::Wlroots(Wlr::Other)
    } else {
        // Started outside the desktop's environment (a terminal over SSH, some autostarts):
        // the compositor this user runs says which desktop it is.
        match running_compositor().as_deref() {
            Some("gnome-shell") => Session::Gnome,
            Some("kwin_wayland") => Session::Kde,
            Some("labwc") => Session::Wlroots(Wlr::Labwc),
            Some("sway") => Session::Wlroots(Wlr::Sway),
            Some(_) => Session::Wlroots(Wlr::Other),
            None => Session::OtherWayland,
        }
    }
}

/// The first Wayland compositor found among this user's processes.
pub fn running_compositor() -> Option<String> {
    const KNOWN: [&str; 7] = ["gnome-shell", "kwin_wayland", "labwc", "sway", "wayfire", "Hyprland", "river"];
    KNOWN.iter().find(|k| process(k).is_some()).map(|k| k.to_string())
}

/// The id of this user's process called `name`.
pub fn process(name: &str) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    let me = std::fs::metadata("/proc/self").ok()?.uid();
    std::fs::read_dir("/proc").ok()?.flatten().find_map(|e| {
        let pid: u32 = e.file_name().to_str()?.parse().ok()?;
        if e.metadata().ok()?.uid() != me {
            return None;
        }
        let comm = std::fs::read_to_string(e.path().join("comm")).ok()?;
        (comm.trim() == name).then_some(pid)
    })
}

impl Session {
    pub fn describe(self) -> &'static str {
        match self {
            Session::Gnome => "GNOME (Wayland)",
            Session::Kde => "KDE Plasma (Wayland)",
            Session::Wlroots(Wlr::Labwc) => "labwc (Wayland)",
            Session::Wlroots(Wlr::Sway) => "sway (Wayland)",
            Session::Wlroots(Wlr::Other) => "wlroots (Wayland)",
            Session::OtherWayland => "Wayland",
            Session::X11 => "X11",
        }
    }
}
