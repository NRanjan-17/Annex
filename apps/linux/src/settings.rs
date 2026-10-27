//! Settings, saved as JSON in `$XDG_CONFIG_HOME/annex/settings.json`. The same choices as the Mac
//! and Windows apps; missing keys take their defaults, so older files keep working.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Annex's own relay and code server (docs/INTERNET-CONNECT.md).
pub const ANNEX_SERVER: &str = "wss://connect.getannex.dev";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// None: the computer's own name (its hostname).
    pub device_name: Option<String>,
    pub open_at_login: bool,
    pub start_sharing_at_launch: bool,
    /// 0 Best, 1 Balanced, 2 Fast (the core's `Quality`).
    pub quality: u32,
    /// Viewing: control the other computer with this one's mouse and keyboard.
    pub use_mouse_and_keyboard: bool,
    pub open_full_screen: bool,
    pub extend_by_default: bool,
    /// Sharing: use the graphics chip's video encoder when there is one.
    pub prefer_hardware_encoder: bool,
    pub show_technical_details: bool,
    pub save_log: bool,
    pub check_for_updates: bool,
    pub beta_updates: bool,
    /// Empty: Annex's server.
    pub rendezvous_server: String,
    pub relay_url: String,
    pub relay_username: String,
    pub relay_password: String,
    /// The desktop's screen-sharing permission (portal restore token), so it asks only once.
    pub screen_token: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            device_name: None,
            open_at_login: false,
            start_sharing_at_launch: false,
            quality: 0,
            use_mouse_and_keyboard: true,
            open_full_screen: false,
            extend_by_default: false,
            prefer_hardware_encoder: true,
            show_technical_details: false,
            save_log: false,
            check_for_updates: true,
            beta_updates: false,
            rendezvous_server: String::new(),
            relay_url: String::new(),
            relay_username: String::new(),
            relay_password: String::new(),
            screen_token: None,
        }
    }
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = p.with_extension("json.tmp");
        // The relay password is in here: readable by this user only.
        if let Ok(json) = serde_json::to_vec_pretty(self)
            && write_private(&tmp, &json).is_ok()
        {
            let _ = std::fs::rename(&tmp, &p);
        }
    }

    /// The code server to use: the user's, or Annex's.
    pub fn server(&self) -> &str {
        let s = self.rendezvous_server.trim();
        if s.is_empty() { ANNEX_SERVER } else { s }
    }
}

fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(data)
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| ".".into())
}

/// `$XDG_CONFIG_HOME/annex` (default `~/.config/annex`).
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("annex")
}

/// `$XDG_DATA_HOME/annex` (default `~/.local/share/annex`): identity key, trusted devices, log.
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("annex")
}

fn path() -> PathBuf {
    config_dir().join("settings.json")
}

/// Open at login: an XDG autostart entry (`~/.config/autostart/annex.desktop`), which every
/// desktop in docs/LINUX.md honours. `--background` starts in the tray without a window.
pub fn set_open_at_login(on: bool) -> std::io::Result<()> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("autostart");
    let file = dir.join("annex.desktop");
    if !on {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(&dir)?;
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "annex".into());
    std::fs::write(
        &file,
        format!(
            "[Desktop Entry]\nType=Application\nName=Annex\nExec={exe} --background\nIcon=annex\n\
             X-GNOME-Autostart-enabled=true\nNoDisplay=true\n"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_take_defaults() {
        let s: Settings = serde_json::from_str(r#"{"quality":2,"unknownKey":true}"#).unwrap();
        assert_eq!(s.quality, 2);
        assert!(s.use_mouse_and_keyboard);
        assert_eq!(s.server(), ANNEX_SERVER);
    }
}
