# Security model

Annex streams your screen, so the bar is: **only devices you approved can see it, and nobody
on the network can read or alter the stream.**

## What protects you today (M1)

| Property | Mechanism |
|---|---|
| Confidentiality + integrity | TLS 1.3 (rustls, ring), AEAD ciphers only |
| Mutual authentication | Both sides present Ed25519 certificates; handshake signatures verified |
| Peer identity pinning | SHA-256 fingerprint stored after explicit user approval (trust on first use with confirmation) |
| Replay protection | Fresh ECDHE per connection; no resumption, tickets or 0-RTT; TLS record sequence numbers |
| Forward secrecy | Ephemeral key exchange per session |
| Revocation | Remove a device in the app (or `annex peers remove <id>`); its next connection needs approval again |
| No cloud | No accounts, servers, relays or telemetry; no outbound internet traffic |

Tests: `core/annex-session/tests/loopback.rs` covers tampered and replayed records, pin
mismatch, rejection, revocation and busy handling. `core/annex-proto/tests/fuzz.rs` fuzzes
every decoder.

## Known limitations

- **M1 pairing is visual fingerprint comparison.** It is only as strong as the user's check of
  the 20-digit code on both screens. A user who accepts without comparing can be fooled by an
  active attacker on the LAN. M2 adds PIN/QR pairing (SPAKE2 bound to the TLS session).
- **Key storage:** the private key is a 0600 file in the app data folder
  (`~/Library/Application Support/Annex`, `%LOCALAPPDATA%\Annex`). M2 moves it to the macOS
  Keychain / Windows CNG.
- Discovery TXT records reveal device names and IDs to anyone on the LAN (by design; they are
  not secrets).
- Windows secure desktop (UAC prompt, lock screen) is not captured.

## Reporting a vulnerability

Please open a private security advisory on GitHub
(Security → Report a vulnerability) rather than a public issue.
