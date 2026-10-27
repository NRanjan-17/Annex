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
            let fresh = install_labwc_bindings()?;
            let press = || -> Result<String, String> {
                let mut input = crate::inject::Wlr::new(None)?;
                let mut keys = CHORD.to_vec();
                keys.push(F11);
                input.chord(&keys)?;
                wait_for_output(&before, Some(LABWC_OUTPUT))
            };
            match press() {
                Ok(o) => o,
                // Bindings added earlier but labwc not reloaded since (it was restarted with
                // another config, or the reload was missed): reload and try again.
                Err(_) if !fresh => {
                    reload_labwc()?;
                    press()?
                }
                Err(e) => return Err(e),
            }
        }
        Wlr::Other => return Err("This desktop can't add a display yet.".into()),
    };
    let d = Display { output, kind };
    d.set_mode(w, h, hz);
    Ok(d)
}

impl Display {
    /// Sizes the display and puts it to the right of the others (new outputs start at 0,0,
    /// on top of the main screen).
    fn set_mode(&self, w: u32, h: u32, hz: u32) {
        let x = self.right_edge().unwrap_or(0).to_string();
        let r = match self.kind {
            Wlr::Sway => run("swaymsg", &["output", &self.output, "mode", "--custom", &format!("{w}x{h}@{hz}Hz")])
                .and_then(|_| run("swaymsg", &["output", &self.output, "pos", &x, "0"])),
            _ if which("wlr-randr") => run(
                "wlr-randr",
                &["--output", &self.output, "--custom-mode", &format!("{w}x{h}@{hz}"), "--pos", &format!("{x},0")],
            ),
            _ => Err("wlr-randr isn't installed".into()),
        };
        if let Err(e) = r {
            tracing::warn!("extend: couldn't size or place the display: {e}");
        }
    }

    /// Where the other outputs end on the right, in layout coordinates.
    fn right_edge(&self) -> Option<i64> {
        let others = |name: &serde_json::Value| name.as_str() != Some(self.output.as_str());
        let json = |cmd: &str, args: &[&str]| -> Option<serde_json::Value> {
            let out = Command::new(cmd).args(args).output().ok()?;
            serde_json::from_slice(&out.stdout).ok()
        };
        match self.kind {
            Wlr::Sway => json("swaymsg", &["-r", "-t", "get_outputs"])?
                .as_array()?
                .iter()
                .filter(|o| others(&o["name"]) && o["active"].as_bool() == Some(true))
                .filter_map(|o| Some(o["rect"]["x"].as_i64()? + o["rect"]["width"].as_i64()?))
                .max(),
            _ => json("wlr-randr", &["--json"])?
                .as_array()?
                .iter()
                .filter(|o| others(&o["name"]) && o["enabled"].as_bool() == Some(true))
                .filter_map(|o| {
                    let mode = o["modes"].as_array()?.iter().find(|m| m["current"].as_bool() == Some(true))?;
                    let scale = o["scale"].as_f64().unwrap_or(1.0).max(0.1);
                    let w = mode["width"].as_f64()?;
                    let rotated = o["transform"].as_str().is_some_and(|t| t.contains("90") || t.contains("270"));
                    let w = if rotated { mode["height"].as_f64()? } else { w };
                    Some(o["position"]["x"].as_i64()? + (w / scale).round() as i64)
                })
                .max(),
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

/// Adds Annex's two bindings to the user's labwc config once, then reloads labwc. True when it
/// added them now.
fn install_labwc_bindings() -> Result<bool, String> {
    let path = crate::settings::config_dir().parent().map(|p| p.join("labwc/rc.xml")).ok_or("no config folder")?;
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current.contains(MARKER) {
        return Ok(false);
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
    reload_labwc()?;
    Ok(true)
}

/// labwc reloads its config on SIGHUP (what `labwc --reconfigure` sends).
fn reload_labwc() -> Result<(), String> {
    let pid = std::env::var("LABWC_PID")
        .ok()
        .and_then(|p| p.parse::<u32>().ok())
        .or_else(|| crate::desktop::process("labwc"))
        .ok_or("couldn't find labwc to reload it")?;
    run("kill", &["-HUP", &pid.to_string()])?;
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}
