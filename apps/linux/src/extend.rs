//! Extend on wlroots compositors: a headless output for the viewer's screen.
//!
//! - sway: `swaymsg create_output`, then `output … mode` and `unplug`.
//! - labwc: no IPC. Its `VirtualOutputAdd`/`VirtualOutputRemove` actions only run from key
//!   bindings, so Annex adds two to `~/.config/labwc/rc.xml` (labwc merges it with the system
//!   config), reloads labwc, and presses them with a virtual keyboard.
//!
//! The size is set with `wlr-randr` (Raspberry Pi OS has it).

use std::process::Command;
use std::time::{Duration, Instant};

use crate::desktop::Wlr;
use crate::wl::Wayland;

/// labwc's output name for Annex's display.
const LABWC_OUTPUT: &str = "ANNEX-1";
const MARKER: &str = "<!-- Annex: Extend -->";
// Super+Ctrl+Alt+Shift+F11 / F12: nobody presses these by hand.
const ADD_KEY: &str = "W-C-A-S-F11";
const REMOVE_KEY: &str = "W-C-A-S-F12";
const CHORD: [u16; 4] = [0xE3, 0xE0, 0xE2, 0xE1];
const F11: u16 = 0x44;
const F12: u16 = 0x45;

pub struct Display {
    pub output: String,
    kind: Wlr,
}

fn output_names() -> Result<Vec<String>, String> {
    Ok(Wayland::connect()?.state.outputs.iter().map(|o| o.name.clone()).collect())
}

/// Waits for an output that wasn't in `before` (or is called `name`).
fn wait_for_output(before: &[String], name: Option<&str>) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let now = output_names()?;
        if let Some(n) = now.iter().find(|n| name.map_or(!before.contains(n), |want| *n == want)) {
            return Ok(n.clone());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("the desktop didn't add a display".into())
}

pub fn supported(kind: Wlr) -> bool {
    match kind {
        Wlr::Sway => which("swaymsg"),
        Wlr::Labwc => true,
        Wlr::Other => false,
    }
}

fn which(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// Adds a display `w`×`h` at `hz`, to the right of the others.
pub fn add(kind: Wlr, w: u32, h: u32, hz: u32) -> Result<Display, String> {
    let before = output_names()?;
    let output = match kind {
        Wlr::Sway => {
            run("swaymsg", &["create_output"])?;
            wait_for_output(&before, None)?
        }
        Wlr::Labwc => {
            install_labwc_bindings()?;
            let mut input = crate::inject::Wlr::new(None)?;
            let mut keys = CHORD.to_vec();
            keys.push(F11);
            input.chord(&keys)?;
            wait_for_output(&before, Some(LABWC_OUTPUT))?
        }
        Wlr::Other => return Err("This desktop can't add a display yet.".into()),
    };
    let d = Display { output, kind };
    d.set_mode(w, h, hz);
    Ok(d)
}

impl Display {
    fn set_mode(&self, w: u32, h: u32, hz: u32) {
        let r = match self.kind {
            Wlr::Sway => run("swaymsg", &["output", &self.output, "mode", "--custom", &format!("{w}x{h}@{hz}Hz")]),
            _ if which("wlr-randr") => {
                run("wlr-randr", &["--output", &self.output, "--custom-mode", &format!("{w}x{h}@{hz}")])
            }
            _ => Err("wlr-randr isn't installed".into()),
        };
        if let Err(e) = r {
            tracing::warn!("extend: couldn't size the display: {e}");
        }
    }

    pub fn remove(&self) {
        let r = match self.kind {
            Wlr::Sway => run("swaymsg", &["output", &self.output, "unplug"]),
            _ => crate::inject::Wlr::new(None).and_then(|mut i| {
                let mut keys = CHORD.to_vec();
                keys.push(F12);
                i.chord(&keys)
            }),
        };
        if let Err(e) = r {
            tracing::warn!("extend: couldn't remove the display: {e}");
        }
    }
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{cmd} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Adds Annex's two bindings to the user's labwc config once, then reloads labwc.
fn install_labwc_bindings() -> Result<(), String> {
    let path = crate::settings::config_dir().parent().map(|p| p.join("labwc/rc.xml")).ok_or("no config folder")?;
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current.contains(MARKER) {
        return Ok(());
    }
    let block = format!(
        "  {MARKER}\n  <keyboard>\n    <keybind key=\"{ADD_KEY}\"><action name=\"VirtualOutputAdd\" output_name=\"{LABWC_OUTPUT}\" /></keybind>\n    <keybind key=\"{REMOVE_KEY}\"><action name=\"VirtualOutputRemove\" output_name=\"{LABWC_OUTPUT}\" /></keybind>\n  </keyboard>\n"
    );
    let updated = if let Some(end) = current.rfind("</labwc_config>") {
        format!("{}{block}{}", &current[..end], &current[end..])
    } else if current.trim().is_empty() {
        format!("<?xml version=\"1.0\"?>\n<labwc_config>\n{block}</labwc_config>\n")
    } else {
        return Err(format!("{} isn't a labwc config Annex understands", path.display()));
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, updated).map_err(|e| e.to_string())?;
    tracing::info!("extend: added Annex's display bindings to {}", path.display());
    // labwc reloads its config on SIGHUP.
    run("labwc", &["--reconfigure"]).or_else(|_| match std::env::var("LABWC_PID") {
        Ok(pid) => run("kill", &["-HUP", &pid]),
        Err(_) => Err("couldn't reload labwc".into()),
    })?;
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}
