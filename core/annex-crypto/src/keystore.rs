//! Where the identity's private key lives.
//!
//! - macOS: the login Keychain (generic password item, service `com.nranjan-17.annex`).
//! - Windows: a file encrypted with DPAPI for the current user (`identity.key.dpapi`).
//! - Elsewhere, or with `ANNEX_KEY_STORAGE=file`: a 0600 PEM file (`identity.key.pem`).
//!
//! A plaintext key left by an older version is migrated into the platform store and deleted.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::CryptoError;

pub const KEYCHAIN_SERVICE: &str = "com.nranjan-17.annex";
pub(crate) const PEM_FILE: &str = "identity.key.pem";
#[cfg(windows)]
const DPAPI_FILE: &str = "identity.key.dpapi";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStorage {
    /// Plain PEM file with owner-only permissions.
    File,
    /// macOS login Keychain.
    Keychain,
    /// Windows DPAPI-encrypted file (current user).
    Dpapi,
}

impl KeyStorage {
    /// The platform's secure store, unless `ANNEX_KEY_STORAGE=file|keychain|dpapi` overrides it
    /// (tests and headless tools use `file` so they never touch the user's keychain).
    pub fn platform_default() -> Self {
        match std::env::var("ANNEX_KEY_STORAGE").as_deref() {
            Ok("file") => return KeyStorage::File,
            Ok("keychain") => return KeyStorage::Keychain,
            Ok("dpapi") => return KeyStorage::Dpapi,
            _ => {}
        }
        if cfg!(target_os = "macos") {
            KeyStorage::Keychain
        } else if cfg!(windows) {
            KeyStorage::Dpapi
        } else {
            KeyStorage::File
        }
    }

    /// Reads the PEM-encoded private key for the identity in `dir`, if one exists.
    pub(crate) fn load(self, dir: &Path) -> Result<Option<String>, CryptoError> {
        let pem_path = dir.join(PEM_FILE);
        let stored = match self {
            KeyStorage::File => return read_optional(&pem_path),
            KeyStorage::Keychain => keychain::get(&account(dir))?,
            KeyStorage::Dpapi => dpapi::load(dir)?,
        };
        if stored.is_some() {
            return Ok(stored);
        }
        // Migrate a plaintext key from an older install.
        match read_optional(&pem_path)? {
            Some(pem) => {
                self.store(dir, &pem)?;
                std::fs::remove_file(&pem_path)?;
                Ok(Some(pem))
            }
            None => Ok(None),
        }
    }

    pub(crate) fn store(self, dir: &Path, pem: &str) -> Result<(), CryptoError> {
        match self {
            KeyStorage::File => crate::identity::write_private(&dir.join(PEM_FILE), pem.as_bytes()).map_err(Into::into),
            KeyStorage::Keychain => keychain::set(&account(dir), pem),
            KeyStorage::Dpapi => dpapi::store(dir, pem),
        }
    }

    /// Removes the stored key (used by uninstall / reset flows and tests).
    pub fn delete(self, dir: &Path) -> Result<(), CryptoError> {
        let _ = std::fs::remove_file(dir.join(PEM_FILE));
        match self {
            KeyStorage::File => Ok(()),
            KeyStorage::Keychain => keychain::delete(&account(dir)),
            KeyStorage::Dpapi => dpapi::delete(dir),
        }
    }
}

/// Keychain account name: one item per data directory, so separate installs/tools don't clash.
fn account(dir: &Path) -> String {
    let canonical: PathBuf = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    format!("identity-{}", hex::encode(&digest[..8]))
}

fn read_optional(path: &Path) -> Result<Option<String>, CryptoError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(target_os = "macos")]
mod keychain {
    use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};

    use super::KEYCHAIN_SERVICE;
    use crate::CryptoError;

    // errSecItemNotFound
    const NOT_FOUND: i32 = -25300;

    pub fn get(account: &str) -> Result<Option<String>, CryptoError> {
        match get_generic_password(KEYCHAIN_SERVICE, account) {
            Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| CryptoError::Corrupt("keychain item".into())),
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(CryptoError::Corrupt(format!("keychain: {e}"))),
        }
    }

    pub fn set(account: &str, pem: &str) -> Result<(), CryptoError> {
        set_generic_password(KEYCHAIN_SERVICE, account, pem.as_bytes())
            .map_err(|e| CryptoError::Corrupt(format!("keychain: {e}")))
    }

    pub fn delete(account: &str) -> Result<(), CryptoError> {
        match delete_generic_password(KEYCHAIN_SERVICE, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == NOT_FOUND => Ok(()),
            Err(e) => Err(CryptoError::Corrupt(format!("keychain: {e}"))),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod keychain {
    use crate::CryptoError;

    fn unsupported() -> CryptoError {
        CryptoError::Corrupt("the Keychain is only available on macOS".into())
    }

    pub fn get(_: &str) -> Result<Option<String>, CryptoError> {
        Err(unsupported())
    }

    pub fn set(_: &str, _: &str) -> Result<(), CryptoError> {
        Err(unsupported())
    }

    pub fn delete(_: &str) -> Result<(), CryptoError> {
        Err(unsupported())
    }
}

#[cfg(windows)]
mod dpapi {
    use std::path::Path;

    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::w;

    use super::DPAPI_FILE;
    use crate::CryptoError;

    const ENTROPY: &[u8] = b"annex identity key v1";

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: DPAPI allocated `cbData` bytes with LocalAlloc; we copy then free them.
        unsafe {
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
            v
        }
    }

    pub fn store(dir: &Path, pem: &str) -> Result<(), CryptoError> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        let entropy = blob(ENTROPY);
        unsafe {
            CryptProtectData(
                &blob(pem.as_bytes()),
                w!("Annex identity"),
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
        .map_err(|e| CryptoError::Corrupt(format!("DPAPI protect: {e}")))?;
        crate::identity::write_private(&dir.join(DPAPI_FILE), &take(out))?;
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Option<String>, CryptoError> {
        let data = match std::fs::read(dir.join(DPAPI_FILE)) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        let entropy = blob(ENTROPY);
        unsafe {
            CryptUnprotectData(&blob(&data), None, Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        }
        .map_err(|e| CryptoError::Corrupt(format!("DPAPI unprotect: {e}")))?;
        String::from_utf8(take(out)).map(Some).map_err(|_| CryptoError::Corrupt("DPAPI payload".into()))
    }

    pub fn delete(dir: &Path) -> Result<(), CryptoError> {
        match std::fs::remove_file(dir.join(DPAPI_FILE)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(not(windows))]
mod dpapi {
    use std::path::Path;

    use crate::CryptoError;

    fn unsupported() -> CryptoError {
        CryptoError::Corrupt("DPAPI is only available on Windows".into())
    }

    pub fn store(_: &Path, _: &str) -> Result<(), CryptoError> {
        Err(unsupported())
    }

    pub fn load(_: &Path) -> Result<Option<String>, CryptoError> {
        Err(unsupported())
    }

    pub fn delete(_: &Path) -> Result<(), CryptoError> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_storage_roundtrip_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(KeyStorage::File.load(dir.path()).unwrap(), None);
        KeyStorage::File.store(dir.path(), "PEM").unwrap();
        assert_eq!(KeyStorage::File.load(dir.path()).unwrap().as_deref(), Some("PEM"));
        KeyStorage::File.delete(dir.path()).unwrap();
        assert_eq!(KeyStorage::File.load(dir.path()).unwrap(), None);
    }

    #[test]
    fn accounts_differ_per_directory() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_ne!(account(a.path()), account(b.path()));
        assert_eq!(account(a.path()), account(a.path()));
    }

    /// Uses the real platform store; leaves nothing behind. Run with `--ignored`.
    #[test]
    #[ignore]
    fn platform_store_roundtrip_and_migration() {
        let storage = if cfg!(target_os = "macos") {
            KeyStorage::Keychain
        } else if cfg!(windows) {
            KeyStorage::Dpapi
        } else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        // A legacy plaintext key is migrated and removed.
        KeyStorage::File.store(dir.path(), "LEGACY").unwrap();
        assert_eq!(storage.load(dir.path()).unwrap().as_deref(), Some("LEGACY"));
        assert!(!dir.path().join(PEM_FILE).exists());
        assert_eq!(storage.load(dir.path()).unwrap().as_deref(), Some("LEGACY"));
        storage.delete(dir.path()).unwrap();
        assert_eq!(storage.load(dir.path()).unwrap(), None);
    }
}
