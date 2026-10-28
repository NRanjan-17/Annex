//! The command line: `annex <command>`, parsed without GTK. The options Annex always had
//! (`--background --share --headless --connect <address | code> [--extend]`) still start it;
//! commands (`annex status`, `annex set headless on`, …) go to the Annex that's running, or edit
//! its settings when none is (see `remote.rs`).
//!
//! Plain Rust with no dependencies, so the parser and its tests stand on their own.

/// How Annex starts (the options it always had).
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct LaunchOpts {
    /// Start in the tray without the window (open at login).
    pub background: bool,
    /// Share this screen right away.
    pub share: bool,
    /// Nobody at this screen: turn on (and keep) the headless setting.
    pub headless: bool,
    /// Connect to this address (or code) right away; `--extend` to use it as a display.
    pub connect: Option<String>,
    pub extend: bool,
}

/// What the command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Start Annex (or, if it's running, bring it forward and apply the options).
    Launch(LaunchOpts),
    /// `annex help [command]`.
    Help(Option<String>),
    Version,
    Run(Command),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Status {
        json: bool,
    },
    Share(bool),
    Start {
        headless: bool,
        share: bool,
    },
    Quit,
    Settings {
        json: bool,
    },
    Get {
        key: String,
        json: bool,
    },
    /// `value` is `None` for a secret key until it's read from stdin.
    Set {
        key: String,
        value: Option<String>,
    },
    Devices {
        json: bool,
    },
    AllowControl {
        device: String,
        on: bool,
    },
    Forget {
        device: String,
        yes: bool,
    },
    Nearby {
        json: bool,
    },
    /// `extend`: `None` follows the Extend display by default setting.
    Connect {
        to: String,
        extend: Option<bool>,
    },
    Disconnect,
    Code,
    CodeStop,
    Pin,
    /// The PIN another computer shows, read from stdin.
    PinEnter {
        pin: Option<String>,
    },
    Send {
        files: Vec<String>,
    },
    Transfers {
        json: bool,
    },
    Pause(u64),
    Resume(u64),
    Unsend(u64),
    UpdateCheck {
        json: bool,
    },
    Logs {
        path: bool,
        follow: bool,
        lines: usize,
    },
}

/// Bad usage: the message, then exit status 2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage(pub String);

/// Settings whose values never go on the command line (read from stdin instead).
pub const SECRET_KEYS: &[&str] = &["relay-password"];

/// Carries a secret from `annex` to the running Annex after it was read from stdin. Internal:
/// not in the help, and refused for keys that aren't secret.
pub const SECRET_FLAG: &str = "--secret";

pub fn parse(args: &[String]) -> Result<Parsed, Usage> {
    let Some(first) = args.first() else { return Ok(Parsed::Launch(LaunchOpts::default())) };
    match first.as_str() {
        "--version" | "-V" | "version" => return Ok(Parsed::Version),
        "--help" | "-h" => return Ok(Parsed::Help(None)),
        "help" => return Ok(Parsed::Help(args.get(1).cloned())),
        a if a.starts_with('-') => return parse_launch(args),
        _ => {}
    }
    let name = first.as_str();
    let rest = &args[1..];
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Parsed::Help(Some(name.to_string())));
    }
    let mut a = Args::new(name, rest);
    let cmd = match name {
        "status" => Command::Status { json: a.flag("--json") },
        "share" => Command::Share(a.on_off("on or off")?),
        "start" => Command::Start { headless: a.flag("--headless"), share: a.flag("--share") },
        "quit" | "stop" => Command::Quit,
        "settings" => Command::Settings { json: a.flag("--json") },
        "get" => {
            let json = a.flag("--json");
            Command::Get { key: a.word("a setting (see `annex settings`)")?, json }
        }
        "set" => {
            let secret = a.value(SECRET_FLAG)?;
            let key = a.word("a setting (see `annex settings`)")?;
            if SECRET_KEYS.contains(&key.as_str()) {
                if !a.rest.is_empty() {
                    return Err(Usage(format!(
                        "{key} isn't taken on the command line, where it would stay in the shell's history. \
                         Run `annex set {key}` and type it, or pipe it in."
                    )));
                }
                Command::Set { key, value: secret }
            } else {
                if secret.is_some() {
                    return Err(Usage(format!("{SECRET_FLAG} is only for {}", SECRET_KEYS.join(", "))));
                }
                // The rest, joined: `annex set name Living room Pi`.
                let value = std::mem::take(&mut a.rest).join(" ");
                if value.is_empty() && !a.had_empty {
                    return Err(Usage(format!("`annex set {key}` needs a value (see `annex settings`)")));
                }
                Command::Set { key, value: Some(value) }
            }
        }
        "devices" => match a.rest.first().map(String::as_str) {
            None | Some("--json") => Command::Devices { json: a.flag("--json") },
            Some("allow-control") => {
                a.rest.remove(0);
                let device = a.word("a device's name or identity code")?;
                Command::AllowControl { device, on: a.on_off("on or off")? }
            }
            Some("forget" | "remove") => {
                a.rest.remove(0);
                let yes = a.flag("--yes") || a.flag("-y");
                Command::Forget { device: a.joined("a device's name or identity code")?, yes }
            }
            Some(other) => return Err(Usage(format!("unknown `annex devices {other}`; see `annex help devices`"))),
        },
        "nearby" => Command::Nearby { json: a.flag("--json") },
        "connect" => {
            let extend = a.flag("--extend");
            let mirror = a.flag("--mirror");
            if extend && mirror {
                return Err(Usage("choose --extend or --mirror, not both".into()));
            }
            let to = a.joined("an address, a code, or the name of a computer nearby")?;
            Command::Connect { to, extend: (extend || mirror).then_some(extend) }
        }
        "disconnect" => Command::Disconnect,
        "code" => match a.rest.first().map(String::as_str) {
            None => Command::Code,
            Some("stop") => {
                a.rest.remove(0);
                Command::CodeStop
            }
            Some(other) => return Err(Usage(format!("unknown `annex code {other}`; see `annex help code`"))),
        },
        "pin" => {
            let secret = a.value(SECRET_FLAG)?;
            match a.rest.first().map(String::as_str) {
                None if secret.is_none() => Command::Pin,
                Some("enter") => {
                    a.rest.remove(0);
                    if !a.rest.is_empty() {
                        return Err(Usage(
                            "the PIN isn't taken on the command line. Run `annex pin enter` and type it.".into(),
                        ));
                    }
                    Command::PinEnter { pin: secret }
                }
                _ => return Err(Usage("use `annex pin` or `annex pin enter`".into())),
            }
        }
        "send" => {
            let files = std::mem::take(&mut a.rest);
            if files.is_empty() {
                return Err(Usage("`annex send` needs one or more files".into()));
            }
            Command::Send { files }
        }
        "transfers" => Command::Transfers { json: a.flag("--json") },
        "pause" => Command::Pause(a.id()?),
        "resume" => Command::Resume(a.id()?),
        "unsend" | "cancel" => Command::Unsend(a.id()?),
        "update" => {
            let json = a.flag("--json");
            match a.rest.first().map(String::as_str) {
                None | Some("check") => {
                    if !a.rest.is_empty() {
                        a.rest.remove(0);
                    }
                    Command::UpdateCheck { json }
                }
                Some(other) => return Err(Usage(format!("unknown `annex update {other}`; use `annex update check`"))),
            }
        }
        "logs" | "log" => {
            let path = a.flag("--path");
            let follow = a.flag("--follow") || a.flag("-f");
            let lines = match a.value("--lines")?.or(a.value("-n")?) {
                Some(n) => n.parse().map_err(|_| Usage(format!("--lines needs a number, not {n}")))?,
                None => 40,
            };
            Command::Logs { path, follow, lines }
        }
        other => return Err(Usage(format!("unknown command `{other}`; `annex help` lists them"))),
    };
    a.done()?;
    Ok(Parsed::Run(cmd))
}

/// The options Annex always had.
fn parse_launch(args: &[String]) -> Result<Parsed, Usage> {
    let mut o = LaunchOpts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--background" => o.background = true,
            "--share" => o.share = true,
            "--headless" => o.headless = true,
            "--extend" => o.extend = true,
            "--connect" => match it.next() {
                Some(to) => o.connect = Some(to.clone()),
                None => return Err(Usage("--connect needs an address or a code".into())),
            },
            "--version" | "-V" => return Ok(Parsed::Version),
            "--help" | "-h" => return Ok(Parsed::Help(None)),
            other => {
                let hint = if other.starts_with('-') { "" } else { " (commands come first: `annex <command> …`)" };
                return Err(Usage(format!("unknown option {other}{hint}; `annex help` lists the options")));
            }
        }
    }
    Ok(Parsed::Launch(o))
}

/// A command's arguments, taken one by one; whatever is left over is an error.
struct Args {
    command: String,
    rest: Vec<String>,
    /// An empty argument was given (`annex set server ""`).
    had_empty: bool,
}

impl Args {
    fn new(command: &str, rest: &[String]) -> Args {
        Args { command: command.to_string(), had_empty: rest.iter().any(String::is_empty), rest: rest.to_vec() }
    }

    fn flag(&mut self, name: &str) -> bool {
        match self.rest.iter().position(|a| a == name) {
            Some(i) => {
                self.rest.remove(i);
                true
            }
            None => false,
        }
    }

    fn value(&mut self, name: &str) -> Result<Option<String>, Usage> {
        let Some(i) = self.rest.iter().position(|a| a == name) else { return Ok(None) };
        if i + 1 >= self.rest.len() {
            return Err(Usage(format!("{name} needs a value")));
        }
        self.rest.remove(i);
        Ok(Some(self.rest.remove(i)))
    }

    /// The next word that isn't an option.
    fn word(&mut self, what: &str) -> Result<String, Usage> {
        match self.rest.iter().position(|a| !a.starts_with("--")) {
            Some(i) => Ok(self.rest.remove(i)),
            None => Err(Usage(format!("`annex {}` needs {what}", self.command))),
        }
    }

    /// Every word that's left, joined with spaces (names can have spaces).
    fn joined(&mut self, what: &str) -> Result<String, Usage> {
        let words: Vec<String> = self.rest.iter().filter(|a| !a.starts_with("--")).cloned().collect();
        if words.is_empty() {
            return Err(Usage(format!("`annex {}` needs {what}", self.command)));
        }
        self.rest.retain(|a| a.starts_with("--"));
        Ok(words.join(" "))
    }

    fn on_off(&mut self, what: &str) -> Result<bool, Usage> {
        let w = self.word(what)?;
        parse_bool(&w).ok_or_else(|| Usage(format!("`annex {}` takes {what}, not {w}", self.command)))
    }

    fn id(&mut self) -> Result<u64, Usage> {
        let w = self.word("a transfer's number (see `annex transfers`)")?;
        w.trim_start_matches('#').parse().map_err(|_| Usage(format!("{w} isn't a transfer number")))
    }

    fn done(self) -> Result<(), Usage> {
        match self.rest.first() {
            None => Ok(()),
            Some(extra) => {
                Err(Usage(format!("`annex {}` doesn't take {extra}; see `annex help {}`", self.command, self.command)))
            }
        }
    }
}

/// on/off, true/false, yes/no, 1/0.
pub fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" | "enable" | "enabled" => Some(true),
        "off" | "false" | "no" | "0" | "disable" | "disabled" => Some(false),
        _ => None,
    }
}

pub fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// An internet code ("K7P-4QX"), not an address: six letters and digits, not all digits.
pub fn looks_like_code(s: &str) -> bool {
    let t: String = s.trim().chars().filter(|c| *c != '-').collect();
    t.len() == 6 && t.chars().all(|c| c.is_ascii_alphanumeric()) && !t.chars().all(|c| c.is_ascii_digit())
}

impl Command {
    /// The command's name for the log: never a value that could be secret (a PIN, a code, a
    /// relay password) or a file's name.
    pub fn describe(&self) -> String {
        match self {
            Command::Status { .. } => "status".into(),
            Command::Share(on) => format!("share {}", on_off(*on)),
            Command::Start { headless, share } => format!("start headless={headless} share={share}"),
            Command::Quit => "quit".into(),
            Command::Settings { .. } => "settings".into(),
            Command::Get { key, .. } => format!("get {key}"),
            Command::Set { key, .. } => format!("set {key}"),
            Command::Devices { .. } => "devices".into(),
            Command::AllowControl { on, .. } => format!("devices allow-control {}", on_off(*on)),
            Command::Forget { .. } => "devices forget".into(),
            Command::Nearby { .. } => "nearby".into(),
            Command::Connect { to, extend } => format!(
                "connect ({}{})",
                if looks_like_code(to) { "code" } else { "address or name" },
                match extend {
                    Some(true) => ", extend",
                    Some(false) => ", mirror",
                    None => "",
                }
            ),
            Command::Disconnect => "disconnect".into(),
            Command::Code => "code".into(),
            Command::CodeStop => "code stop".into(),
            Command::Pin => "pin".into(),
            Command::PinEnter { .. } => "pin enter".into(),
            Command::Send { files } => format!("send ({} items)", files.len()),
            Command::Transfers { .. } => "transfers".into(),
            Command::Pause(id) => format!("pause {id}"),
            Command::Resume(id) => format!("resume {id}"),
            Command::Unsend(id) => format!("unsend {id}"),
            Command::UpdateCheck { .. } => "update check".into(),
            Command::Logs { .. } => "logs".into(),
        }
    }
}

/// Finds a paired device by name (any case) or by the start of its identity code (spaces and case
/// don't matter, at least 4 characters). `devices`: (name, fingerprint).
pub fn match_device(query: &str, devices: &[(String, String)]) -> Result<usize, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("Say which device: its name or identity code (see `annex devices`).".into());
    }
    let by_name: Vec<usize> =
        devices.iter().enumerate().filter(|(_, (n, _))| n.trim().eq_ignore_ascii_case(q)).map(|(i, _)| i).collect();
    let hex: String = q.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    let by_code: Vec<usize> = if hex.len() >= 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        devices
            .iter()
            .enumerate()
            .filter(|(_, (_, fp))| fp.to_ascii_lowercase().starts_with(&hex))
            .map(|(i, _)| i)
            .collect()
    } else {
        Vec::new()
    };
    let mut all = by_name;
    for i in by_code {
        if !all.contains(&i) {
            all.push(i);
        }
    }
    match all.as_slice() {
        [i] => Ok(*i),
        [] => Err(format!("No paired device is called {q}. `annex devices` lists them.")),
        _ => Err(format!("More than one paired device matches {q}: use more of its identity code.")),
    }
}

/// "1.2 MB".
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} bytes") } else { format!("{v:.1} {}", UNITS[u]) }
}

pub const HELP: &str = "\
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
";

/// More on one command.
pub fn help_for(command: &str) -> Option<&'static str> {
    Some(match command {
        "status" => {
            "annex status [--json]\n\nWhether Annex is running, its version and this computer's name, whether No one at this \
             screen is on, sharing (and its port), who's connected and how (Mirror or Extend, controlling or \
             not), what this computer is viewing, an internet code's time left, a waiting pairing PIN, and \
             whether an update is available.\n"
        }
        "share" => {
            "annex share on|off\n\nShares this screen with computers that connect to it, or stops (ending every \
             session). The first time, the desktop may ask for permission on the screen itself.\n"
        }
        "start" => {
            "annex start [--headless] [--share]\n\nStarts Annex in the background (in the tray, without its window) \
             and waits until it's running. From SSH it uses the desktop's display (WAYLAND_DISPLAY or DISPLAY; \
             found in the session if not set). --headless turns on No one at this screen, --share shares \
             this screen. If Annex is already running, the options are applied to it.\n"
        }
        "quit" | "stop" => "annex quit\n\nQuits Annex: sharing stops and every session ends.\n",
        "settings" | "get" | "set" => {
            "annex settings [--json]\nannex get <setting> [--json]\nannex set <setting> <value>\n\nSwitches take \
             on/off (or true/false, yes/no). `annex settings` lists every setting with its value and what it \
             takes. A running Annex applies a change at once, as its Settings page does (open-at-login writes \
             the autostart entry, verbose-logging switches the log, name renames this computer…); otherwise \
             the settings file is changed and Annex uses it when it starts.\n\n`annex set relay-password` reads \
             the password from the terminal without showing it (or from a pipe); it's never taken on the \
             command line. Text settings (server, relay, relay-username, name) go back to their default with \
             `default`.\n"
        }
        "devices" => {
            "annex devices [--json]\nannex devices allow-control <device> on|off\nannex devices forget <device> \
             [--yes]\n\nPaired devices: name, identity code, when it last connected, and whether it may use this \
             computer's mouse and keyboard. <device> is its name or the start of its identity code. forget asks \
             first unless --yes is given; the device needs a PIN to connect again.\n"
        }
        "nearby" => {
            "annex nearby [--json]\n\nComputers with Annex open on this network (or a private network like \
             Tailscale): whether each is sharing, whether it can be extended to, how it's reached and at which \
             address.\n"
        }
        "connect" | "disconnect" => {
            "annex connect <address | code | name> [--extend | --mirror]\nannex disconnect\n\nViews another \
             computer's screen (--mirror) or uses it as an extra display (--extend); without either, Extend \
             display by default decides. <name> is a computer listed by `annex nearby`. If the other computer \
             asks for a PIN, type it with `annex pin enter`. The video window opens on this computer's screen.\n"
        }
        "code" => {
            "annex code\nannex code stop\n\nGets a code for connecting over the internet, prints it and how long it \
             works (10 minutes, once). Sharing turns on if it was off.\n"
        }
        "pin" => {
            "annex pin\nannex pin enter\n\n`annex pin` prints the PIN to type on a computer that's pairing with this \
             one (with No one at this screen on, it's also in pairing-pin.txt in Annex's data folder). `annex \
             pin enter` asks for the PIN another computer shows, when this one connects to it.\n"
        }
        "send" | "transfers" | "pause" | "resume" | "unsend" => {
            "annex send <file>...\nannex transfers [--json]\nannex pause|resume|unsend <number>\n\nSends files (a \
             folder sends what's in it) to the computer this one controls, or to viewers that control this \
             one. transfers lists them with a number each and their progress. unsend stops a file being sent \
             (the other computer deletes what arrived), or stops one arriving.\n"
        }
        "update" => {
            "annex update check [--json]\n\nLooks for a newer version on GitHub and prints it with its release \
             page. Install it with the package manager (.deb or .rpm).\n"
        }
        "logs" | "log" => {
            "annex logs [--path] [--follow] [-n <lines>]\n\nPrints the end of Annex's log (40 lines unless -n says \
             otherwise), only its path with --path, or follows it with --follow. Attach it to a bug report.\n"
        }
        "version" => "annex version\n\nPrints Annex's version.\n",
        "help" => "annex help [command]\n\nLists the commands, or tells more about one.\n",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Parsed, Usage> {
        let args: Vec<String> = s.split_whitespace().map(String::from).collect();
        parse(&args)
    }

    fn run(s: &str) -> Command {
        match p(s) {
            Ok(Parsed::Run(c)) => c,
            other => panic!("{s}: {other:?}"),
        }
    }

    #[test]
    fn old_options_still_launch() {
        assert_eq!(p(""), Ok(Parsed::Launch(LaunchOpts::default())));
        assert_eq!(
            p("--background --share --headless"),
            Ok(Parsed::Launch(LaunchOpts { background: true, share: true, headless: true, ..Default::default() }))
        );
        assert_eq!(
            p("--connect K7P-4QX --extend"),
            Ok(Parsed::Launch(LaunchOpts { connect: Some("K7P-4QX".into()), extend: true, ..Default::default() }))
        );
        assert_eq!(p("--version"), Ok(Parsed::Version));
        assert_eq!(p("-h"), Ok(Parsed::Help(None)));
        assert!(p("--connect").is_err());
        assert!(p("--bogus").is_err());
    }

    #[test]
    fn commands() {
        assert_eq!(run("status"), Command::Status { json: false });
        assert_eq!(run("status --json"), Command::Status { json: true });
        assert_eq!(run("share on"), Command::Share(true));
        assert_eq!(run("share off"), Command::Share(false));
        assert_eq!(run("start --share --headless"), Command::Start { headless: true, share: true });
        assert_eq!(run("quit"), Command::Quit);
        assert_eq!(run("get quality --json"), Command::Get { key: "quality".into(), json: true });
        assert_eq!(run("set headless on"), Command::Set { key: "headless".into(), value: Some("on".into()) });
        assert_eq!(
            run("set name Living room Pi"),
            Command::Set { key: "name".into(), value: Some("Living room Pi".into()) }
        );
        assert_eq!(run("devices"), Command::Devices { json: false });
        assert_eq!(run("devices --json"), Command::Devices { json: true });
        assert_eq!(run("devices allow-control Mac on"), Command::AllowControl { device: "Mac".into(), on: true });
        assert_eq!(run("devices forget Mac Studio --yes"), Command::Forget { device: "Mac Studio".into(), yes: true });
        assert_eq!(run("connect 10.0.0.2"), Command::Connect { to: "10.0.0.2".into(), extend: None });
        assert_eq!(run("connect K7P-4QX --extend"), Command::Connect { to: "K7P-4QX".into(), extend: Some(true) });
        assert_eq!(run("connect Mac --mirror"), Command::Connect { to: "Mac".into(), extend: Some(false) });
        assert_eq!(run("code"), Command::Code);
        assert_eq!(run("code stop"), Command::CodeStop);
        assert_eq!(run("pin"), Command::Pin);
        assert_eq!(run("pin enter"), Command::PinEnter { pin: None });
        assert_eq!(run("pin enter --secret 123456"), Command::PinEnter { pin: Some("123456".into()) });
        assert_eq!(run("send a.txt b.txt"), Command::Send { files: vec!["a.txt".into(), "b.txt".into()] });
        assert_eq!(run("pause 3"), Command::Pause(3));
        assert_eq!(run("unsend #4"), Command::Unsend(4));
        assert_eq!(run("update check"), Command::UpdateCheck { json: false });
        assert_eq!(run("update"), Command::UpdateCheck { json: false });
        assert_eq!(run("logs -f -n 100"), Command::Logs { path: false, follow: true, lines: 100 });
        assert_eq!(run("logs --path"), Command::Logs { path: true, follow: false, lines: 40 });
        assert_eq!(p("version"), Ok(Parsed::Version));
        assert_eq!(p("help set"), Ok(Parsed::Help(Some("set".into()))));
        assert_eq!(p("share --help"), Ok(Parsed::Help(Some("share".into()))));
    }

    #[test]
    fn empty_value_clears() {
        let args: Vec<String> = vec!["set".into(), "server".into(), String::new()];
        assert_eq!(parse(&args), Ok(Parsed::Run(Command::Set { key: "server".into(), value: Some(String::new()) })));
    }

    #[test]
    fn bad_usage() {
        for s in [
            "share",
            "share maybe",
            "set",
            "set headless",
            "status extra",
            "connect",
            "connect a --extend --mirror",
            "pause x",
            "devices dance",
            "code now",
            "send",
            "frobnicate",
            "logs -n many",
            "status --share",
        ] {
            assert!(p(s).is_err(), "{s} should be bad usage");
        }
    }

    #[test]
    fn secrets_stay_off_the_command_line() {
        assert!(p("set relay-password hunter2").is_err());
        assert_eq!(run("set relay-password"), Command::Set { key: "relay-password".into(), value: None });
        assert_eq!(
            run("set relay-password --secret hunter2"),
            Command::Set { key: "relay-password".into(), value: Some("hunter2".into()) }
        );
        assert!(p("set headless --secret on").is_err());
        assert!(p("pin enter 123456").is_err());
        assert!(!run("set relay-password --secret hunter2").describe().contains("hunter2"));
        assert!(!run("pin enter --secret 123456").describe().contains("123456"));
        assert!(!run("connect K7P-4QX").describe().contains("K7P"));
        assert!(!run("connect 10.1.2.3").describe().contains("10.1"));
    }

    #[test]
    fn bools() {
        for s in ["on", "ON", "true", "yes", "1"] {
            assert_eq!(parse_bool(s), Some(true));
        }
        for s in ["off", "false", "No", "0"] {
            assert_eq!(parse_bool(s), Some(false));
        }
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn codes() {
        assert!(looks_like_code("K7P-4QX"));
        assert!(looks_like_code("k7p4qx"));
        assert!(!looks_like_code("123456"));
        assert!(!looks_like_code("10.0.0.2"));
    }

    #[test]
    fn devices_match_by_name_or_code() {
        let d = vec![
            ("Mac Studio".to_string(), "34cbb46a8cdc".to_string()),
            ("Pi".to_string(), "34cc0000ffff".to_string()),
            ("pi".to_string(), "aaaa1111bbbb".to_string()),
        ];
        assert_eq!(match_device("mac studio", &d), Ok(0));
        assert_eq!(match_device("34CB B46A", &d), Ok(0));
        assert_eq!(match_device("aaaa", &d), Ok(2));
        assert!(match_device("PI", &d).is_err()); // two named Pi
        assert!(match_device("34c", &d).is_err()); // too short to be a code
        assert!(match_device("34cc", &d).is_ok());
        assert!(match_device("Windows", &d).is_err());
        assert!(match_device("", &d).is_err());
    }

    #[test]
    fn every_command_has_help() {
        for c in [
            "status",
            "share",
            "start",
            "quit",
            "settings",
            "get",
            "set",
            "devices",
            "nearby",
            "connect",
            "disconnect",
            "code",
            "pin",
            "send",
            "transfers",
            "pause",
            "resume",
            "unsend",
            "update",
            "logs",
            "version",
            "help",
        ] {
            assert!(help_for(c).is_some(), "{c}");
            assert!(HELP.contains(c), "{c} missing from the help");
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(size(12), "12 bytes");
        assert_eq!(size(1_234_567), "1.2 MB");
    }
}
