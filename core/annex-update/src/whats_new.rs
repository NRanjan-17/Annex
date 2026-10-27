//! What's new: the short notes the apps show once after an update, so people learn what the new
//! version does and where to find it. Written in `notes/whats-new.json`, newest release first.

use semver::Version;
use serde::{Deserialize, Serialize};

const NOTES: &str = include_str!("../notes/whats-new.json");

/// This release's version (the release stamps it into the core crates).
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Item {
    /// "new", "improved" or "fixed".
    pub kind: String,
    pub title: String,
    /// How to use it, in a sentence or two.
    pub detail: String,
}

/// The notes to show after updating from `last_seen` to `current`: every release after
/// `last_seen` up to `current`, newest first (one dialog covers versions that were skipped).
/// Empty on a first install (`last_seen` is `None`), when nothing changed, or for a version
/// without notes.
pub fn since(last_seen: Option<&str>, current: &str) -> Vec<Release> {
    let (Some(last), Ok(current)) = (last_seen.and_then(parse), Version::parse(current)) else {
        return Vec::new();
    };
    all().into_iter().filter(|r| parse(&r.version).is_some_and(|v| v > last && v <= current)).collect()
}

/// Every release's notes, newest first (for a "What's new" button).
pub fn all() -> Vec<Release> {
    serde_json::from_str(NOTES).unwrap_or_default()
}

fn parse(v: &str) -> Option<Version> {
    Version::parse(v.trim_start_matches('v')).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_valid_and_newest_first() {
        let all: Vec<Release> = serde_json::from_str(NOTES).expect("notes/whats-new.json is valid");
        assert!(!all.is_empty());
        let versions: Vec<Version> = all.iter().map(|r| Version::parse(&r.version).unwrap()).collect();
        assert!(versions.windows(2).all(|w| w[0] > w[1]), "newest first: {versions:?}");
        for r in &all {
            assert!(!r.items.is_empty(), "{} has no notes", r.version);
            for i in &r.items {
                assert!(["new", "improved", "fixed"].contains(&i.kind.as_str()), "{}: kind {}", r.version, i.kind);
                assert!(!i.title.is_empty() && !i.detail.is_empty());
            }
        }
    }

    #[test]
    fn shows_after_an_update_only() {
        let newest = all()[0].version.clone();
        assert!(since(None, &newest).is_empty(), "not on a first install");
        assert!(since(Some(&newest), &newest).is_empty(), "not twice");
        assert_eq!(since(Some("0.3.0"), &newest)[0].version, newest);
        assert_eq!(since(Some("v0.3.0"), &newest)[0].version, newest);
        assert!(since(Some("0.3.0"), "0.3.1").iter().all(|r| r.version != newest), "not a later version's notes");
    }
}
