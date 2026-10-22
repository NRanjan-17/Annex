//! TLS 1.3 configuration with mutual authentication and certificate pinning.
//!
//! The verifiers here check that the peer proves possession of the key in its certificate
//! (the TLS handshake signature) but do not decide whether that key is *trusted*: the session
//! layer compares [`peer_fingerprint`] against the trust store, and asks the user if unknown.
//! When the expected fingerprint is already known (connecting to a paired host), the client
//! verifier rejects any other certificate during the handshake itself.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, DistinguishedName, Error, ServerConfig, SignatureScheme,
};

use crate::{CryptoError, Identity, fingerprint};

/// Fixed SNI / subject name. Peers are identified by fingerprint, never by name.
pub const SERVER_NAME: &str = "annex.local";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct PinningVerifier {
    algs: WebPkiSupportedAlgorithms,
    expected: Option<String>,
}

impl PinningVerifier {
    fn new(expected: Option<String>) -> Self {
        PinningVerifier { algs: provider().signature_verification_algorithms, expected }
    }

    fn check(&self, end_entity: &CertificateDer<'_>, intermediates: &[CertificateDer<'_>]) -> Result<(), Error> {
        if !intermediates.is_empty() {
            return Err(Error::InvalidCertificate(CertificateError::BadEncoding));
        }
        if let Some(expected) = &self.expected
            && fingerprint(end_entity) != *expected
        {
            return Err(Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure));
        }
        Ok(())
    }

    fn verify13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        if dss.scheme != SignatureScheme::ED25519 {
            return Err(Error::PeerIncompatible(rustls::PeerIncompatible::NoSignatureSchemesInCommon));
        }
        verify_tls13_signature(message, cert, dss, &self.algs)
    }
}

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        self.check(end_entity, intermediates)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::General("TLS 1.2 is disabled".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

impl ClientCertVerifier for PinningVerifier {
    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        self.check(end_entity, intermediates)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::General("TLS 1.2 is disabled".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

fn key_of(id: &Identity) -> PrivateKeyDer<'static> {
    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key_der().to_vec()))
}

/// TLS 1.3-only server config that requires a client certificate.
pub fn server_config(id: &Identity) -> Result<ServerConfig, CryptoError> {
    let mut cfg = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(PinningVerifier::new(None)))
        .with_single_cert(vec![CertificateDer::from(id.cert_der().to_vec())], key_of(id))?;
    cfg.alpn_protocols = vec![annex_alpn()];
    // No session tickets / resumption: every connection does a full handshake with fresh
    // ephemeral keys, and there is no 0-RTT data that could be replayed.
    cfg.send_tls13_tickets = 0;
    cfg.max_early_data_size = 0;
    Ok(cfg)
}

/// TLS 1.3-only client config presenting our certificate. If `expected_fingerprint` is set,
/// the handshake fails unless the server presents exactly that certificate.
pub fn client_config(id: &Identity, expected_fingerprint: Option<String>) -> Result<ClientConfig, CryptoError> {
    let mut cfg = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinningVerifier::new(expected_fingerprint)))
        .with_client_auth_cert(vec![CertificateDer::from(id.cert_der().to_vec())], key_of(id))?;
    cfg.alpn_protocols = vec![annex_alpn()];
    cfg.resumption = rustls::client::Resumption::disabled();
    cfg.enable_early_data = false;
    Ok(cfg)
}

fn annex_alpn() -> Vec<u8> {
    // Kept in sync with annex_proto::ALPN by the session crate's tests.
    b"annex/1".to_vec()
}

/// Fingerprint of the single certificate the peer presented, if any.
pub fn peer_fingerprint(certs: Option<&[CertificateDer<'_>]>) -> Option<String> {
    match certs {
        Some([one]) => Some(fingerprint(one)),
        _ => None,
    }
}
