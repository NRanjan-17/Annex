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
        Session::OtherWayland
    }
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
