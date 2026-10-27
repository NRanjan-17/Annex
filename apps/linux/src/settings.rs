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
    /// Nobody at this screen (reached over SSH): pairing PINs also go to `pairing-pin.txt` in the
    /// data folder, and paired devices may control it without asking. `annex --headless` turns it on.
    pub headless: bool,
    /// 0 Best, 1 Balanced, 2 Fast (the core's `Quality`).
    pub quality: u32,
    /// Viewing: control the other computer with this one's mouse and keyboard.
    pub use_mouse_and_keyboard: bool,
    pub open_full_screen: bool,
    pub extend_by_default: bool,
    /// Copy on one computer, paste on the other, while one controls the other.
    pub share_clipboard: bool,
    /// Take files the other computer sends, into Downloads.
    pub receive_files: bool,
    /// Drag files to the top of the screen to send them (wlroots desktops and KDE).
    pub drop_at_top: bool,
    /// Sharing: use the graphics chip's video encoder when there is one.
    pub prefer_hardware_encoder: bool,
    /// Sharing: the size of text and windows on a screen extending this one. 0 = the other
    /// screen's own scale (200 % on a Retina Mac); otherwise a percentage, 100–200.
    pub extend_scale: u32,
    /// Sound: send what this computer plays to whoever views it.
    pub share_sound: bool,
    /// Sound: play what the other computer sends (its sound, or a viewer's microphone).
    pub hear_sound: bool,
    /// Sound: send this computer's microphone to the computer it views.
    pub send_mic: bool,
    /// Sound: a viewer's microphone becomes "Annex Microphone" here instead of playing aloud.
    pub virtual_mic: bool,
    pub show_technical_details: bool,
    /// Everything Annex does goes in the log (every input event, every progress step).
    pub verbose_logging: bool,
    pub check_for_updates: bool,
    /// Include pre-releases; off goes back to the latest normal release. Until the user flips it
    /// (`beta_updates_chosen`), it follows what's installed: someone who installed a beta by hand
    /// gets betas, not an offer to go back.
    pub beta_updates: bool,
    /// The user has flipped Beta updates. (Older settings files always hold `betaUpdates`, so its
    /// presence can't say this.)
    pub beta_updates_chosen: bool,
    /// Empty: Annex's server.
    pub rendezvous_server: String,
    pub relay_url: String,
    pub relay_username: String,
    pub relay_password: String,
    /// The desktop's screen-sharing permission (portal restore token), so it asks only once.
    pub screen_token: Option<String>,
    /// The release whose "What's new" was last shown (or skipped on a first install).
    pub last_seen_version: Option<String>,
    /// Verbose logging was switched on once for installs from before it was the default.
    pub verbose_logging_defaulted: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            device_name: None,
            open_at_login: false,
            start_sharing_at_launch: false,
            headless: false,
            quality: 0,
            use_mouse_and_keyboard: true,
            open_full_screen: true,
            extend_by_default: false,
            share_clipboard: true,
            receive_files: true,
            drop_at_top: true,
            prefer_hardware_encoder: true,
            extend_scale: 0,
            share_sound: true,
            hear_sound: true,
            send_mic: false,
            virtual_mic: true,
            show_technical_details: false,
            verbose_logging: true,
            check_for_updates: true,
            beta_updates: running_beta(),
            beta_updates_chosen: false,
            rendezvous_server: String::new(),
            relay_url: String::new(),
            relay_username: String::new(),
            relay_password: String::new(),
            screen_token: None,
            last_seen_version: None,
            verbose_logging_defaulted: false,
        }
    }
}

impl Settings {
    pub fn load() -> Settings {
        let Ok(bytes) = std::fs::read(path()) else {
            tracing::info!("settings: none saved yet, using the defaults");
            return Settings::default();
        };
        let mut s: Settings = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!("settings: {} couldn't be read ({e}); using the defaults", path().display());
            Settings::default()
        });
        s.after_load(running_beta());
        s
    }

    /// Fixes up what was read: Beta updates follows the installed build until the user chooses,
    /// and verbose logging is switched on once for installs from before it was the default.
    fn after_load(&mut self, running_beta: bool) {
        if !self.beta_updates_chosen {
            self.beta_updates = running_beta;
        }
        if !self.verbose_logging_defaulted {
            self.verbose_logging = true;
            self.verbose_logging_defaulted = true;
        }
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = p.with_extension("json.tmp");
        // The relay password is in here: readable by this user only.
        let saved = serde_json::to_vec_pretty(self)
            .map_err(|e| e.to_string())
            .and_then(|json| write_private(&tmp, &json).map_err(|e| e.to_string()))
            .and_then(|()| std::fs::rename(&tmp, &p).map_err(|e| e.to_string()));
        if let Err(e) = saved {
            tracing::warn!("couldn't save the settings to {}: {e}", p.display());
        }
    }

    /// The choices, for the log: no relay login, no screen-sharing token.
    pub fn describe(&self) -> String {
        format!(
            "open_at_login={} start_sharing_at_launch={} headless={} quality={} use_mouse_and_keyboard={} \
             open_full_screen={} extend_by_default={} share_clipboard={} receive_files={} drop_at_top={} \
             prefer_hardware_encoder={} share_sound={} hear_sound={} send_mic={} virtual_mic={} show_technical_details={} verbose_logging={} own_server={} own_relay={} \
             screen_token={} last_seen_version={} check_for_updates={} beta_updates={} beta_updates_chosen={}",
            self.open_at_login,
            self.start_sharing_at_launch,
            self.headless,
            self.quality,
            self.use_mouse_and_keyboard,
            self.open_full_screen,
            self.extend_by_default,
            self.share_clipboard,
            self.receive_files,
            self.drop_at_top,
            self.prefer_hardware_encoder,
            self.share_sound,
            self.hear_sound,
            self.send_mic,
            self.virtual_mic,
            self.show_technical_details,
            self.verbose_logging,
            !self.rendezvous_server.trim().is_empty(),
            !self.relay_url.trim().is_empty(),
            self.screen_token.is_some(),
            self.last_seen_version.as_deref().unwrap_or("none"),
            self.check_for_updates,
            self.beta_updates,
            self.beta_updates_chosen,
        )
    }

    /// The sound switches, for the core.
    pub fn audio(&self) -> annex_session::audio::AudioSettings {
        annex_session::audio::AudioSettings {
            share_sound: self.share_sound,
            hear_sound: self.hear_sound,
            send_mic: self.send_mic,
            virtual_mic: self.virtual_mic,
        }
    }

    /// The code server to use: the user's, or Annex's.
    pub fn server(&self) -> &str {
        let s = self.rendezvous_server.trim();
        if s.is_empty() { ANNEX_SERVER } else { s }
    }
}

/// This build is a pre-release ("0.9.0-beta.1").
pub fn running_beta() -> bool {
    annex_update::whats_new::CURRENT.contains('-')
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

/// Annex ran here before: its settings, log or identity are already there. Ask before the log
/// starts and the identity is made. Versions before 0.4.0 didn't save `last_seen_version`, so
/// this tells an update from a first install.
pub fn used_before() -> bool {
    let data = data_dir();
    path().exists() || ["annex.log", "identity.cert.der", "trusted_peers.json"].iter().any(|f| data.join(f).exists())
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
    tracing::info!("settings: open at login on ({})", file.display());
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
        assert!(s.verbose_logging);
        assert!(s.last_seen_version.is_none());
        assert!(s.share_sound && s.hear_sound && !s.send_mic && s.virtual_mic);
        assert!(s.open_full_screen);
        assert!(!s.beta_updates_chosen);
    }

    #[test]
    fn beta_updates_follow_the_build_until_chosen() {
        // An older file that never chose: follows what's installed, either way.
        for running in [true, false] {
            let mut s: Settings = serde_json::from_str(r#"{"betaUpdates":false}"#).unwrap();
            s.after_load(running);
            assert_eq!(s.beta_updates, running);
            let mut s: Settings = serde_json::from_str(r#"{"betaUpdates":true}"#).unwrap();
            s.after_load(running);
            assert_eq!(s.beta_updates, running);
        }
        // Chosen: kept, whatever is installed.
        let mut s: Settings = serde_json::from_str(r#"{"betaUpdates":false,"betaUpdatesChosen":true}"#).unwrap();
        s.after_load(true);
        assert!(!s.beta_updates);
        let mut s: Settings = serde_json::from_str(r#"{"betaUpdates":true,"betaUpdatesChosen":true}"#).unwrap();
        s.after_load(false);
        assert!(s.beta_updates);
    }

    #[test]
    fn description_has_no_secrets() {
        let s = Settings {
            relay_username: "alice".into(),
            relay_password: "hunter2".into(),
            screen_token: Some("token-123".into()),
            ..Settings::default()
        };
        let d = s.describe();
        assert!(!d.contains("alice") && !d.contains("hunter2") && !d.contains("token-123"));
    }
}
