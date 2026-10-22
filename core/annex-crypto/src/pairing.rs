//! PIN pairing: SPAKE2 (password-authenticated key exchange) bound to the TLS session.
//!
//! The host shows a random 6-digit PIN; the user types it on the viewer. Both run SPAKE2 with the
//! PIN, then prove they derived the same key with HMACs over a transcript that includes both
//! certificate fingerprints, both SPAKE2 messages, and the TLS exporter secret.
//!
//! Why this is safe with only 10^6 PINs:
//! - SPAKE2 gives a passive eavesdropper nothing to brute-force offline, and an active attacker
//!   exactly one PIN guess per protocol run. The host limits failed runs (see `annex-session`).
//! - Binding to the TLS exporter means a man in the middle who terminates two TLS sessions has
//!   two different exporters, so the confirmations fail unless the attacker guessed the PIN.
//! - Binding to both fingerprints means the keys that get pinned afterwards are exactly the
//!   keys that took part in the exchange.

use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use subtle::ConstantTimeEq;

use crate::CryptoError;

pub const PIN_DIGITS: usize = 6;
/// TLS exporter label (RFC 8446 §7.5) used to bind pairing to the session.
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-annex-pairing-v1";
const TRANSCRIPT_TAG: &[u8] = b"annex/1 pairing transcript";

/// Which side of the exchange we are. The viewer is SPAKE2 side A, the host side B.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairRole {
    Viewer,
    Host,
}

/// A uniformly random 6-digit PIN from the OS CSPRNG.
pub fn generate_pin() -> Result<String, CryptoError> {
    // Rejection sampling avoids modulo bias.
    const LIMIT: u32 = u32::MAX - (u32::MAX % 1_000_000);
    loop {
        let mut b = [0u8; 4];
        getrandom::fill(&mut b).map_err(|e| CryptoError::Corrupt(format!("rng: {e}")))?;
        let v = u32::from_le_bytes(b);
        if v < LIMIT {
            return Ok(format!("{:06}", v % 1_000_000));
        }
    }
}

/// Accepts what a user might type ("123 456", "123-456") and returns the 6 digits.
pub fn normalize_pin(input: &str) -> Option<String> {
    let digits: String = input.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    (digits.len() == PIN_DIGITS && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}

/// First half of a pairing run: our SPAKE2 message is ready to send.
pub struct Pairing {
    role: PairRole,
    state: Spake2<Ed25519Group>,
    my_msg: Vec<u8>,
    viewer_fp: String,
    host_fp: String,
    exporter: Vec<u8>,
}

/// Second half: keys derived, confirmations ready.
pub struct Confirmation {
    mine: [u8; 32],
    expected_peer: [u8; 32],
}

impl Pairing {
    /// `exporter` is 32 bytes of TLS keying material exported with [`EXPORTER_LABEL`].
    pub fn start(role: PairRole, pin: &str, viewer_fp: &str, host_fp: &str, exporter: &[u8]) -> (Self, Vec<u8>) {
        let pw = Password::new(pin.as_bytes());
        let (a, b) = (Identity::new(viewer_fp.as_bytes()), Identity::new(host_fp.as_bytes()));
        let (state, msg) = match role {
            PairRole::Viewer => Spake2::<Ed25519Group>::start_a(&pw, &a, &b),
            PairRole::Host => Spake2::<Ed25519Group>::start_b(&pw, &a, &b),
        };
        let p = Pairing {
            role,
            state,
            my_msg: msg.clone(),
            viewer_fp: viewer_fp.to_string(),
            host_fp: host_fp.to_string(),
            exporter: exporter.to_vec(),
        };
        (p, msg)
    }

    /// Consumes the peer's SPAKE2 message. A malformed message is an error; a wrong PIN is
    /// only detected when confirmations are compared.
    pub fn finish(self, peer_msg: &[u8]) -> Result<Confirmation, CryptoError> {
        let key = self.state.finish(peer_msg).map_err(|e| CryptoError::Corrupt(format!("spake2: {e:?}")))?;
        let (msg_a, msg_b) = match self.role {
            PairRole::Viewer => (self.my_msg.as_slice(), peer_msg),
            PairRole::Host => (peer_msg, self.my_msg.as_slice()),
        };
        let mut transcript = Vec::with_capacity(256);
        for part in [TRANSCRIPT_TAG, self.viewer_fp.as_bytes(), self.host_fp.as_bytes(), msg_a, msg_b] {
            transcript.extend_from_slice(&(part.len() as u32).to_be_bytes());
            transcript.extend_from_slice(part);
        }
        let hk = Hkdf::<Sha256>::new(Some(&self.exporter), &key);
        let mut k_viewer = [0u8; 32];
        let mut k_host = [0u8; 32];
        hk.expand(b"annex/1 pairing confirm viewer", &mut k_viewer).expect("32 bytes is a valid HKDF length");
        hk.expand(b"annex/1 pairing confirm host", &mut k_host).expect("32 bytes is a valid HKDF length");
        let mac = |k: &[u8; 32]| -> [u8; 32] {
            let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(k).expect("HMAC accepts any key length");
            m.update(&transcript);
            m.finalize().into_bytes().into()
        };
        let (viewer_mac, host_mac) = (mac(&k_viewer), mac(&k_host));
        Ok(match self.role {
            PairRole::Viewer => Confirmation { mine: viewer_mac, expected_peer: host_mac },
            PairRole::Host => Confirmation { mine: host_mac, expected_peer: viewer_mac },
        })
    }
}

impl Confirmation {
    pub fn mine(&self) -> &[u8] {
        &self.mine
    }

    /// Constant-time comparison of the peer's confirmation.
    pub fn verify_peer(&self, mac: &[u8]) -> bool {
        mac.len() == 32 && bool::from(self.expected_peer.ct_eq(mac))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &str = "11111111111111111111111111111111111111111111111111111111111111aa";
    const H: &str = "22222222222222222222222222222222222222222222222222222222222222bb";

    fn run(pin_v: &str, pin_h: &str, exp_v: &[u8], exp_h: &[u8], fps_h: (&str, &str)) -> (bool, bool) {
        let (pv, mv) = Pairing::start(PairRole::Viewer, pin_v, V, H, exp_v);
        let (ph, mh) = Pairing::start(PairRole::Host, pin_h, fps_h.0, fps_h.1, exp_h);
        let cv = pv.finish(&mh).unwrap();
        let ch = ph.finish(&mv).unwrap();
        (ch.verify_peer(cv.mine()), cv.verify_peer(ch.mine()))
    }

    #[test]
    fn matching_pin_confirms_both_ways() {
        assert_eq!(run("123456", "123456", &[7; 32], &[7; 32], (V, H)), (true, true));
    }

    #[test]
    fn wrong_pin_fails() {
        assert_eq!(run("123456", "123457", &[7; 32], &[7; 32], (V, H)), (false, false));
    }

    #[test]
    fn different_tls_sessions_fail_even_with_right_pin() {
        // A man in the middle relaying between two TLS sessions has two exporters.
        assert_eq!(run("123456", "123456", &[7; 32], &[8; 32], (V, H)), (false, false));
    }

    #[test]
    fn substituted_identity_fails() {
        let attacker = "33333333333333333333333333333333333333333333333333333333333333cc";
        assert_eq!(run("123456", "123456", &[7; 32], &[7; 32], (attacker, H)), (false, false));
    }

    #[test]
    fn malformed_spake_message_is_an_error() {
        let (pv, _) = Pairing::start(PairRole::Viewer, "000000", V, H, &[0; 32]);
        assert!(pv.finish(&[1, 2, 3]).is_err());
    }

    #[test]
    fn confirmation_length_is_checked() {
        let (pv, mv) = Pairing::start(PairRole::Viewer, "1", V, H, &[0; 32]);
        let (ph, mh) = Pairing::start(PairRole::Host, "1", V, H, &[0; 32]);
        let cv = pv.finish(&mh).unwrap();
        let _ = ph.finish(&mv).unwrap();
        assert!(!cv.verify_peer(&[]));
        assert!(!cv.verify_peer(&[0; 31]));
    }

    #[test]
    fn pins_are_six_digits_and_vary() {
        let pins: Vec<String> = (0..50).map(|_| generate_pin().unwrap()).collect();
        assert!(pins.iter().all(|p| p.len() == 6 && p.bytes().all(|b| b.is_ascii_digit())));
        let distinct: std::collections::HashSet<_> = pins.iter().collect();
        assert!(distinct.len() > 45);
    }

    #[test]
    fn pin_normalization() {
        assert_eq!(normalize_pin(" 123 456 ").as_deref(), Some("123456"));
        assert_eq!(normalize_pin("123-456").as_deref(), Some("123456"));
        assert_eq!(normalize_pin("12345"), None);
        assert_eq!(normalize_pin("12345a"), None);
        assert_eq!(normalize_pin("1234567"), None);
    }
}
