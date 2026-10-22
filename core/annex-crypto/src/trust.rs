use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::CryptoError;
use crate::identity::write_atomic;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TrustedPeer {
    pub fingerprint: String,
    pub name: String,
    pub added_at: u64,
    #[serde(default)]
    pub last_seen: u64,
    /// Host side: this device may control our mouse and keyboard. Off until the user turns it on.
    #[serde(default)]
    pub allow_input: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct TrustFile {
    version: u32,
    peers: Vec<TrustedPeer>,
}

/// Pinned peer fingerprints, persisted as JSON. Removing an entry revokes the peer:
/// its next connection is treated as unknown and requires approval again.
pub struct TrustStore {
    path: Option<PathBuf>,
    peers: Vec<TrustedPeer>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl TrustStore {
    pub fn in_memory() -> Self {
        TrustStore { path: None, peers: Vec::new() }
    }

    pub fn open(path: &Path) -> Result<Self, CryptoError> {
        let peers = match std::fs::read(path) {
            Ok(bytes) => {
                let f: TrustFile =
                    serde_json::from_slice(&bytes).map_err(|e| CryptoError::Corrupt(format!("trust store: {e}")))?;
                f.peers
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(TrustStore { path: Some(path.to_path_buf()), peers })
    }

    pub fn get(&self, fingerprint: &str) -> Option<&TrustedPeer> {
        self.peers.iter().find(|p| p.fingerprint == fingerprint)
    }

    pub fn is_trusted(&self, fingerprint: &str) -> bool {
        self.get(fingerprint).is_some()
    }

    pub fn list(&self) -> &[TrustedPeer] {
        &self.peers
    }

    pub fn add(&mut self, fingerprint: &str, name: &str) -> Result<(), CryptoError> {
        let t = now();
        match self.peers.iter_mut().find(|p| p.fingerprint == fingerprint) {
            Some(p) => {
                p.name = name.to_string();
                p.last_seen = t;
            }
            None => self.peers.push(TrustedPeer {
                fingerprint: fingerprint.to_string(),
                name: name.to_string(),
                added_at: t,
                last_seen: t,
                allow_input: false,
            }),
        }
        self.save()
    }

    /// Returns true if the peer was present.
    pub fn remove(&mut self, fingerprint: &str) -> Result<bool, CryptoError> {
        let before = self.peers.len();
        self.peers.retain(|p| p.fingerprint != fingerprint);
        let removed = self.peers.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Returns false if the peer isn't trusted.
    pub fn set_allow_input(&mut self, fingerprint: &str, allow: bool) -> Result<bool, CryptoError> {
        let Some(p) = self.peers.iter_mut().find(|p| p.fingerprint == fingerprint) else { return Ok(false) };
        p.allow_input = allow;
        self.save()?;
        Ok(true)
    }

    pub fn touch(&mut self, fingerprint: &str) -> Result<(), CryptoError> {
        if let Some(p) = self.peers.iter_mut().find(|p| p.fingerprint == fingerprint) {
            p.last_seen = now();
            self.save()?;
        }
        Ok(())
    }

    fn save(&self) -> Result<(), CryptoError> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let f = TrustFile { version: 1, peers: self.peers.clone() };
        let json = serde_json::to_vec_pretty(&f).map_err(|e| CryptoError::Corrupt(e.to_string()))?;
        write_atomic(path, &json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_persist_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let fp = "a".repeat(64);
        {
            let mut s = TrustStore::open(&path).unwrap();
            assert!(!s.is_trusted(&fp));
            s.add(&fp, "Laptop").unwrap();
        }
        let mut s = TrustStore::open(&path).unwrap();
        assert_eq!(s.get(&fp).unwrap().name, "Laptop");
        assert!(!s.get(&fp).unwrap().allow_input);
        assert!(s.set_allow_input(&fp, true).unwrap());
        assert!(TrustStore::open(&path).unwrap().get(&fp).unwrap().allow_input);
        // Re-pairing keeps the setting.
        s.add(&fp, "Laptop 2").unwrap();
        assert!(s.get(&fp).unwrap().allow_input);
        assert!(!s.set_allow_input(&"b".repeat(64), true).unwrap());
        assert!(s.remove(&fp).unwrap());
        assert!(!s.remove(&fp).unwrap());
        assert!(!TrustStore::open(&path).unwrap().is_trusted(&fp));
    }

    #[test]
    fn corrupt_file_is_an_error_not_empty_trust() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(TrustStore::open(&path).is_err());
    }
}
