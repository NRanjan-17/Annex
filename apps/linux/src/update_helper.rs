//! annex-update-helper: installs a signed Annex package as root, for updates with no one at the
//! screen to type a password. pkexec runs it without asking (packaging/linux/
//! dev.getannex.annex.update.policy), so it trusts nothing it's given:
//!
//! - `install <package> <SHA256SUMS.txt> <SHA256SUMS.txt.sig> [<previous package> <its sums> <its sig>]`
//!   copies the files into a root-only folder, then installs the package only if its signature
//!   verifies, it's named `Annex-<version>-linux-<this CPU>.<deb|rpm>` and `<version>` is newer than
//!   this one. The previous package (this version, also verified) is kept for `rollback`.
//! - `rollback` reinstalls that kept package: the new version didn't start.
//! - `confirm` drops it: the new version runs.
//!
//! Nothing else: no other package, file or command can be reached through it.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// This helper's version is the installed Annex's (scripts/package-linux.sh sets both).
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const STATE: &str = "/var/lib/annex";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("annex-update-helper: must run as root (through pkexec)");
        return ExitCode::from(2);
    }
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["install", package, sums, sig] => install([*package, *sums, *sig], None),
        ["install", package, sums, sig, prev, prev_sums, prev_sig] => {
            install([*package, *sums, *sig], Some([*prev, *prev_sums, *prev_sig]))
        }
        ["rollback"] => rollback(),
        ["confirm"] => confirm(),
        _ => Err(
            "usage: annex-update-helper install <package> <sums> <sig> [<previous> <sums> <sig>] | rollback | confirm"
                .into(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("annex-update-helper: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `deb` or `rpm`: what this system installs.
fn kind() -> &'static str {
    if Path::new("/usr/bin/dpkg").exists() { "deb" } else { "rpm" }
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        a => a,
    }
}

/// The version in a package's name, if it's exactly `Annex-<version>-linux-<this CPU>.<kind>`.
fn version_of(name: &str) -> Option<&str> {
    let v = name.strip_prefix("Annex-")?.strip_suffix(&format!("-linux-{}.{}", arch(), kind()))?;
    (!v.is_empty() && !v.contains('/')).then_some(v)
}

/// A fresh root-only folder.
fn folder(name: &str) -> Result<PathBuf, String> {
    let dir = Path::new(STATE).join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    fs::set_permissions(Path::new(STATE), fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Copies the package and its signed checksums into `dir`, then checks them there (the caller
/// can't change the copies). Returns the package's path and version.
fn take(files: [&str; 3], dir: &Path) -> Result<(PathBuf, String), String> {
    let name = Path::new(files[0]).file_name().and_then(|n| n.to_str()).ok_or("the package has no name")?.to_string();
    let version =
        version_of(&name).ok_or_else(|| format!("{name} isn't an Annex package for this computer"))?.to_string();
    let copies = [dir.join(&name), dir.join(annex_update::SUMS_NAME), dir.join(annex_update::SIG_NAME)];
    for (from, to) in files.iter().zip(&copies) {
        fs::copy(from, to).map_err(|e| format!("{from}: {e}"))?;
    }
    let sums = fs::read(&copies[1]).map_err(|e| e.to_string())?;
    let sig = fs::read(&copies[2]).map_err(|e| e.to_string())?;
    annex_update::verify(&sums, &sig, &copies[0], &name).map_err(|e| e.to_string())?;
    Ok((copies[0].clone(), version))
}

fn install(new: [&str; 3], previous: Option<[&str; 3]>) -> Result<(), String> {
    let dir = folder("update")?;
    let (package, version) = take(new, &dir)?;
    if !annex_update::is_newer(&version, CURRENT) {
        return Err(format!("{version} isn't newer than {CURRENT}"));
    }
    if let Some(previous) = previous {
        let keep = folder("previous")?;
        let (_, v) = take(previous, &keep)?;
        if v != CURRENT {
            let _ = fs::remove_dir_all(&keep);
            return Err(format!("the previous package is {v}, not {CURRENT}"));
        }
        eprintln!("annex-update-helper: keeping {CURRENT} to put back if {version} doesn't start");
    }
    eprintln!("annex-update-helper: installing {version} (was {CURRENT})");
    let result = run_install(&package, false);
    let _ = fs::remove_dir_all(&dir);
    result
}

fn rollback() -> Result<(), String> {
    let keep = Path::new(STATE).join("previous");
    let package = fs::read_dir(&keep)
        .map_err(|e| format!("nothing to put back: {e}"))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()).and_then(version_of).is_some())
        .ok_or("nothing to put back")?;
    eprintln!("annex-update-helper: putting back {}", package.display());
    let result = run_install(&package, true);
    let _ = fs::remove_dir_all(&keep);
    result
}

fn confirm() -> Result<(), String> {
    let keep = Path::new(STATE).join("previous");
    if keep.exists() {
        fs::remove_dir_all(&keep).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Installs with the system's own tool (it fetches any new dependency). `older`: putting the
/// previous version back.
fn run_install(package: &Path, older: bool) -> Result<(), String> {
    let p = package.to_str().ok_or("bad path")?;
    let mut cmd = if kind() == "deb" {
        if Path::new("/usr/bin/apt-get").exists() {
            let mut c = Command::new("/usr/bin/apt-get");
            c.args([
                "install",
                "-y",
                "-q",
                "-o",
                "Dpkg::Options::=--force-confdef",
                "-o",
                "Dpkg::Options::=--force-confold",
            ]);
            if older {
                c.arg("--allow-downgrades");
            }
            c.arg(p);
            c
        } else {
            let mut c = Command::new("/usr/bin/dpkg");
            c.args(["-i", p]);
            c
        }
    } else if Path::new("/usr/bin/dnf").exists() {
        let mut c = Command::new("/usr/bin/dnf");
        c.args(["-y", if older { "downgrade" } else { "install" }, p]);
        c
    } else if Path::new("/usr/bin/zypper").exists() {
        let mut c = Command::new("/usr/bin/zypper");
        c.args(["--non-interactive", "install", "--allow-unsigned-rpm"]);
        if older {
            c.arg("--oldpackage");
        }
        c.arg(p);
        c
    } else {
        let mut c = Command::new("/usr/bin/rpm");
        c.arg("-U");
        if older {
            c.arg("--oldpackage");
        }
        c.arg(p);
        c
    };
    cmd.env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("LANG", "C.UTF-8");
    let status = cmd.status().map_err(|e| format!("couldn't run the installer: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("the installer failed ({status})")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_annex_packages_for_this_computer_are_taken() {
        let ok = format!("Annex-0.5.0-linux-{}.{}", arch(), kind());
        assert_eq!(version_of(&ok), Some("0.5.0"));
        let pre = format!("Annex-0.5.0-beta.1-linux-{}.{}", arch(), kind());
        assert_eq!(version_of(&pre), Some("0.5.0-beta.1"));
        assert_eq!(version_of(&format!("Annex-0.5.0-linux-sparc.{}", kind())), None);
        assert_eq!(version_of(&format!("evil-0.5.0-linux-{}.{}", arch(), kind())), None);
        assert_eq!(version_of(&format!("Annex--linux-{}.{}", arch(), kind())), None);
        assert_eq!(version_of(&format!("Annex-0.5.0-linux-{}.tar.gz", arch())), None);
    }
}
