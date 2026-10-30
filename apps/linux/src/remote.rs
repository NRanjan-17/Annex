//! `annex <command>` from a terminal (over SSH too), against the Annex that's running.
//!
//! The app is a `GApplication` with `HANDLES_COMMAND_LINE`: a second `annex` hands its command
//! line to the running one over the session bus, which runs it on its main thread against the
//! [`Model`] and prints the answer in the first one's terminal (`ApplicationCommandLine`). The
//! terminal waits until the running Annex lets go of the command line, so a command can wait for
//! its outcome (a connection, a code) before answering. Commands never open a window.
//!
//! [`client`] runs in the terminal's process, before GTK, GStreamer and the log start: it finds
//! out whether Annex is running and hands the command over ([`forward_to_annex`]), or does what it can
//! without it ([`offline`]: settings and paired devices are files). [`run`] runs in Annex.

use std::io::{BufRead, IsTerminal, Write};
use std::time::{Duration, Instant};

use annex_session::TrustedPeer;
use gtk::prelude::*;
use gtk::{gio, glib};
use serde_json::json;

use crate::cli::{self, Command, Parsed, Permission, on_off};
use crate::model::{InternetShare, Model, PinAsked, SessionRole, Shared, UpdateState};
use crate::settings::{self, Settings};
use crate::settings_keys::{self, Effect, Key};
use crate::ui;

const NOT_RUNNING: &str = "Annex isn't running; start it with `annex start --headless --share`.";

fn version() -> &'static str {
    annex_update::whats_new::CURRENT
}

// ---- In the terminal ---------------------------------------------------------------------

/// `annex <command>`: hands it to the running Annex, or does it here when none is running.
pub fn client(args: &[String], cmd: Command) -> glib::ExitCode {
    if let Command::Logs { path, follow, lines } = cmd {
        return logs(path, follow, lines);
    }
    let running = match is_running() {
        Ok(r) => r,
        Err(e) => {
            if let Command::Start { headless, share } = cmd {
                return start(headless, share, false);
            }
            // Annex may be running anyway: nothing is changed behind its back.
            return fail(&format!(
                "Can't reach this desktop session's bus, so Annex can't be asked ({e}). Over SSH, log in as \
                 the user whose desktop runs Annex; if that's not enough, set \
                 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus."
            ));
        }
    };
    if !running && !matches!(cmd, Command::Set { .. } | Command::Allow { .. } | Command::Forget { .. }) {
        return offline(cmd);
    }
    let mut cmd = cmd;
    let mut forward = args.to_vec();
    // Secrets are read here (never from the command line) and travel with the command.
    match &mut cmd {
        Command::Set { key, value: value @ None } => match read_secret(&format!("{key}: ")) {
            Ok(v) => {
                forward.extend([cli::SECRET_FLAG.to_string(), v.clone()]);
                *value = Some(v);
            }
            Err(e) => return fail(&e),
        },
        Command::PinEnter { pin: pin @ None } => match read_secret("The PIN the other computer shows: ") {
            Ok(v) => {
                let digits: String = v.chars().filter(|c| !c.is_whitespace()).collect();
                forward.extend([cli::SECRET_FLAG.to_string(), digits.clone()]);
                *pin = Some(digits);
            }
            Err(e) => return fail(&e),
        },
        Command::Forget { device, yes } if !*yes => {
            if !confirm(&format!("Forget {device}? It will need a PIN to connect again. [y/N] ")) {
                eprintln!("Not forgotten.");
                return glib::ExitCode::FAILURE;
            }
            forward.push("--yes".into());
            *yes = true;
        }
        _ => {}
    }
    if running { forward_to_annex(&forward) } else { offline(cmd) }
}

/// Is Annex running (does anything own its name on the session bus)?
fn is_running() -> Result<bool, String> {
    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).map_err(|e| e.to_string())?;
    let reply = bus
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(crate::APP_ID.to_string(),).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
        .map_err(|e| e.to_string())?;
    Ok(reply.get::<(bool,)>().is_some_and(|(owned,)| owned))
}

/// Hands the command line to the running Annex and waits for its answer and exit status. A
/// launcher never becomes the running Annex itself, and starts no GTK.
fn forward_to_annex(args: &[String]) -> glib::ExitCode {
    let app = gio::Application::new(
        Some(crate::APP_ID),
        gio::ApplicationFlags::IS_LAUNCHER | gio::ApplicationFlags::HANDLES_COMMAND_LINE,
    );
    if let Err(e) = app.register(gio::Cancellable::NONE) {
        return fail(&format!("Annex is running but didn't answer: {e}"));
    }
    app.run_with_args(args)
}

fn fail(msg: &str) -> glib::ExitCode {
    eprintln!("{msg}");
    glib::ExitCode::FAILURE
}

/// Reads a line without showing it (from a pipe too). Never logged.
fn read_secret(prompt: &str) -> Result<String, String> {
    let stdin = std::io::stdin();
    let mut line = String::new();
    if stdin.is_terminal() {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let echo = EchoOff::new();
        let read = stdin.lock().read_line(&mut line);
        drop(echo);
        eprintln!();
        read.map_err(|e| e.to_string())?;
    } else {
        stdin.lock().read_line(&mut line).map_err(|e| e.to_string())?;
    }
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// The terminal stops showing what's typed, until dropped.
struct EchoOff(Option<libc::termios>);

impl EchoOff {
    fn new() -> EchoOff {
        // SAFETY: plain termios calls on stdin with a zeroed struct for the result.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                return EchoOff(None);
            }
            let saved = t;
            t.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t) != 0 {
                return EchoOff(None);
            }
            EchoOff(Some(saved))
        }
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        if let Some(t) = &self.0 {
            // SAFETY: restores what tcgetattr returned.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, t);
            }
        }
    }
}

/// y/N in the terminal; no terminal is a no.
fn confirm(question: &str) -> bool {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        eprintln!("Add --yes to do this without a terminal to ask in.");
        return false;
    }
    eprint!("{question}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).is_ok() && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Annex isn't running: settings and paired devices are files; the rest needs Annex.
fn offline(cmd: Command) -> glib::ExitCode {
    match cmd {
        Command::Status { json } => {
            let s = Settings::load();
            let name = s.device_name.clone().unwrap_or_else(crate::model::system_name);
            if json {
                let v = json!({
                    "running": false,
                    "version": version(),
                    "name": name,
                    "headless": s.headless,
                    "share_at_launch": s.start_sharing_at_launch,
                });
                println!("{}", pretty(&v));
            } else {
                println!("Annex {} isn't running. `annex start` starts it.", version());
                println!("This computer: {name}");
                println!("No one at this screen: {}", on_off(s.headless));
                println!("Share this screen when Annex opens: {}", on_off(s.start_sharing_at_launch));
            }
            glib::ExitCode::SUCCESS
        }
        Command::Settings { json } => {
            print!("{}", settings_list(&Settings::load(), json));
            glib::ExitCode::SUCCESS
        }
        Command::Get { key, json } => match get(&key, &Settings::load(), json) {
            Ok(v) => {
                println!("{v}");
                glib::ExitCode::SUCCESS
            }
            Err(e) => usage(&e),
        },
        Command::Set { key, value } => {
            let Some(k) = settings_keys::find(&key) else { return usage(&unknown_key(&key)) };
            let mut s = Settings::load();
            if let Err(e) = k.set(&mut s, value.as_deref().unwrap_or_default()) {
                return usage(&e);
            }
            annex_log::init(&settings::data_dir(), false);
            tracing::info!(target: "annex::cli", "cli: set {} (Annex isn't running)", k.name);
            s.save();
            if k.effect == Effect::OpenAtLogin
                && let Err(e) = settings::set_open_at_login(s.open_at_login)
            {
                return fail(&format!("Couldn't change the autostart entry: {e}"));
            }
            println!("{}: {} (Annex uses it when it starts)", k.name, settings_keys::shown(k, &s));
            glib::ExitCode::SUCCESS
        }
        Command::Devices { json } => match offline_core() {
            Ok(core) => {
                let s = Settings::load();
                let mut list = core.map(|c| c.trusted_peers()).unwrap_or_default();
                list.sort_by_key(|p| p.name.to_lowercase());
                print!("{}", devices_list(&list, s.headless, json, |n| n.to_string()));
                glib::ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        },
        Command::Allow { what, device, on } => {
            let core = match offline_core() {
                Ok(Some(c)) => c,
                Ok(None) => return fail("No paired devices yet."),
                Err(e) => return fail(&e),
            };
            // Only the fingerprint is taken from the list; answers name the device as it was typed.
            let fp = match find_device(&core.trusted_peers(), &device) {
                Ok((fp, _)) => fp,
                Err(e) => return fail(&e),
            };
            annex_log::init(&settings::data_dir(), false);
            tracing::info!(target: "annex::cli", "cli: devices {} {} (Annex isn't running)", what.word(), on_off(on));
            let changed = match what {
                Permission::Control => core.set_allow_input(&fp, on),
                Permission::Sound => core.set_allow_sound(&fp, on),
                Permission::Mic => core.set_allow_mic(&fp, on),
            };
            match changed {
                Ok(_) => {
                    println!("{}", what.sentence(&device, on));
                    glib::ExitCode::SUCCESS
                }
                Err(e) => fail(&format!("Couldn't change it: {e}")),
            }
        }
        Command::Forget { device, .. } => {
            let core = match offline_core() {
                Ok(Some(c)) => c,
                Ok(None) => return fail("No paired devices yet."),
                Err(e) => return fail(&e),
            };
            let fp = match find_device(&core.trusted_peers(), &device) {
                Ok((fp, _)) => fp,
                Err(e) => return fail(&e),
            };
            annex_log::init(&settings::data_dir(), false);
            tracing::info!(target: "annex::cli", "cli: devices forget (Annex isn't running)");
            match core.untrust(&fp) {
                Ok(_) => {
                    println!("Forgot {device}. It needs a PIN to connect again.");
                    glib::ExitCode::SUCCESS
                }
                Err(e) => fail(&format!("Couldn't forget it: {e}")),
            }
        }
        Command::Start { headless, share } => start(headless, share, true),
        Command::Quit => {
            println!("Annex isn't running.");
            glib::ExitCode::SUCCESS
        }
        Command::Logs { path, follow, lines } => logs(path, follow, lines),
        _ => fail(NOT_RUNNING),
    }
}

/// The identity and paired devices, when Annex isn't running. None before Annex ever ran here
/// (opening them would make an identity).
fn offline_core() -> Result<Option<std::sync::Arc<annex_session::Core>>, String> {
    let data = settings::data_dir();
    if !data.join("identity.cert.der").exists() {
        return Ok(None);
    }
    let name = Settings::load().device_name.unwrap_or_else(crate::model::system_name);
    annex_session::Core::open(&data, &name, annex_session::TrustPolicy::Pair)
        .map(Some)
        .map_err(|e| format!("Couldn't read the paired devices in {}: {e}", data.display()))
}

fn usage(msg: &str) -> glib::ExitCode {
    eprintln!("{msg}");
    glib::ExitCode::from(2)
}

/// `annex start`: Annex in the background, on the desktop's display, out of this terminal's
/// session so logging out of SSH doesn't end it.
fn start(headless: bool, share: bool, wait: bool) -> glib::ExitCode {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().unwrap_or_else(|_| "annex".into());
    let mut c = std::process::Command::new(exe);
    c.arg("--background");
    if headless {
        c.arg("--headless");
    }
    if share {
        c.arg("--share");
    }
    c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let display = find_display(&mut c);
    // SAFETY: setsid is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = match c.spawn() {
        Ok(child) => child,
        Err(e) => return fail(&format!("Couldn't start Annex: {e}")),
    };
    println!("Starting Annex{display}…");
    if !wait {
        return glib::ExitCode::SUCCESS;
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(300));
        if let Ok(Some(status)) = child.try_wait() {
            return fail(&format!("Annex stopped as it started ({status}). `annex logs` shows why."));
        }
        if is_running() == Ok(true) {
            println!("Annex is running.");
            return glib::ExitCode::SUCCESS;
        }
    }
    println!("Annex is still starting; `annex status` shows when it's ready.");
    glib::ExitCode::SUCCESS
}

/// From SSH there's no display to start on: use the desktop's, if this user has one running.
fn find_display(c: &mut std::process::Command) -> String {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some() {
        return String::new();
    }
    let runtime = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            // SAFETY: getuid can't fail.
            let d = std::path::PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
            if d.is_dir() {
                c.env("XDG_RUNTIME_DIR", &d);
            }
            d
        }
    };
    for n in 0..4 {
        let w = format!("wayland-{n}");
        if runtime.join(&w).exists() {
            c.env("WAYLAND_DISPLAY", &w);
            return format!(" on the desktop's display ({w})");
        }
    }
    for n in 0..4 {
        if std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() {
            c.env("DISPLAY", format!(":{n}"));
            return format!(" on the desktop's display (:{n})");
        }
    }
    String::new()
}

/// `annex logs`: the end of the log, its path, or following it.
fn logs(path_only: bool, follow: bool, lines: usize) -> glib::ExitCode {
    let path = settings::data_dir().join("annex.log");
    if path_only {
        println!("{}", path.display());
        return glib::ExitCode::SUCCESS;
    }
    if follow {
        use std::os::unix::process::CommandExt;
        let e = std::process::Command::new("tail").arg("-n").arg(lines.to_string()).arg("-F").arg(&path).exec();
        return fail(&format!("Couldn't run tail: {e}"));
    }
    match std::fs::read(&path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let all: Vec<&str> = text.lines().collect();
            for l in &all[all.len().saturating_sub(lines)..] {
                println!("{l}");
            }
            glib::ExitCode::SUCCESS
        }
        Err(e) => fail(&format!("No log at {} ({e}).", path.display())),
    }
}

/// `annex help [command]`.
pub fn help(topic: Option<&str>) -> Result<String, String> {
    match topic {
        None => Ok(cli::HELP.to_string()),
        Some(t) => {
            cli::help_for(t).map(String::from).ok_or_else(|| format!("No command called {t}. `annex help` lists them."))
        }
    }
}

pub fn version_line() -> String {
    format!("Annex {}", version())
}

// ---- Shared by both sides ----------------------------------------------------------------

fn pretty(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn unknown_key(key: &str) -> String {
    format!("No setting called {key}. `annex settings` lists them.")
}

fn settings_list(s: &Settings, json: bool) -> String {
    if json {
        return format!("{}\n", pretty(&settings_keys::to_json(s)));
    }
    let mut out = String::new();
    for k in settings_keys::KEYS {
        out.push_str(&format!("{:<26} {:<30} {}\n", k.name, settings_keys::shown(k, s), k.allowed()));
    }
    out
}

fn get(key: &str, s: &Settings, json: bool) -> Result<String, String> {
    let k: &Key = settings_keys::find(key).ok_or_else(|| unknown_key(key))?;
    Ok(if json { pretty(&json!({ "key": k.name, "value": k.get(s) })) } else { k.get(s) })
}

fn last_connected(t: &TrustedPeer) -> Option<String> {
    (t.last_seen > 0)
        .then(|| glib::DateTime::from_unix_local(t.last_seen as i64).ok())
        .flatten()
        .and_then(|d| d.format("%e %b %Y, %H:%M").ok())
        .map(|d| d.trim().to_string())
}

fn devices_list(list: &[TrustedPeer], headless: bool, json: bool, label: impl Fn(&str) -> String) -> String {
    if json {
        let v: Vec<serde_json::Value> = list
            .iter()
            .map(|t| {
                json!({
                    "name": label(&t.name),
                    "identity": crate::model::display_fingerprint(&t.fingerprint),
                    "fingerprint": t.fingerprint,
                    "last_connected": (t.last_seen > 0).then_some(t.last_seen),
                    "control": t.allow_input || headless,
                    "control_allowed": t.allow_input,
                    "sound": t.allow_sound,
                    "microphone": t.allow_mic,
                })
            })
            .collect();
        return format!("{}\n", pretty(&serde_json::Value::Array(v)));
    }
    if list.is_empty() {
        return "No paired devices. Pair by connecting from another computer, or to one.\n".into();
    }
    let mut out = String::new();
    for t in list {
        let control = match (t.allow_input, headless) {
            (true, _) => "yes",
            (false, true) => "yes (No one at this screen is on)",
            (false, false) => "no",
        };
        out.push_str(&format!(
            "{}\n  Identity code: {}\n  Last connected: {}\n  Can use the mouse and keyboard: {control}\n  \
             Can hear this computer's sound: {}\n  Its microphone plays here: {}\n",
            label(&t.name),
            crate::model::display_fingerprint(&t.fingerprint),
            last_connected(t).unwrap_or_else(|| "never".into()),
            if t.allow_sound { "yes" } else { "no" },
            if t.allow_mic { "yes" } else { "no" },
        ));
    }
    out
}

/// (fingerprint, name) of the paired device `query` names.
fn find_device(list: &[TrustedPeer], query: &str) -> Result<(String, String), String> {
    let pairs: Vec<(String, String)> = list.iter().map(|t| (t.name.clone(), t.fingerprint.clone())).collect();
    cli::match_device(query, &pairs).map(|i| (list[i].fingerprint.clone(), list[i].name.clone()))
}

// ---- In the running Annex ----------------------------------------------------------------

/// Where an answer goes: the terminal that ran `annex`. It gets its exit status, and stops
/// waiting, once every copy is dropped.
#[derive(Clone)]
struct Out(gio::ApplicationCommandLine);

impl Out {
    fn print(&self, text: &str) {
        self.emit(false, text);
    }

    fn line(&self, text: &str) {
        self.emit(false, &format!("{text}\n"));
    }

    fn error(&self, text: &str) {
        self.emit(true, &format!("{text}\n"));
    }

    fn emit(&self, stderr: bool, text: &str) {
        let Ok(text) = std::ffi::CString::new(text.replace('\0', "")) else { return };
        let cmd: *mut gio::ffi::GApplicationCommandLine = self.0.as_ptr();
        // `print_literal` needs GLib 2.80; "%s" is the same, back to GLib 2.28.
        // SAFETY: a live command line, a NUL-terminated format and one string for its %s.
        unsafe {
            if stderr {
                gio::ffi::g_application_command_line_printerr(cmd, c"%s".as_ptr(), text.as_ptr());
            } else {
                gio::ffi::g_application_command_line_print(cmd, c"%s".as_ptr(), text.as_ptr());
            }
        }
    }

    fn status(&self, code: u8) -> glib::ExitCode {
        self.0.set_exit_status(code.into());
        glib::ExitCode::from(code)
    }

    fn ok(&self) -> glib::ExitCode {
        self.status(0)
    }

    fn fail(&self, text: &str) -> glib::ExitCode {
        self.error(text);
        self.status(1)
    }

    fn usage(&self, text: &str) -> glib::ExitCode {
        self.error(text);
        self.status(2)
    }
}

/// Help, the version or bad usage, if they reach the running Annex (`annex` answers them itself).
pub fn reply(cmdline: &gio::ApplicationCommandLine, parsed: Result<Parsed, cli::Usage>) -> glib::ExitCode {
    let out = Out(cmdline.clone());
    match parsed {
        Ok(Parsed::Help(topic)) => match help(topic.as_deref()) {
            Ok(text) => {
                out.print(&text);
                out.ok()
            }
            Err(e) => out.usage(&e),
        },
        Ok(Parsed::Version) => {
            out.line(&version_line());
            out.ok()
        }
        Err(cli::Usage(e)) => out.usage(&e),
        Ok(_) => out.ok(),
    }
}

/// A command from `annex` in a terminal, run here on the main thread.
pub fn run(
    app: &gtk::Application,
    cmdline: &gio::ApplicationCommandLine,
    model: Option<Shared>,
    cmd: Command,
) -> glib::ExitCode {
    let out = Out(cmdline.clone());
    tracing::info!(target: "annex::cli", remote = cmdline.is_remote(), "cli: {}", cmd.describe());
    let Some(model) = model else { return out.fail("Annex is still starting. Try again in a moment.") };
    let model = &model;
    match cmd {
        Command::Status { json } => {
            out.print(&status(&model.borrow(), json));
            out.ok()
        }
        Command::Share(on) => {
            ui::act(app, model, |m| m.set_hosting(on));
            let m = model.borrow();
            match (on, m.is_hosting()) {
                (true, true) => {
                    out.line(&format!(
                        "Sharing this screen{}.",
                        m.host_port.map(|p| format!(" on port {p}")).unwrap_or_default()
                    ));
                    out.ok()
                }
                (true, false) => {
                    out.fail(&format!("Sharing didn't start: {}", m.host_error.as_deref().unwrap_or("no reason given")))
                }
                (false, _) => {
                    out.line("Not sharing this screen.");
                    out.ok()
                }
            }
        }
        Command::Start { headless, share } => {
            out.line("Annex is already running.");
            if headless {
                crate::turn_on_headless(model);
                ui::act(app, model, |m| m.render());
                out.line("No one at this screen: on");
            }
            if share {
                ui::act(app, model, |m| m.set_hosting(true));
                let m = model.borrow();
                if !m.is_hosting() {
                    return out.fail(&format!(
                        "Sharing didn't start: {}",
                        m.host_error.as_deref().unwrap_or("no reason given")
                    ));
                }
                out.line("Sharing this screen.");
            }
            out.ok()
        }
        Command::Quit => {
            out.line("Annex is quitting.");
            // Once this answer is on its way.
            let (app, model) = (app.clone(), model.clone());
            glib::idle_add_local_once(move || crate::quit(&app, &model));
            out.ok()
        }
        Command::Settings { json } => {
            out.print(&settings_list(&model.borrow().settings, json));
            out.ok()
        }
        Command::Get { key, json } => match get(&key, &model.borrow().settings, json) {
            Ok(v) => {
                out.line(&v);
                out.ok()
            }
            Err(e) => out.usage(&e),
        },
        Command::Set { key, value } => set(app, &out, model, &key, value),
        Command::Devices { json } => {
            let m = model.borrow();
            out.print(&devices_list(&m.trusted, m.settings.headless, json, |n| m.label(n)));
            out.ok()
        }
        Command::Allow { what, device, on } => {
            let (fp, name) = match find_device(&model.borrow().trusted, &device) {
                Ok(d) => d,
                Err(e) => return out.fail(&e),
            };
            ui::act(app, model, |m| match what {
                Permission::Control => m.set_allow_input(&fp, on),
                Permission::Sound => m.set_allow_sound(&fp, on),
                Permission::Mic => m.set_allow_mic(&fp, on),
            });
            out.line(&what.sentence(&name, on));
            if what == Permission::Control && !on && model.borrow().settings.headless {
                out.line("No one at this screen is on, so every paired device can anyway (`annex set headless off`).");
            }
            out.ok()
        }
        Command::Forget { device, yes } => {
            if !yes {
                return out.usage("Add --yes to forget a device.");
            }
            let (fp, name) = match find_device(&model.borrow().trusted, &device) {
                Ok(d) => d,
                Err(e) => return out.fail(&e),
            };
            ui::act(app, model, |m| m.forget(&fp));
            out.line(&format!("Forgot {name}. It needs a PIN to connect again."));
            out.ok()
        }
        Command::Nearby { json } => {
            out.print(&nearby(&model.borrow(), json));
            out.ok()
        }
        Command::Connect { to, extend } => connect(app, out, model, &to, extend),
        Command::Disconnect => {
            if model.borrow().viewer.is_none() {
                out.line("This computer isn't viewing another one.");
                return out.ok();
            }
            let peer = {
                let m = model.borrow();
                m.label(&m.viewer_peer)
            };
            ui::act(app, model, |m| m.disconnect());
            out.line(&format!("Disconnected from {peer}."));
            out.ok()
        }
        Command::Code => code(app, out, model),
        Command::CodeStop => {
            if model.borrow().internet.is_none() {
                out.line("No code is in use.");
                return out.ok();
            }
            ui::act(app, model, |m| m.stop_code());
            out.line("The code is stopped.");
            out.ok()
        }
        Command::Pin => {
            let m = model.borrow();
            if let Some(p) = m.pin_shown.as_ref().filter(|p| p.at.elapsed() < Duration::from_secs(120)) {
                // The user asked for it: the one place a PIN is printed. Never logged.
                out.line(&p.pin);
                out.line(&format!(
                    "Type it on {} to pair. It works once, for the next 2 minutes.",
                    m.label(&p.peer_name)
                ));
                out.ok()
            } else if let Some(PinAsked { peer_name, .. }) = &m.pin_asked {
                out.line(&format!("{} shows a PIN: type it with `annex pin enter`.", m.label(peer_name)));
                out.ok()
            } else {
                out.fail("No computer is pairing with this one.")
            }
        }
        Command::PinEnter { pin } => pin_enter(out, model, pin),
        Command::Send { files } => send(&out, model, &files),
        Command::Transfers { json } => {
            out.print(&transfers(&model.borrow(), json));
            out.ok()
        }
        Command::Pause(n) => transfer_action(app, &out, model, n, Action::Pause),
        Command::Resume(n) => transfer_action(app, &out, model, n, Action::Resume),
        Command::Unsend(n) => transfer_action(app, &out, model, n, Action::Unsend),
        Command::UpdateCheck { json } => update_check(app, out, model, json),
        Command::Logs { path: true, .. } => {
            out.line(&settings::data_dir().join("annex.log").display().to_string());
            out.ok()
        }
        Command::Logs { .. } => out.usage("`annex logs` runs in the terminal, not in Annex."),
    }
}

/// Checks `done` every 200 ms until it answers (true: done, false: failed) or `secs` pass (then
/// `late` answers). The terminal waits meanwhile: it has its answer once `out` is dropped.
fn wait_for<D, L>(model: &Shared, out: Out, secs: u64, done: D, late: L) -> glib::ExitCode
where
    D: Fn(&Model, &Out) -> Option<bool> + 'static,
    L: FnOnce(&Model, &Out) + 'static,
{
    let model = model.clone();
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut late = Some(late);
    glib::timeout_add_local(Duration::from_millis(200), move || {
        let Ok(guard) = model.try_borrow() else { return glib::ControlFlow::Continue };
        let m: &Model = &guard;
        match done(m, &out) {
            Some(ok) => {
                out.0.set_exit_status(if ok { 0 } else { 1 });
                glib::ControlFlow::Break
            }
            None if Instant::now() >= deadline => {
                if let Some(late) = late.take() {
                    late(m, &out);
                }
                out.0.set_exit_status(0);
                glib::ControlFlow::Break
            }
            None => glib::ControlFlow::Continue,
        }
    });
    glib::ExitCode::SUCCESS
}

fn set(app: &gtk::Application, out: &Out, model: &Shared, key: &str, value: Option<String>) -> glib::ExitCode {
    let Some(k) = settings_keys::find(key) else { return out.usage(&unknown_key(key)) };
    let Some(value) = value else { return out.usage(&format!("`annex set {key}` needs a value.")) };
    let mut s = model.borrow().settings.clone();
    if let Err(e) = k.set(&mut s, &value) {
        return out.usage(&e);
    }
    // As the Settings page does it: the setting, then what changes with it, then it's saved.
    let mut result = Ok(());
    ui::act(app, model, |m| {
        m.settings = s;
        result = m.after_setting(k.effect);
    });
    if let Err(e) = result {
        return out.fail(&e);
    }
    let m = model.borrow();
    out.line(&format!("{}: {}", k.name, settings_keys::shown(k, &m.settings)));
    if m.is_hosting() && matches!(k.name, "hardware-encoder" | "extend-scale") {
        out.line("It applies the next time sharing turns on (`annex share off`, then `annex share on`).");
    }
    out.ok()
}

fn mode(extend: bool) -> &'static str {
    if extend { "Extend" } else { "Mirror" }
}

fn status(m: &Model, json: bool) -> String {
    let code_left = match &m.internet {
        Some(InternetShare::Code { expires, .. }) => Some(expires.saturating_duration_since(Instant::now()).as_secs()),
        _ => None,
    };
    let viewing = m.role().and_then(|r| match r {
        SessionRole::Viewing { from, extend } => Some((m.label(&from), extend)),
        SessionRole::Sharing { .. } => None,
    });
    let sharing_peers = m.peers().iter().filter(|p| p.sharing).count();
    if json {
        let update = match &m.update {
            UpdateState::Idle => json!({ "state": "not checked" }),
            UpdateState::Checking => json!({ "state": "checking" }),
            UpdateState::UpToDate => json!({ "state": "up to date" }),
            UpdateState::Available(u) => json!({ "state": "available", "version": u.version, "url": u.page_url }),
            UpdateState::Failed(e) => json!({ "state": "failed", "error": e }),
        };
        let mut update = update;
        if let Some(s) = &m.update_staged {
            update["downloaded"] = json!(s.version);
        }
        let v = json!({
            "running": true,
            "version": version(),
            "name": m.device_name(),
            "identity": m.fingerprint_display(),
            "headless": m.settings.headless,
            "sharing": m.is_hosting(),
            "port": m.host_port,
            "sharing_error": m.host_error,
            "sessions": m.host_sessions.iter().map(|s| json!({
                "with": m.label(&s.name),
                "mode": mode(s.extend),
                "controlling": s.input,
            })).collect::<Vec<_>>(),
            "viewing": m.viewer.as_ref().map(|_| json!({
                "peer": m.label(&m.viewer_peer),
                "connected": viewing.is_some(),
                "mode": m.viewer_extends.map(mode),
                "controlling": m.controlling,
                "route": m.viewer_route,
                "status": m.viewer_status,
            })),
            "code_seconds_left": code_left,
            "pin_waiting": m.pin_shown.as_ref().is_some_and(|p| p.at.elapsed() < Duration::from_secs(120)),
            "pin_asked_by": m.pin_asked.as_ref().map(|p| m.label(&p.peer_name)),
            "update": update,
            "nearby": m.peers().len(),
            "nearby_sharing": sharing_peers,
            "paired": m.trusted.len(),
        });
        return format!("{}\n", pretty(&v));
    }
    let mut o = format!("Annex {} is running.\n", version());
    o += &format!("This computer: {} (identity code {})\n", m.device_name(), m.fingerprint_display());
    o += &format!("No one at this screen: {}\n", on_off(m.settings.headless));
    match (m.is_hosting(), m.host_port) {
        (true, Some(p)) => o += &format!("Sharing: on, port {p}\n"),
        (true, None) => o += "Sharing: on\n",
        (false, _) => o += "Sharing: off\n",
    }
    for s in &m.host_sessions {
        o += &format!(
            "  {}: {}, {}\n",
            m.label(&s.name),
            mode(s.extend),
            if s.input { "controlling this computer" } else { "watching" }
        );
    }
    if let Some(e) = &m.host_error {
        o += &format!("Sharing problem: {e}\n");
    }
    match (&viewing, &m.viewer) {
        (Some((from, extend)), _) => {
            let route = if m.viewer_route.is_empty() { String::new() } else { format!(" · {}", m.viewer_route) };
            o += &format!(
                "Viewing: {from} ({}, {}){route}\n",
                mode(*extend),
                if m.controlling { "controlling it" } else { "watching" }
            );
        }
        (None, Some(_)) => {
            o += &format!("Viewing: {}\n", m.viewer_status.as_deref().unwrap_or("connecting…"));
        }
        (None, None) => o += "Viewing: off\n",
    }
    if let Some(left) = code_left {
        o += &format!("Internet code: in use, {}:{:02} left (`annex code` shows it)\n", left / 60, left % 60);
    }
    if m.pin_shown.as_ref().is_some_and(|p| p.at.elapsed() < Duration::from_secs(120)) {
        o += "Pairing: a computer is waiting for its PIN (`annex pin` shows it)\n";
    }
    if let Some(p) = &m.pin_asked {
        o += &format!("Pairing: {} shows a PIN (`annex pin enter`)\n", m.label(&p.peer_name));
    }
    o += &match &m.update {
        UpdateState::Idle => "Updates: not checked yet\n".to_string(),
        UpdateState::Checking => "Updates: checking…\n".to_string(),
        UpdateState::UpToDate => "Updates: this is the latest version\n".to_string(),
        UpdateState::Available(u) => format!("Update: {} ({})\n", UpdateState::headline(u), u.page_url),
        UpdateState::Failed(e) => format!("Updates: couldn't check ({e})\n"),
    };
    if let Some(s) = &m.update_staged {
        o += &format!("Update: Annex {} is downloaded; it installs once no one is connected\n", s.version);
    }
    o += &format!("Nearby: {} computers, {sharing_peers} sharing\n", m.peers().len());
    o += &format!("Paired devices: {}\n", m.trusted.len());
    o
}

/// How a computer nearby is reached.
fn link(p: &annex_discovery::PeerInfo) -> &'static str {
    if p.mesh {
        "private network"
    } else if p.addresses.iter().any(|a| a.starts_with("169.254.") || a.to_ascii_lowercase().starts_with("fe80")) {
        "cable"
    } else {
        "network"
    }
}

fn nearby(m: &Model, json: bool) -> String {
    let peers = m.peers();
    if json {
        let v: Vec<serde_json::Value> = peers
            .iter()
            .map(|p| {
                json!({
                    "name": m.label(&p.name),
                    "sharing": p.sharing,
                    "can_extend": p.extend,
                    "link": link(p),
                    "address": crate::model::connect_address(p).unwrap_or_default(),
                    "paired": m.is_paired(p),
                })
            })
            .collect();
        return format!("{}\n", pretty(&serde_json::Value::Array(v)));
    }
    if peers.is_empty() {
        return "No computers with Annex open were found nearby yet.\n".into();
    }
    let mut o = String::new();
    for p in &peers {
        let mut what = vec![if p.sharing { "sharing" } else { "Annex open, not sharing" }];
        if p.extend == Some(true) {
            what.push("can be extended to");
        }
        if m.is_paired(p) {
            what.push("paired");
        }
        o += &format!(
            "{}\n  {}\n  Link: {}, {}\n",
            m.label(&p.name),
            what.join(", "),
            link(p),
            crate::model::connect_address(p).unwrap_or_default()
        );
    }
    o
}

fn connect(app: &gtk::Application, out: Out, model: &Shared, to: &str, extend: Option<bool>) -> glib::ExitCode {
    let to = to.trim().to_string();
    let extend = extend.unwrap_or(model.borrow().settings.extend_by_default);
    let display = crate::viewer::display_info(None::<&gtk::Widget>);
    // A computer nearby, by name; else a code or an address.
    let peer = model.borrow().peers().into_iter().find(|p| p.name.trim().eq_ignore_ascii_case(&to));
    ui::act(app, model, |m| {
        match &peer {
            Some(p) => m.connect_peer(p, extend, display),
            None if cli::looks_like_code(&to) => m.connect_code(&to, extend, display),
            None => m.connect_address(&to, extend, display),
        }
        m.cli_viewer = true;
    });
    wait_for(
        model,
        out,
        20,
        |m, out| {
            if m.viewer_extends.is_some() {
                out.line(&format!("{}.", m.viewer_status.as_deref().unwrap_or("Connected")));
                Some(true)
            } else if let Some(p) = &m.pin_asked {
                out.line(&format!(
                    "{} asks for the PIN it shows (its identity code: {}). Type it with: annex pin enter",
                    m.label(&p.peer_name),
                    p.fingerprint
                ));
                Some(true)
            } else if m.viewer.is_none() {
                out.error(m.viewer_status.as_deref().unwrap_or("Couldn't connect."));
                Some(false)
            } else {
                None
            }
        },
        |m, out| {
            out.line(&format!(
                "{} `annex status` shows when it's done.",
                m.viewer_status.as_deref().unwrap_or("Still connecting.")
            ))
        },
    )
}

fn pin_enter(out: Out, model: &Shared, pin: Option<String>) -> glib::ExitCode {
    let Some(pin) = pin.filter(|p| !p.is_empty()) else { return out.usage("No PIN was typed.") };
    let Some(asked) = model.borrow().pin_asked.clone() else {
        return out.fail("No computer is asking this one for a PIN. Connect first: annex connect <computer>.");
    };
    model.borrow().submit_pin(asked.request_id, &pin);
    let request = asked.request_id;
    wait_for(
        model,
        out,
        20,
        move |m, out| {
            if m.viewer_extends.is_some() {
                out.line(&format!("Paired. {}.", m.viewer_status.as_deref().unwrap_or("Connected")));
                Some(true)
            } else if m.viewer.is_none() {
                out.error(m.viewer_status.as_deref().unwrap_or("Pairing didn't finish."));
                Some(false)
            } else if m.pin_asked.as_ref().is_some_and(|p| p.request_id != request) {
                out.error("That PIN didn't work. Type the new one with `annex pin enter`.");
                Some(false)
            } else {
                None
            }
        },
        |_, out| out.line("PIN sent; still connecting. `annex status` shows when it's done."),
    )
}

fn code(app: &gtk::Application, out: Out, model: &Shared) -> glib::ExitCode {
    if !model.borrow().is_hosting() {
        ui::act(app, model, |m| m.set_hosting(true));
        let m = model.borrow();
        if !m.is_hosting() {
            return out
                .fail(&format!("Sharing didn't start: {}", m.host_error.as_deref().unwrap_or("no reason given")));
        }
        out.line("Sharing this screen.");
    }
    let has_code = matches!(model.borrow().internet, Some(InternetShare::Code { .. }));
    if !has_code {
        ui::act(app, model, |m| m.share_code());
    }
    wait_for(
        model,
        out,
        20,
        |m, out| match &m.internet {
            Some(InternetShare::Starting) => None,
            Some(InternetShare::Code { code, expires }) => {
                // The user asked for it; never logged.
                let left = expires.saturating_duration_since(Instant::now()).as_secs();
                out.line(code);
                out.line(&format!(
                    "Type it on the other computer (`annex connect {code}`, or Nearby → Connect by address or code). \
                     It works once, for {}:{:02}.",
                    left / 60,
                    left % 60
                ));
                Some(true)
            }
            Some(InternetShare::Joining(name) | InternetShare::Relay { name, .. }) => {
                out.line(&format!("{} is connecting with the code.", m.label(name)));
                Some(true)
            }
            Some(InternetShare::Ended(why)) => {
                out.error(why.as_deref().unwrap_or("The code was used."));
                Some(false)
            }
            None => {
                out.error("The code stopped.");
                Some(false)
            }
        },
        |_, out| out.line("Still getting a code; run `annex code` again in a moment."),
    )
}

fn send(out: &Out, model: &Shared, files: &[String]) -> glib::ExitCode {
    let mut paths = Vec::new();
    for f in files {
        // Relative to where `annex` ran, not to the running Annex.
        let Some(p) = out.0.create_file_for_arg(f).path() else {
            return out.fail(&format!("{f} isn't a file on this computer."));
        };
        if !p.exists() {
            return out.fail(&format!("{} doesn't exist.", p.display()));
        }
        let found = crate::drop_tray::files_in(&p);
        if found.is_empty() {
            out.error(&format!("Nothing to send in {}.", p.display()));
        }
        paths.extend(found);
    }
    let m = model.borrow();
    tracing::info!(target: "annex::transfer", count = paths.len(), to = m.files_target(), "files: sending from the command line");
    let mut failed = 0;
    for p in &paths {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let size = std::fs::metadata(p).map(|md| md.len()).unwrap_or(0);
        match m.send_file(p) {
            Ok(()) => out.line(&format!("Sending {name} ({}).", cli::size(size))),
            Err(e) => {
                failed += 1;
                out.error(&format!("Couldn't send {name}: {e}"));
            }
        }
    }
    if failed > 0 || paths.is_empty() {
        return out.status(1);
    }
    out.line("`annex transfers` shows how they're going.");
    out.ok()
}

fn transfers(m: &Model, json: bool) -> String {
    if json {
        let v: Vec<serde_json::Value> = m
            .transfers
            .iter()
            .map(|t| {
                json!({
                    "number": t.number,
                    "direction": if t.key.incoming { "receiving" } else { "sending" },
                    "name": t.name,
                    "size": t.size,
                    "done": t.done,
                    "paused": t.paused,
                    "state": match &t.ended {
                        None if t.paused => "paused",
                        None => "going",
                        Some(Ok(())) => "done",
                        Some(Err(_)) => "failed",
                    },
                    "error": t.ended.as_ref().and_then(|r| r.as_ref().err()),
                })
            })
            .collect();
        return format!("{}\n", pretty(&serde_json::Value::Array(v)));
    }
    if m.transfers.is_empty() {
        return "No files sent or received yet.\n".into();
    }
    let mut o = String::new();
    for t in &m.transfers {
        let percent = (t.done.min(t.size) * 100).checked_div(t.size).map_or(100, |p| p as u32);
        let state = match &t.ended {
            Some(Ok(())) => format!("done, {}", cli::size(t.size)),
            Some(Err(e)) => format!("stopped: {e}"),
            None if t.paused => format!("paused at {percent}% of {}", cli::size(t.size)),
            None => format!("{percent}% of {}", cli::size(t.size)),
        };
        let dir = if t.key.incoming { "receiving" } else { "sending  " };
        o += &format!("{:>3}  {dir}  {}  ({state})\n", t.number, t.name);
    }
    o
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Pause,
    Resume,
    Unsend,
}

fn transfer_action(app: &gtk::Application, out: &Out, model: &Shared, number: u64, action: Action) -> glib::ExitCode {
    let Some(t) = model.borrow().transfers.iter().find(|t| t.number == number).cloned() else {
        return out.fail(&format!("No transfer {number}. `annex transfers` lists them."));
    };
    if t.ended.is_some() {
        return out.fail(&format!("{} has already finished.", t.name));
    }
    if t.key.incoming && action != Action::Unsend {
        return out.fail("Only files being sent can be paused; `annex unsend` stops one arriving.");
    }
    let mut done = false;
    ui::act(app, model, |m| {
        done = match action {
            Action::Pause => m.pause_transfer(t.key, true),
            Action::Resume => m.pause_transfer(t.key, false),
            Action::Unsend => m.cancel_transfer(t.key),
        };
    });
    if !done {
        return out.fail(&format!("Couldn't: {} isn't being sent any more.", t.name));
    }
    out.line(&match action {
        Action::Pause => format!("Paused {}.", t.name),
        Action::Resume => format!("Resumed {}.", t.name),
        Action::Unsend if t.key.incoming => format!("Stopped receiving {}.", t.name),
        Action::Unsend => format!("Unsent {}: the other computer deletes what arrived.", t.name),
    });
    out.ok()
}

fn update_check(app: &gtk::Application, out: Out, model: &Shared, json: bool) -> glib::ExitCode {
    ui::act(app, model, |m| {
        m.update_dismissed = None;
        m.check_for_updates();
    });
    wait_for(
        model,
        out,
        40,
        move |m, out| {
            let (v, ok) = match &m.update {
                UpdateState::Idle | UpdateState::Checking => return None,
                UpdateState::UpToDate => (json!({ "available": false, "current": version() }), true),
                UpdateState::Available(u) => (
                    json!({
                        "available": true,
                        "current": version(),
                        "version": u.version,
                        "prerelease": u.prerelease,
                        "url": u.page_url,
                    }),
                    true,
                ),
                UpdateState::Failed(e) => (json!({ "error": e }), false),
            };
            if json {
                out.line(&pretty(&v));
            } else {
                match &m.update {
                    UpdateState::Available(u) => {
                        out.line(&UpdateState::headline(u));
                        out.line(&u.page_url);
                        out.line("Install it with the package manager (.deb or .rpm).");
                    }
                    UpdateState::Failed(e) => out.error(&format!("Couldn't check for updates: {e}")),
                    _ => out.line(&format!("Annex {} is the latest version.", version())),
                }
            }
            Some(ok)
        },
        |_, out| out.line("Still checking; `annex update check` again in a minute shows the answer."),
    )
}
