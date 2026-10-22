# Security model

Annex streams your screen, so the bar is: **only devices you approved can see it, and nobody
on the network can read or alter the stream.**

## What protects you today

| Property | Mechanism |
|---|---|
| Confidentiality + integrity | TLS 1.3 (rustls, ring), AEAD ciphers only |
| Mutual authentication | Both sides present Ed25519 certificates; handshake signatures verified |
| Pairing | 6-digit PIN + SPAKE2, bound to the TLS session and both device IDs; 5 failures / 10 min lockout |
| Peer identity pinning | SHA-256 certificate fingerprint stored after successful pairing |
| Replay protection | Fresh ECDHE per connection; no resumption, tickets or 0-RTT; TLS record sequence numbers |
| Forward secrecy | Ephemeral key exchange per session |
| Revocation | Remove a device in the app (or `annex peers remove <id>`); its next connection needs approval again |
| No cloud | No accounts, servers, relays or telemetry; no outbound internet traffic |

Tests: `core/annex-crypto/src/pairing.rs` covers right/wrong PIN, MITM across two TLS
sessions and substituted identities. `core/annex-session/tests/loopback.rs` covers pairing
success/failure/cancel/lockout, revocation, tampered and replayed records, pin mismatch and
busy handling. `core/annex-proto/tests/fuzz.rs` fuzzes every decoder.

## Known limitations

- A 6-digit PIN allows one online guess per attempt (≤ 5 per 10 minutes), i.e. about a 1 in
  200,000 chance per locked-out window for a persistent attacker on the LAN. The PIN is only
  shown on the host's screen.
- **Key storage:** the private key is a 0600 file in the app data folder
  (`~/Library/Application Support/Annex`, `%LOCALAPPDATA%\Annex`). M2 moves it to the macOS
  Keychain / Windows CNG.
- Discovery TXT records reveal device names and IDs to anyone on the LAN (by design; they are
  not secrets).
- Windows secure desktop (UAC prompt, lock screen) is not captured.

## Reporting a vulnerability

Please open a private security advisory on GitHub
(Security → Report a vulnerability) rather than a public issue.
