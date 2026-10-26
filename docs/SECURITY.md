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
| No cloud | No accounts, servers, relays or telemetry. The only internet traffic is the update check (GitHub's releases list, once a day; can be turned off) |
| Updates | Installed only if the release's `SHA256SUMS.txt` carries a valid Ed25519 signature (made in CI with a key kept in the protected `release` environment; public key in `core/annex-update`) and the download's SHA-256 is listed in it. Unsigned releases are never offered |

Tests: `core/annex-crypto/src/pairing.rs` covers right/wrong PIN, MITM across two TLS
sessions and substituted identities. `core/annex-session/tests/loopback.rs` covers pairing
success/failure/cancel/lockout, revocation, tampered and replayed records, pin mismatch and
busy handling. `core/annex-proto/tests/fuzz.rs` fuzzes every decoder.

## Known limitations

- A 6-digit PIN allows one online guess per attempt (≤ 5 per 10 minutes), i.e. about a 1 in
  200,000 chance per locked-out window for a persistent attacker on the LAN. The PIN is only
  shown on the host's screen.
- **Key storage:** the private key is in the macOS login Keychain (service
  `com.nranjan-17.annex`) or, on Windows, in `%LOCALAPPDATA%\Annex\identity.key.dpapi`
  encrypted with DPAPI for the current user. Keys from older versions are migrated
  automatically. Anyone who can run code as you can still use the key. The headless CLI keeps
  its own key in a 0600 file. Development builds signed ad hoc trigger a Keychain access
  prompt after each rebuild ("Always Allow" is fine for your own builds).
- Discovery TXT records reveal device names and IDs to anyone on the LAN (by design; they are
  not secrets). Annex also answers the same fields on UDP port 47790, only to private,
  shared-address-space (100.64.0.0/10), unique-local and link-local addresses, so computers on
  the same mesh VPN learn them too. Questions must be at least as large as the answer.
- Windows secure desktop (UAC prompt, lock screen) is not captured.

## Reporting a vulnerability

Please open a private security advisory on GitHub
(Security → Report a vulnerability) rather than a public issue.
