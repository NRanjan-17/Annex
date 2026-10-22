//! Device identity, trust store and TLS configuration for Annex.
//!
//! Each install has one long-lived Ed25519 key and self-signed certificate. A device is
//! identified by the SHA-256 fingerprint of that certificate. There is no CA: peers are
//! trusted only after the user explicitly approves them (M1) or pairs with a PIN (M2), and
//! from then on the fingerprint is pinned.

mod identity;
mod keystore;
pub mod pairing;
mod tls;
mod trust;

pub use identity::{Identity, display_fingerprint, fingerprint};
pub use keystore::{KEYCHAIN_SERVICE, KeyStorage};
pub use tls::{SERVER_NAME, client_config, peer_fingerprint, server_config};
pub use trust::{TrustStore, TrustedPeer};

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("certificate generation failed: {0}")]
    Cert(#[from] rcgen::Error),
    #[error("tls error: {0}")]
    Tls(#[from] rustls::Error),
    #[error("corrupt data: {0}")]
    Corrupt(String),
}
