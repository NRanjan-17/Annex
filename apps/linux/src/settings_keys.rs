//! Every setting by its command-line name (`annex get/set <key>`): how it reads and changes
//! `Settings`, and what else changes with it ([`Effect`], applied by `Model::after_setting`, the
//! same as the Settings page does).

use crate::cli::{on_off, parse_bool};
use crate::settings::{ANNEX_SERVER, Settings};

/// What a key takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Switch,
    Choice(&'static [&'static str]),
    Text,
    /// Never shown, never taken on the command line (read from stdin).
    Secret,
    /// Only `clear`.
    Clear,
    ReadOnly,
}

/// What else changes with a setting, besides saving it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    /// The computer's name: the core and the network records too.
    Rename,
    /// The autostart entry.
    OpenAtLogin,
    Headless,
    /// The core's sound switches.
    Audio,
    Clipboard,
    ReceiveFiles,
    DropIsland,
    /// Only what the window shows.
    Render,
    VerboseLog,
    CheckUpdates,
    BetaUpdates,
}

type Setter = fn(&mut Settings, &str) -> Result<(), String>;

pub struct Key {
    pub name: &'static str,
    pub kind: Kind,
    pub effect: Effect,
    pub about: &'static str,
    get: fn(&Settings) -> String,
    set: Option<Setter>,
}

impl Key {
    pub fn get(&self, s: &Settings) -> String {
        (self.get)(s)
    }

    pub fn settable(&self) -> bool {
        self.set.is_some()
    }

    pub fn secret(&self) -> bool {
        self.kind == Kind::Secret
    }

    /// Changes `s`; nothing else (see [`Effect`]).
    pub fn set(&self, s: &mut Settings, value: &str) -> Result<(), String> {
        match self.set {
            Some(f) => f(s, value),
            None => Err(format!("{} can't be changed; Annex keeps it up to date itself.", self.name)),
        }
    }

    /// What it takes, for `annex settings`.
    pub fn allowed(&self) -> String {
        match self.kind {
            Kind::Switch => "on|off".into(),
            Kind::Choice(c) => c.join("|"),
            Kind::Text => "text, or default".into(),
            Kind::Secret => "read from the terminal".into(),
            Kind::Clear => "clear".into(),
            Kind::ReadOnly => "read-only".into(),
        }
    }
}

fn switch(v: &str) -> Result<bool, String> {
    parse_bool(v).ok_or_else(|| format!("Use on or off, not {v}."))
}

/// Empty, `default` or `none` clears a text setting.
fn text(v: &str) -> String {
    let t = v.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("default") || t.eq_ignore_ascii_case("none") {
        String::new()
    } else {
        t.to_string()
    }
}

const QUALITY: &[&str] = &["best", "balanced", "fast"];
const SCALES: &[&str] = &["auto", "100", "125", "150", "175", "200"];

macro_rules! switch_key {
    ($name:literal, $field:ident, $effect:expr, $about:literal) => {
        Key {
            name: $name,
            kind: Kind::Switch,
            effect: $effect,
            about: $about,
            get: |s| on_off(s.$field).into(),
            set: Some(|s, v| {
                s.$field = switch(v)?;
                Ok(())
            }),
        }
    };
}

pub const KEYS: &[Key] = &[
    Key {
        name: "name",
        kind: Kind::Text,
        effect: Effect::Rename,
        about: "This computer's name, shown to other computers (default: the computer's own name)",
        get: |s| s.device_name.clone().unwrap_or_default(),
        set: Some(|s, v| {
            let t = text(v);
            s.device_name = (!t.is_empty()).then_some(t);
            Ok(())
        }),
    },
    switch_key!("open-at-login", open_at_login, Effect::OpenAtLogin, "Start Annex in the tray when you log in"),
    switch_key!("share-at-launch", start_sharing_at_launch, Effect::None, "Share this screen when Annex opens"),
    switch_key!(
        "headless",
        headless,
        Effect::Headless,
        "No one at this screen: PINs also go to pairing-pin.txt, paired devices control it without asking"
    ),
    Key {
        name: "quality",
        kind: Kind::Choice(QUALITY),
        effect: Effect::None,
        about: "Picture quality when viewing; fast if it stutters on Wi-Fi",
        get: |s| QUALITY[(s.quality as usize).min(2)].into(),
        set: Some(|s, v| {
            let v = v.trim().to_ascii_lowercase();
            s.quality = match v.as_str() {
                "best" | "0" => 0,
                "balanced" | "1" => 1,
                "fast" | "2" => 2,
                _ => return Err(format!("Use best, balanced or fast, not {v}.")),
            };
            Ok(())
        }),
    },
    switch_key!(
        "control",
        use_mouse_and_keyboard,
        Effect::None,
        "Viewing: control the other computer with this one's mouse and keyboard, when it allows it"
    ),
    switch_key!("open-full-screen", open_full_screen, Effect::None, "Viewing: open the other screen full screen"),
    switch_key!(
        "extend-by-default",
        extend_by_default,
        Effect::Render,
        "Connecting uses the other computer as a display instead of showing its screen"
    ),
    switch_key!(
        "hardware-encoder",
        prefer_hardware_encoder,
        Effect::Render,
        "Sharing: the graphics chip's video encoder (next time sharing turns on)"
    ),
    Key {
        name: "extend-scale",
        kind: Kind::Choice(SCALES),
        effect: Effect::None,
        about: "Sharing: text size on a computer extending this one; auto matches its screen (next time sharing turns on)",
        get: |s| if s.extend_scale == 0 { "auto".into() } else { s.extend_scale.to_string() },
        set: Some(|s, v| {
            let v = v.trim().trim_end_matches('%').trim().to_ascii_lowercase();
            s.extend_scale = match v.as_str() {
                "auto" | "0" | "match" => 0,
                n if SCALES.contains(&n) => n.parse().map_err(|_| format!("{n} isn't a number"))?,
                _ => return Err(format!("Use {}, not {v}.", SCALES.join(", "))),
            };
            Ok(())
        }),
    },
    switch_key!("share-sound", share_sound, Effect::Audio, "Sound: people viewing this computer hear what it plays"),
    switch_key!("hear-sound", hear_sound, Effect::Audio, "Sound: play the other computer's sound here"),
    switch_key!("send-mic", send_mic, Effect::Audio, "Sound: send this microphone to the computer it views"),
    switch_key!(
        "virtual-mic",
        virtual_mic,
        Effect::Audio,
        "Sound: a viewer's microphone becomes Annex Microphone here instead of playing aloud"
    ),
    switch_key!(
        "share-clipboard",
        share_clipboard,
        Effect::Clipboard,
        "Copy on one computer, paste on the other, while one controls the other"
    ),
    switch_key!(
        "receive-files",
        receive_files,
        Effect::ReceiveFiles,
        "Take files the other computer sends, into Downloads"
    ),
    switch_key!(
        "drop-island",
        drop_at_top,
        Effect::DropIsland,
        "Drag files to the top of the screen to send them while connected"
    ),
    switch_key!(
        "technical-details",
        show_technical_details,
        Effect::Render,
        "Show identity codes, routes, decoder and encoder in the window"
    ),
    switch_key!(
        "verbose-logging",
        verbose_logging,
        Effect::VerboseLog,
        "Log every step (every input event, never which keys)"
    ),
    switch_key!("check-updates", check_for_updates, Effect::CheckUpdates, "Look for updates on GitHub once a day"),
    Key {
        name: "beta-updates",
        kind: Kind::Switch,
        effect: Effect::BetaUpdates,
        about: "Include beta versions when looking for updates",
        get: |s| on_off(s.beta_updates).into(),
        set: Some(|s, v| {
            s.beta_updates = switch(v)?;
            s.beta_updates_chosen = true;
            Ok(())
        }),
    },
    Key {
        name: "server",
        kind: Kind::Text,
        effect: Effect::None,
        about: "The code server (default: Annex's, wss://connect.getannex.dev)",
        get: |s| s.rendezvous_server.clone(),
        set: Some(|s, v| {
            s.rendezvous_server = text(v);
            Ok(())
        }),
    },
    Key {
        name: "relay",
        kind: Kind::Text,
        effect: Effect::None,
        about: "Your own TURN relay (turn:host:port), for networks that block direct connections",
        get: |s| s.relay_url.clone(),
        set: Some(|s, v| {
            s.relay_url = text(v);
            Ok(())
        }),
    },
    Key {
        name: "relay-username",
        kind: Kind::Text,
        effect: Effect::None,
        about: "Your relay's user name",
        get: |s| s.relay_username.clone(),
        set: Some(|s, v| {
            s.relay_username = text(v);
            Ok(())
        }),
    },
    Key {
        name: "relay-password",
        kind: Kind::Secret,
        effect: Effect::None,
        about: "Your relay's password; `annex set relay-password` asks for it (empty clears it)",
        get: |s| if s.relay_password.is_empty() { "not set".into() } else { "set".into() },
        set: Some(|s, v| {
            // As typed: a password can start or end with a space.
            s.relay_password = v.trim_end_matches(['\r', '\n']).to_string();
            Ok(())
        }),
    },
    Key {
        name: "screen-token",
        kind: Kind::Clear,
        effect: Effect::None,
        about: "The desktop's saved permission to share the screen; clear it to be asked again",
        get: |s| if s.screen_token.is_some() { "saved".into() } else { "none".into() },
        set: Some(|s, v| {
            if !matches!(v.trim().to_ascii_lowercase().as_str(), "clear" | "none" | "") {
                return Err(format!("screen-token only takes clear, not {v}."));
            }
            s.screen_token = None;
            Ok(())
        }),
    },
    Key {
        name: "last-seen-version",
        kind: Kind::ReadOnly,
        effect: Effect::None,
        about: "The version whose What's New was last shown",
        get: |s| s.last_seen_version.clone().unwrap_or_else(|| "none".into()),
        set: None,
    },
    Key {
        name: "beta-updates-chosen",
        kind: Kind::ReadOnly,
        effect: Effect::None,
        about: "Beta updates was set by you (until then it follows the installed version)",
        get: |s| on_off(s.beta_updates_chosen).into(),
        set: None,
    },
    Key {
        name: "verbose-logging-defaulted",
        kind: Kind::ReadOnly,
        effect: Effect::None,
        about: "Internal: verbose logging was turned on once for installs from before it was the default",
        get: |s| on_off(s.verbose_logging_defaulted).into(),
        set: None,
    },
];

pub fn find(name: &str) -> Option<&'static Key> {
    let n = name.trim().to_ascii_lowercase().replace('_', "-");
    KEYS.iter().find(|k| k.name == n)
}

/// What a key shows: its value, or what the default means.
pub fn shown(k: &Key, s: &Settings) -> String {
    let v = k.get(s);
    match (k.name, v.is_empty()) {
        ("name", true) => format!("(the computer's own name: {})", crate::model::system_name()),
        ("server", true) => format!("(Annex's: {ANNEX_SERVER})"),
        (_, true) => "(none)".into(),
        _ => v,
    }
}

/// `annex settings --json`.
pub fn to_json(s: &Settings) -> serde_json::Value {
    serde_json::Value::Array(
        KEYS.iter()
            .map(|k| {
                serde_json::json!({
                    "key": k.name,
                    "value": k.get(s),
                    "allowed": k.allowed(),
                    "settable": k.settable(),
                    "about": k.about,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_has_a_key() {
        // Each field of Settings, by its saved name: adding one without a key fails here.
        let json = serde_json::to_value(Settings::default()).unwrap();
        let fields: Vec<String> = json.as_object().unwrap().keys().cloned().collect();
        let field_of = |field: &str| -> &str {
            match field {
                "deviceName" => "name",
                "openAtLogin" => "open-at-login",
                "startSharingAtLaunch" => "share-at-launch",
                "headless" => "headless",
                "quality" => "quality",
                "useMouseAndKeyboard" => "control",
                "openFullScreen" => "open-full-screen",
                "extendByDefault" => "extend-by-default",
                "shareClipboard" => "share-clipboard",
                "receiveFiles" => "receive-files",
                "dropAtTop" => "drop-island",
                "preferHardwareEncoder" => "hardware-encoder",
                "extendScale" => "extend-scale",
                "shareSound" => "share-sound",
                "hearSound" => "hear-sound",
                "sendMic" => "send-mic",
                "virtualMic" => "virtual-mic",
                "showTechnicalDetails" => "technical-details",
                "verboseLogging" => "verbose-logging",
                "checkForUpdates" => "check-updates",
                "betaUpdates" => "beta-updates",
                "betaUpdatesChosen" => "beta-updates-chosen",
                "rendezvousServer" => "server",
                "relayUrl" => "relay",
                "relayUsername" => "relay-username",
                "relayPassword" => "relay-password",
                "screenToken" => "screen-token",
                "lastSeenVersion" => "last-seen-version",
                "verboseLoggingDefaulted" => "verbose-logging-defaulted",
                other => panic!("Settings.{other} has no command-line key"),
            }
        };
        for f in &fields {
            assert!(find(field_of(f)).is_some(), "{f}");
        }
        assert_eq!(fields.len(), KEYS.len());
    }

    #[test]
    fn names_are_unique_kebab_case() {
        for (i, k) in KEYS.iter().enumerate() {
            assert!(k.name.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{}", k.name);
            assert!(KEYS[..i].iter().all(|o| o.name != k.name), "{} twice", k.name);
            assert_eq!(k.secret(), crate::cli::SECRET_KEYS.contains(&k.name), "{}", k.name);
            assert_eq!(k.settable(), k.kind != Kind::ReadOnly, "{}", k.name);
        }
    }

    #[test]
    fn switches_and_choices() {
        let mut s = Settings::default();
        find("headless").unwrap().set(&mut s, "on").unwrap();
        assert!(s.headless);
        find("headless").unwrap().set(&mut s, "false").unwrap();
        assert!(!s.headless);
        assert!(find("headless").unwrap().set(&mut s, "maybe").is_err());
        find("quality").unwrap().set(&mut s, "Fast").unwrap();
        assert_eq!(s.quality, 2);
        assert_eq!(find("quality").unwrap().get(&s), "fast");
        assert!(find("quality").unwrap().set(&mut s, "ultra").is_err());
        find("extend-scale").unwrap().set(&mut s, "150%").unwrap();
        assert_eq!(s.extend_scale, 150);
        find("extend-scale").unwrap().set(&mut s, "auto").unwrap();
        assert_eq!(s.extend_scale, 0);
        assert!(find("extend-scale").unwrap().set(&mut s, "130").is_err());
        find("beta-updates").unwrap().set(&mut s, "on").unwrap();
        assert!(s.beta_updates && s.beta_updates_chosen);
        find("drop_island").unwrap().set(&mut s, "off").unwrap();
        assert!(!s.drop_at_top);
        assert!(find("last-seen-version").unwrap().set(&mut s, "1.0").is_err());
    }

    #[test]
    fn text_and_secrets() {
        let mut s = Settings::default();
        find("name").unwrap().set(&mut s, "  Living room Pi ").unwrap();
        assert_eq!(s.device_name.as_deref(), Some("Living room Pi"));
        find("name").unwrap().set(&mut s, "default").unwrap();
        assert_eq!(s.device_name, None);
        find("server").unwrap().set(&mut s, "wss://example.com").unwrap();
        assert_eq!(s.server(), "wss://example.com");
        find("server").unwrap().set(&mut s, "").unwrap();
        assert_eq!(s.server(), ANNEX_SERVER);
        find("relay-password").unwrap().set(&mut s, "hunter2\n").unwrap();
        assert_eq!(s.relay_password, "hunter2");
        let shown = find("relay-password").unwrap().get(&s);
        assert!(!shown.contains("hunter2"));
        assert!(!to_json(&s).to_string().contains("hunter2"));
        s.screen_token = Some("tok-5150".into());
        assert!(!to_json(&s).to_string().contains("tok-5150"));
        find("screen-token").unwrap().set(&mut s, "clear").unwrap();
        assert!(s.screen_token.is_none());
    }
}
