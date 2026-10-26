use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ED25519};
use sha2::{Digest, Sha256};

use crate::CryptoError;

use crate::keystore::KeyStorage;

const CERT_FILE: &str = "identity.cert.der";

/// This device's long-lived key pair and certificate.
pub struct Identity {
    key_der: Vec<u8>,
    cert_der: Vec<u8>,
    fingerprint: String,
}

impl std::fmt::Debug for Identity {
    // Never print key material.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity").field("fingerprint", &self.fingerprint).finish_non_exhaustive()
    }
}

impl Identity {
    /// Generates a fresh identity in memory (tests, benchmarks).
    pub fn generate() -> Result<Self, CryptoError> {
        let key = KeyPair::generate_for(&PKCS_ED25519)?;
        Self::from_key(key)
    }

    /// Loads the identity from `dir` (private key in the platform store, certificate as a
    /// file), creating and persisting a new one on first run.
    pub fn load_or_create(dir: &Path) -> Result<Self, CryptoError> {
        Self::load_or_create_with(dir, KeyStorage::platform_default())
    }

    pub fn load_or_create_with(dir: &Path, storage: KeyStorage) -> Result<Self, CryptoError> {
        fs::create_dir_all(dir)?;
        let cert_path = dir.join(CERT_FILE);
        if let Some(pem) = storage.load(dir)?
            && cert_path.exists()
        {
            let key = KeyPair::from_pem(&pem)?;
            let cert_der = fs::read(&cert_path)?;
            if keys_match(&key, &cert_der) {
                return Ok(Identity { key_der: key.serialize_der(), fingerprint: fingerprint(&cert_der), cert_der });
            }
            // Something replaced one of the two (TLS would fail with KeyMismatch on every
            // connection): keep the stored key and make its certificate again. Paired computers
            // then ask for the PIN once more, as the fingerprint changes.
            tracing::warn!("identity certificate doesn't match the stored key; making a new certificate");
            let id = Self::from_key(key)?;
            write_atomic(&cert_path, &id.cert_der)?;
            return Ok(id);
        }
        let key = KeyPair::generate_for(&PKCS_ED25519)?;
        let pem = key.serialize_pem();
        let id = Self::from_key(key)?;
        storage.store(dir, &pem)?;
        write_atomic(&cert_path, &id.cert_der)?;
        Ok(id)
    }

    fn from_key(key: KeyPair) -> Result<Self, CryptoError> {
        let mut params = CertificateParams::new(vec![crate::SERVER_NAME.to_string()])?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "Annex Device");
        params.distinguished_name = dn;
        let cert = params.self_signed(&key)?;
        let cert_der = cert.der().to_vec();
        Ok(Identity { key_der: key.serialize_der(), fingerprint: fingerprint(&cert_der), cert_der })
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    pub(crate) fn key_der(&self) -> &[u8] {
        &self.key_der
    }
}

/// Whether `cert_der` is the certificate for `key`.
fn keys_match(key: &KeyPair, cert_der: &[u8]) -> bool {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let Ok(signer) = rustls::crypto::ring::default_provider().key_provider.load_private_key(der) else {
        return false;
    };
    rustls::sign::CertifiedKey::new(vec![CertificateDer::from(cert_der.to_vec())], signer).keys_match().is_ok()
}

/// Lowercase hex SHA-256 of a DER certificate.
pub fn fingerprint(cert_der: &[u8]) -> String {
    hex::encode(Sha256::digest(cert_der))
}

/// Short, human-comparable form shown in trust prompts: the first 80 bits as
/// five upper-case groups, e.g. `AB12 CD34 EF56 7890 1234`.
pub fn display_fingerprint(fp: &str) -> String {
    fp.as_bytes().chunks(4).take(5).map(|c| String::from_utf8_lossy(c).to_uppercase()).collect::<Vec<_>>().join(" ")
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".tmp");
    PathBuf::from(p)
}

pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = tmp_path(path);
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(tmp, path)
}

pub(crate) fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = tmp_path(path);
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_and_reloads_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create_with(dir.path(), KeyStorage::File).unwrap();
        let b = Identity::load_or_create_with(dir.path(), KeyStorage::File).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint().len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(crate::keystore::PEM_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn replaced_certificate_is_made_again_for_the_stored_key() {
        let dir = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create_with(dir.path(), KeyStorage::File).unwrap();
        // Another identity's certificate, as a tool writing into the same folder would leave.
        fs::write(dir.path().join(CERT_FILE), Identity::generate().unwrap().cert_der()).unwrap();
        let b = Identity::load_or_create_with(dir.path(), KeyStorage::File).unwrap();
        assert_eq!(a.key_der(), b.key_der());
        assert!(crate::tls::server_config(&b).is_ok());
        let c = Identity::load_or_create_with(dir.path(), KeyStorage::File).unwrap();
        assert_eq!(b.fingerprint(), c.fingerprint());
    }

    #[test]
    fn distinct_identities_differ() {
        assert_ne!(Identity::generate().unwrap().fingerprint(), Identity::generate().unwrap().fingerprint());
    }

    #[test]
    fn debug_hides_key() {
        let id = Identity::generate().unwrap();
        assert!(!format!("{id:?}").contains("key_der"));
    }

    #[test]
    fn display_form() {
        assert_eq!(display_fingerprint(&"ab12".repeat(16)), "AB12 AB12 AB12 AB12 AB12");
    }
}
