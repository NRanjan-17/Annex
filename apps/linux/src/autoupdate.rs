//! Updates with No one at this screen on: the update is downloaded and verified as soon as the
//! daily check finds it, then installed once no one is connected, and Annex starts again (sharing
//! if it was). Two kinds of install:
//!
//! - A folder (the .tar.gz unpacked anywhere): the new version is unpacked beside this one.
//! - A package (/usr/bin/annex): annex-update-helper installs it as root through pkexec, and keeps
//!   this version's package to put back.
//!
//! A watchdog script starts the new version and waits for it to write its version to `update-ok`
//! (it does once it's up and sharing). If it doesn't within [`CONFIRM_WITHIN`] seconds, the old
//! version comes back and that release is never installed unattended again.

use std::path::{Path, PathBuf};

use annex_update::Update;

use crate::settings;

/// The new version must be running (and sharing, if it was) this soon.
pub const CONFIRM_WITHIN: u32 = 90;
/// No one connected this long before installing, so a viewer switching mirror and extend stays.
pub const IDLE_BEFORE_INSTALL: std::time::Duration = std::time::Duration::from_secs(60);
const HELPER: &str = "/usr/lib/annex/annex-update-helper";
const PKEXEC: &str = "/usr/bin/pkexec";

/// How this copy of Annex was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// The .deb or .rpm: /usr/bin/annex.
    Package,
    /// The .tar.gz, unpacked at this folder (it holds bin/annex).
    Folder(PathBuf),
}

/// The running binary, even after a package update replaced it (Linux then adds " (deleted)").
fn this_exe() -> Option<PathBuf> {
    let link = std::fs::read_link("/proc/self/exe").ok()?;
    let s = link.to_str()?;
    Some(PathBuf::from(s.strip_suffix(" (deleted)").unwrap_or(s)))
}

pub fn install() -> Option<Install> {
    let exe = this_exe()?;
    if exe == Path::new("/usr/bin/annex") {
        return Some(Install::Package);
    }
    let root = exe.parent().filter(|b| b.ends_with("bin"))?.parent()?;
    root.join("lib/annex").is_dir().then(|| Install::Folder(root.to_path_buf()))
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        a => a,
    }
}

/// `deb` or `rpm`: what this system installs.
pub fn package_kind() -> &'static str {
    if !Path::new("/usr/bin/dpkg").exists() && Path::new("/usr/bin/rpm").exists() { "rpm" } else { "deb" }
}

/// The release file for this install: `-linux-<cpu>.tar.gz`, `.deb` or `.rpm`.
pub fn asset_suffix(install: Option<&Install>) -> String {
    match install {
        Some(Install::Folder(_)) => format!("-linux-{}.tar.gz", arch()),
        _ => format!("-linux-{}.{}", arch(), package_kind()),
    }
}

/// An update downloaded, verified and ready to switch to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub version: String,
    /// Where its files are (removed once it's installed or dropped).
    dir: PathBuf,
    kind: StagedKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StagedKind {
    /// Unpacked: the new folder's bin/annex.
    Folder { new_exe: PathBuf, old_exe: PathBuf },
    /// The package and checksums for the helper, and this version's, to put back.
    Package { files: [PathBuf; 3], previous: [PathBuf; 3] },
}

fn marker(name: &str) -> PathBuf {
    settings::data_dir().join(name)
}

fn read_marker(name: &str) -> Option<String> {
    std::fs::read_to_string(marker(name)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The release the watchdog had to undo here: never installed unattended again.
pub fn failed_version() -> Option<String> {
    read_marker("update-failed")
}

fn curl(url: &str, to: &Path) -> Result<(), String> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "600", "-A"])
        .arg(format!("Annex/{}", annex_update::whats_new::CURRENT))
        .arg("-o")
        .arg(to)
        .arg(url)
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("download failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Downloads `u` (and its signed checksums) into `dir` and verifies it. Returns the three files.
fn fetch(u: &Update, dir: &Path) -> Result<[PathBuf; 3], String> {
    let name = Path::new(&u.asset_name).file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if name.is_empty() || name != u.asset_name {
        return Err("the update has an unexpected file name".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let files = [dir.join(name), dir.join(annex_update::SUMS_NAME), dir.join(annex_update::SIG_NAME)];
    curl(&u.asset_url, &files[0])?;
    curl(&u.sums_url, &files[1])?;
    curl(&u.sig_url, &files[2])?;
    let sums = std::fs::read(&files[1]).map_err(|e| e.to_string())?;
    let sig = std::fs::read(&files[2]).map_err(|e| e.to_string())?;
    annex_update::verify(&sums, &sig, &files[0], name).map_err(|e| e.to_string())?;
    Ok(files)
}

/// Downloads and verifies `u` and gets it ready to switch to (in a background thread: it blocks).
/// `releases` is the list the check read: it has this version's package, to put back.
pub fn prepare(u: &Update, install: &Install, releases: &str) -> Result<Staged, String> {
    let dir = std::env::temp_dir().join(format!("annex-update-{}", u.version));
    let _ = std::fs::remove_dir_all(&dir);
    match stage(u, install, releases, &dir) {
        Ok(kind) => Ok(Staged { version: u.version.clone(), dir, kind }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

fn stage(u: &Update, install: &Install, releases: &str, dir: &Path) -> Result<StagedKind, String> {
    let current = annex_update::whats_new::CURRENT;
    match install {
        Install::Folder(root) => {
            let parent = root.parent().ok_or("the install folder has no parent")?;
            let files = fetch(u, dir)?;
            // Unpacked next to this one, under a temporary name until it's complete.
            let unpack = parent.join(format!(".annex-update-{}", u.version));
            let _ = std::fs::remove_dir_all(&unpack);
            std::fs::create_dir_all(&unpack).map_err(|e| format!("{}: {e}", parent.display()))?;
            let status = std::process::Command::new("tar")
                .arg("-xzf")
                .arg(&files[0])
                .arg("-C")
                .arg(&unpack)
                .status()
                .map_err(|e| format!("couldn't run tar: {e}"))?;
            if !status.success() {
                return Err(format!("tar failed ({status})"));
            }
            let top = format!("Annex-{}-linux-{}", u.version, arch());
            let new_root = parent.join(&top);
            if new_root == *root {
                return Err("the update would replace the running copy".into());
            }
            let _ = std::fs::remove_dir_all(&new_root);
            std::fs::rename(unpack.join(&top), &new_root).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_dir_all(&unpack);
            let new_exe = new_root.join("bin/annex");
            if !new_exe.is_file() {
                return Err(format!("{} isn't in the update", new_exe.display()));
            }
            Ok(StagedKind::Folder { new_exe, old_exe: root.join("bin/annex") })
        }
        Install::Package => {
            if !Path::new(PKEXEC).exists() || !Path::new(HELPER).exists() {
                return Err("pkexec or annex-update-helper isn't installed".into());
            }
            let previous = annex_update::find(releases, current, &asset_suffix(Some(install)))
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("{current} isn't published, so it couldn't be put back"))?;
            let files = fetch(u, &dir.join("new"))?;
            let previous = fetch(&previous, &dir.join("previous"))?;
            Ok(StagedKind::Package { files, previous })
        }
    }
}

/// Drops a staged update that won't be installed.
pub fn discard(s: &Staged) {
    let _ = std::fs::remove_dir_all(&s.dir);
    if let StagedKind::Folder { new_exe, .. } = &s.kind
        && let Some(root) = new_exe.parent().and_then(Path::parent)
    {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Single-quoted for the shell.
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Symlinks in ~/.local/bin pointing at `exe` (how a folder install is usually started).
fn links_to(exe: &Path) -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(Path::new(&home).join(".local/bin")) else { return Vec::new() };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| std::fs::canonicalize(p).ok().as_deref() == Some(exe))
        .collect()
}

/// Installs the staged update and starts the watchdog. On success Annex must quit at once: the
/// watchdog waits for this process to end, then starts the new version.
pub fn switch(s: &Staged, share: bool) -> Result<(), String> {
    let (new_exe, old_exe, mode, links) = match &s.kind {
        StagedKind::Folder { new_exe, old_exe } => {
            let links = links_to(&std::fs::canonicalize(old_exe).unwrap_or_else(|_| old_exe.clone()));
            (new_exe.clone(), old_exe.clone(), "folder", links)
        }
        StagedKind::Package { files, previous } => {
            tracing::info!(target: "annex::update", version = s.version, "update: installing the package through pkexec");
            let out = std::process::Command::new(PKEXEC)
                .arg(HELPER)
                .arg("install")
                .args(files)
                .args(previous)
                .output()
                .map_err(|e| format!("couldn't run pkexec: {e}"))?;
            if !out.status.success() {
                return Err(format!("the helper failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            (PathBuf::from("/usr/bin/annex"), PathBuf::from("/usr/bin/annex"), "package", Vec::new())
        }
    };
    let _ = std::fs::remove_file(marker("update-ok"));
    std::fs::write(marker("update-pending"), format!("{} {mode}", s.version)).map_err(|e| e.to_string())?;
    // The session's display and bus, for an Annex started outside this process.
    let env: Vec<String> = [
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_SESSION_TYPE",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "HOME",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| format!("{k}={v}")))
    .collect();
    let script = s.dir.join("watchdog.sh");
    let line = |v: &[String]| v.iter().map(String::as_str).map(q).collect::<Vec<_>>().join(" ");
    let links: Vec<String> = links.iter().map(|l| l.display().to_string()).collect();
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/bash
# Annex updater: starts the new version once this one has quit, and puts the old one back if the
# new one doesn't say it runs within {wait} s.
pid={pid}; new={new}; old={old}; version={version}; mode={mode}; ok={ok}; failed={failed}; share={share}
env=({env}); links=({links}); helper={helper}; work={work}
start() {{
  if command -v systemd-run >/dev/null 2>&1; then
    args=(); for e in "${{env[@]}}"; do args+=(-E "$e"); done
    systemd-run --user --collect --quiet "${{args[@]}}" "$1" --background $share && return
  fi
  env "${{env[@]}}" setsid -f "$1" --background $share >/dev/null 2>&1 </dev/null
}}
while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
start "$new"
for _ in $(seq 1 {wait}); do
  if [ "$(cat "$ok" 2>/dev/null)" = "$version" ]; then
    for l in "${{links[@]}}"; do ln -sfn "$new" "$l"; done
    rm -f "$ok"; rm -rf "$work"; exit 0
  fi
  sleep 1
done
# The new version isn't running: put the old one back.
pkill -u "$(id -u)" -x annex
sleep 2
if [ "$mode" = package ]; then {pkexec} "$helper" rollback; fi
echo "$version" > "$failed"
start "$old"
rm -rf "$work"
"#,
            wait = CONFIRM_WITHIN,
            pid = std::process::id(),
            new = q(&new_exe.display().to_string()),
            old = q(&old_exe.display().to_string()),
            version = q(&s.version),
            mode = mode,
            ok = q(&marker("update-ok").display().to_string()),
            failed = q(&marker("update-failed").display().to_string()),
            share = if share { "--share" } else { "" },
            env = line(&env),
            links = line(&links),
            helper = q(HELPER),
            pkexec = PKEXEC,
            work = q(&s.dir.display().to_string()),
        ),
    )
    .map_err(|e| e.to_string())?;
    // Outside this process's unit and session, so it outlives Annex quitting.
    let started = std::process::Command::new("systemd-run")
        .args(["--user", "--collect", "--quiet", "/bin/bash"])
        .arg(&script)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        || std::process::Command::new("setsid")
            .args(["-f", "/bin/bash"])
            .arg(&script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    if !started {
        return Err("couldn't start the watchdog".into());
    }
    tracing::info!(target: "annex::update", version = s.version, mode, share, "update: switching; the old version stays until the new one runs");
    Ok(())
}

/// At launch, once sharing is back (if it was on): tells the watchdog this version runs, or logs
/// that an update was undone.
pub fn confirm() {
    let Some(pending) = read_marker("update-pending") else { return };
    let _ = std::fs::remove_file(marker("update-pending"));
    let (version, mode) = pending.split_once(' ').unwrap_or((pending.as_str(), ""));
    let current = annex_update::whats_new::CURRENT;
    if version != current {
        tracing::warn!(target: "annex::update", version, current, "update: the new version didn't start in {CONFIRM_WITHIN} s, the old one is back");
        return;
    }
    match std::fs::write(marker("update-ok"), version) {
        Ok(()) => tracing::info!(target: "annex::update", version, "update: updated unattended, and it runs"),
        Err(e) => tracing::warn!(target: "annex::update", "update: couldn't confirm the update: {e}"),
    }
    if mode == "package" {
        // The kept package isn't needed any more.
        let _ = std::process::Command::new(PKEXEC).arg(HELPER).arg("confirm").spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_installs_get_the_tarball() {
        let suffix = asset_suffix(Some(&Install::Folder(PathBuf::from("/opt/Annex"))));
        assert!(suffix.starts_with("-linux-") && suffix.ends_with(".tar.gz"), "{suffix}");
        assert!(asset_suffix(Some(&Install::Package)).ends_with(&format!(".{}", package_kind())));
    }

    #[test]
    fn shell_quoting_survives_quotes() {
        assert_eq!(q("it's"), r"'it'\''s'");
    }
}
