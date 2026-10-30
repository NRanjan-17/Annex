//! In-app updates. The apps fetch this repository's GitHub releases list and the files themselves;
//! this crate decides which release to offer and checks that a download is genuine.
//!
//! Each release publishes `SHA256SUMS.txt` and `SHA256SUMS.txt.sig`, an Ed25519 signature over
//! that file made in CI with a key kept in the protected `release` environment. A download is
//! accepted only if the signature verifies against [`UPDATE_PUBLIC_KEY`] and the file's SHA-256 is
//! listed for its name. Releases without a signature are never offered.

pub mod whats_new;

use std::io::Read;
use std::path::Path;

use ring::signature::{ED25519, UnparsedPublicKey};
use semver::Version;
use serde::{Deserialize, Serialize};

/// Ed25519 public key that signs release checksums (the private half is a CI secret).
pub const UPDATE_PUBLIC_KEY: [u8; 32] = hex_key("95ebd305c46941cb5b5dd30f8f9c1d3c2c7454327f29fd925a91f2044205dc36");

/// Where the apps fetch the releases list from.
pub const RELEASES_URL: &std::ffi::CStr = c"https://api.github.com/repos/NRanjan-17/Annex/releases?per_page=30";

pub const SUMS_NAME: &str = "SHA256SUMS.txt";
pub const SIG_NAME: &str = "SHA256SUMS.txt.sig";

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("the releases list couldn't be read: {0}")]
    BadReleases(String),
    #[error("the update's signature doesn't match; it was not installed")]
    BadSignature,
    #[error("the download doesn't match its signed checksum; it was not installed")]
    BadChecksum,
    #[error("{0} isn't listed in the signed checksums")]
    NotListed(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// A release worth offering, with everything needed to download and verify it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Update {
    pub version: String,
    pub prerelease: bool,
    /// Moving from a pre-release back to an older normal release.
    pub downgrade: bool,
    pub notes: String,
    pub page_url: String,
    pub asset_name: String,
    pub asset_url: String,
    pub sums_url: String,
    pub sig_url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Picks the release to offer from GitHub's releases JSON, or `None` if this version is current.
///
/// `beta`: include pre-releases. Without it, someone on a pre-release is offered the newest normal
/// release even if it's older (that's what turning Beta updates off means). `asset_suffix` names
/// this platform's file, e.g. `-macos-universal.dmg`, `-x64.exe` or `-linux-arm64.deb`.
///
/// The tag isn't signed, so the file must be named for the tag's version (`Annex-<v><suffix>` or
/// `Annex-Setup-<v><suffix>`, as CI names it). [`verify`] then requires exactly that name in the
/// signed checksums, which ties the offered version to the signature: an old signed file
/// re-published under a newer tag isn't offered.
pub fn pick(releases_json: &str, current: &str, beta: bool, asset_suffix: &str) -> Result<Option<Update>, UpdateError> {
    let releases: Vec<Release> =
        serde_json::from_str(releases_json).map_err(|e| UpdateError::BadReleases(e.to_string()))?;
    let current =
        Version::parse(current.trim_start_matches('v')).map_err(|e| UpdateError::BadReleases(e.to_string()))?;
    let candidate = releases
        .iter()
        .filter(|r| !r.draft && (beta || !r.prerelease))
        .filter_map(|r| {
            let v = r.tag_name.trim_start_matches('v');
            let version = Version::parse(v).ok()?;
            let names = [format!("Annex-{v}{asset_suffix}"), format!("Annex-Setup-{v}{asset_suffix}")];
            let asset = r.assets.iter().find(|a| names.contains(&a.name))?;
            let sums = r.assets.iter().find(|a| a.name == SUMS_NAME)?;
            let sig = r.assets.iter().find(|a| a.name == SIG_NAME)?;
            Some((version, r, asset, sums, sig))
        })
        .max_by(|a, b| a.0.cmp(&b.0));
    let Some((version, r, asset, sums, sig)) = candidate else { return Ok(None) };
    let newer = version > current;
    // Beta off while on a pre-release: go back to the newest normal release.
    let leave_beta = !beta && !current.pre.is_empty() && version.pre.is_empty() && version != current;
    if !(newer || leave_beta) {
        return Ok(None);
    }
    Ok(Some(Update {
        version: version.to_string(),
        prerelease: r.prerelease,
        downgrade: !newer,
        notes: r.body.clone().unwrap_or_default(),
        page_url: r.html_url.clone(),
        asset_name: asset.name.clone(),
        asset_url: asset.browser_download_url.clone(),
        sums_url: sums.browser_download_url.clone(),
        sig_url: sig.browser_download_url.clone(),
    }))
}

/// The release of exactly `version`, with its `asset_suffix` file: what an unattended update keeps
/// to put this version back if the new one doesn't start. `None` if it isn't published (or isn't
/// signed).
pub fn find(releases_json: &str, version: &str, asset_suffix: &str) -> Result<Option<Update>, UpdateError> {
    let releases: Vec<Release> =
        serde_json::from_str(releases_json).map_err(|e| UpdateError::BadReleases(e.to_string()))?;
    let v = version.trim_start_matches('v');
    let Some(r) = releases.iter().find(|r| !r.draft && r.tag_name.trim_start_matches('v') == v) else { return Ok(None) };
    let names = [format!("Annex-{v}{asset_suffix}"), format!("Annex-Setup-{v}{asset_suffix}")];
    let (Some(asset), Some(sums), Some(sig)) = (
        r.assets.iter().find(|a| names.contains(&a.name)),
        r.assets.iter().find(|a| a.name == SUMS_NAME),
        r.assets.iter().find(|a| a.name == SIG_NAME),
    ) else {
        return Ok(None);
    };
    Ok(Some(Update {
        version: v.to_string(),
        prerelease: r.prerelease,
        downgrade: false,
        notes: r.body.clone().unwrap_or_default(),
        page_url: r.html_url.clone(),
        asset_name: asset.name.clone(),
        asset_url: asset.browser_download_url.clone(),
        sums_url: sums.browser_download_url.clone(),
        sig_url: sig.browser_download_url.clone(),
    }))
}

/// Whether `candidate` is a newer version than `current` (semver; `false` if either doesn't parse).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate.trim_start_matches('v')), Version::parse(current.trim_start_matches('v'))) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

/// Checks a downloaded file against the release's signed checksums.
pub fn verify(sums: &[u8], signature: &[u8], file: &Path, asset_name: &str) -> Result<(), UpdateError> {
    verify_with_key(&UPDATE_PUBLIC_KEY, sums, signature, file, asset_name)
}

fn verify_with_key(
    key: &[u8; 32],
    sums: &[u8],
    signature: &[u8],
    file: &Path,
    asset_name: &str,
) -> Result<(), UpdateError> {
    UnparsedPublicKey::new(&ED25519, key).verify(sums, signature).map_err(|_| UpdateError::BadSignature)?;
    let text = std::str::from_utf8(sums).map_err(|_| UpdateError::BadSignature)?;
    // `sha256sum` lines: "<64 hex>  <name>" (a `*` before the name marks binary mode).
    let expected = text
        .lines()
        .filter_map(|l| l.split_once(char::is_whitespace))
        .find(|(_, name)| name.trim_start().trim_start_matches('*') == asset_name)
        .map(|(hash, _)| hash.to_ascii_lowercase())
        .ok_or_else(|| UpdateError::NotListed(asset_name.to_string()))?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut f = std::fs::File::open(file)?;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    if hex::encode(ctx.finish()) == expected { Ok(()) } else { Err(UpdateError::BadChecksum) }
}

const fn hex_key(s: &str) -> [u8; 32] {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("bad hex in UPDATE_PUBLIC_KEY"),
        }
    }
    let b = s.as_bytes();
    assert!(b.len() == 64, "UPDATE_PUBLIC_KEY must be 64 hex characters");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = nibble(b[2 * i]) << 4 | nibble(b[2 * i + 1]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    fn release(tag: &str, pre: bool, signed: bool) -> String {
        let v = tag.trim_start_matches('v');
        let sig = if signed {
            format!(r#",{{"name":"SHA256SUMS.txt.sig","browser_download_url":"https://x/{v}/sig"}}"#)
        } else {
            String::new()
        };
        format!(
            r#"{{"tag_name":"{tag}","draft":false,"prerelease":{pre},"body":"notes {v}","html_url":"https://x/{v}",
            "assets":[{{"name":"Annex-{v}-macos-universal.dmg","browser_download_url":"https://x/{v}/dmg"}},
            {{"name":"Annex-Setup-{v}-x64.exe","browser_download_url":"https://x/{v}/exe"}},
            {{"name":"SHA256SUMS.txt","browser_download_url":"https://x/{v}/sums"}}{sig}]}}"#
        )
    }

    fn list(rs: &[String]) -> String {
        format!("[{}]", rs.join(","))
    }

    #[test]
    fn offers_the_newest_normal_release() {
        let json = list(&[
            release("v0.2.0", false, true),
            release("v0.3.0-pre.1", true, true),
            release("v0.1.0", false, true),
        ]);
        let u = pick(&json, "0.1.0", false, "-macos-universal.dmg").unwrap().unwrap();
        assert_eq!(u.version, "0.2.0");
        assert_eq!(u.asset_url, "https://x/0.2.0/dmg");
        assert!(!u.downgrade && !u.prerelease);
        assert_eq!(pick(&json, "0.2.0", false, "-x64.exe").unwrap(), None, "already current");
    }

    #[test]
    fn beta_includes_pre_releases_in_semver_order() {
        let json = list(&[
            release("v0.1.1-pre.2", true, true),
            release("v0.1.1-pre.10", true, true),
            release("v0.1.0", false, true),
        ]);
        let u = pick(&json, "0.1.0", true, "-x64.exe").unwrap().unwrap();
        assert_eq!(u.version, "0.1.1-pre.10");
        assert!(u.prerelease);
        assert_eq!(pick(&json, "0.1.1-pre.10", true, "-x64.exe").unwrap(), None);
    }

    #[test]
    fn turning_beta_off_goes_back_to_the_newest_normal_release() {
        let json = list(&[release("v0.1.1-pre.1", true, true), release("v0.1.0", false, true)]);
        let u = pick(&json, "0.1.1-pre.1", false, "-x64.exe").unwrap().unwrap();
        assert_eq!(u.version, "0.1.0");
        assert!(u.downgrade);
        // A normal release newer than the pre-release is a plain update.
        let json = list(&[release("v0.1.1", false, true), release("v0.1.1-pre.1", true, true)]);
        assert!(!pick(&json, "0.1.1-pre.1", false, "-x64.exe").unwrap().unwrap().downgrade);
    }

    #[test]
    fn unsigned_releases_drafts_and_missing_assets_are_skipped() {
        let mut draft = release("v0.4.0", false, true);
        draft = draft.replace(r#""draft":false"#, r#""draft":true"#);
        let json = list(&[draft, release("v0.3.0", false, false), release("v0.2.0", false, true)]);
        assert_eq!(pick(&json, "0.1.0", false, "-x64.exe").unwrap().unwrap().version, "0.2.0");
        assert_eq!(pick(&json, "0.1.0", false, "-arm64.exe").unwrap(), None, "no file for this platform");
        assert!(pick("not json", "0.1.0", false, "-x64.exe").is_err());
    }

    #[test]
    fn a_newer_tag_on_an_older_file_is_not_offered() {
        // Someone re-publishes the signed 0.1.0 files under a v99 tag to force a downgrade.
        let replayed = release("v0.1.0", false, true).replace(r#""tag_name":"v0.1.0""#, r#""tag_name":"v99.0.0""#);
        let json = list(&[replayed, release("v0.2.0", false, true)]);
        let u = pick(&json, "0.1.0", false, "-x64.exe").unwrap().unwrap();
        assert_eq!((u.version.as_str(), u.asset_name.as_str()), ("0.2.0", "Annex-Setup-0.2.0-x64.exe"));
        assert_eq!(pick(&json, "0.2.0", false, "-macos-universal.dmg").unwrap(), None);
        // A pre-release's file doesn't stand in for the release of the same number either.
        let json = list(&[
            release("v1.0.0-pre.1", false, true).replace(r#""tag_name":"v1.0.0-pre.1""#, r#""tag_name":"v1.0.0""#)
        ]);
        assert_eq!(pick(&json, "0.1.0", false, "-x64.exe").unwrap(), None);
    }

    #[test]
    fn find_returns_exactly_that_signed_version() {
        let json = list(&[release("v0.4.1", false, true), release("v0.4.0", false, true), release("v0.3.9", false, false)]);
        let u = find(&json, "0.4.0", "-macos-universal.dmg").unwrap().unwrap();
        assert_eq!((u.version.as_str(), u.asset_name.as_str()), ("0.4.0", "Annex-0.4.0-macos-universal.dmg"));
        assert!(find(&json, "0.3.9", "-macos-universal.dmg").unwrap().is_none(), "unsigned");
        assert!(find(&json, "0.4.2", "-macos-universal.dmg").unwrap().is_none(), "not published");
    }

    #[test]
    fn newer_compares_semver() {
        assert!(is_newer("0.4.10", "0.4.9"));
        assert!(is_newer("v0.5.0", "0.5.0-beta.2"));
        assert!(!is_newer("0.4.1", "0.4.1"));
        assert!(!is_newer("0.4.0", "0.4.1"));
        assert!(!is_newer("garbage", "0.4.1"));
    }

    #[test]
    fn verify_accepts_only_signed_matching_files() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let key: [u8; 32] = pair.public_key().as_ref().try_into().unwrap();

        let dir = std::env::temp_dir().join(format!("annex-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Annex-Setup-1.0.0-x64.exe");
        std::fs::write(&file, b"installer bytes").unwrap();
        let hash = hex::encode(ring::digest::digest(&ring::digest::SHA256, b"installer bytes"));
        let sums = format!("{}  other.dmg\n{hash}  Annex-Setup-1.0.0-x64.exe\n", "0".repeat(64));
        let sig = pair.sign(sums.as_bytes());

        verify_with_key(&key, sums.as_bytes(), sig.as_ref(), &file, "Annex-Setup-1.0.0-x64.exe").unwrap();
        // Tampered checksums, wrong key, a file that changed, a name that isn't listed.
        let tampered = sums.replace("other", "evil");
        assert!(matches!(
            verify_with_key(&key, tampered.as_bytes(), sig.as_ref(), &file, "Annex-Setup-1.0.0-x64.exe"),
            Err(UpdateError::BadSignature)
        ));
        assert!(matches!(
            verify(sums.as_bytes(), sig.as_ref(), &file, "Annex-Setup-1.0.0-x64.exe"),
            Err(UpdateError::BadSignature)
        ));
        std::fs::write(&file, b"installer bytes, modified").unwrap();
        assert!(matches!(
            verify_with_key(&key, sums.as_bytes(), sig.as_ref(), &file, "Annex-Setup-1.0.0-x64.exe"),
            Err(UpdateError::BadChecksum)
        ));
        assert!(matches!(
            verify_with_key(&key, sums.as_bytes(), sig.as_ref(), &file, "missing.exe"),
            Err(UpdateError::NotListed(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn embedded_key_is_the_release_key() {
        assert_eq!(hex::encode(UPDATE_PUBLIC_KEY), "95ebd305c46941cb5b5dd30f8f9c1d3c2c7454327f29fd925a91f2044205dc36");
    }
}
