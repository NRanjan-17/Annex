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
| Headless mode | Off by default. When on (Settings → No one at this screen), the pairing PIN is also written to `pairing-pin.txt` in the data folder (owner-only, deleted when pairing ends) for someone logged in over SSH, so anyone who can read this user's files can pair; and paired devices may use the mouse and keyboard without being allowed one by one. Unpaired devices never get input |
| Install scripts | `scripts/install.sh` and `install.ps1` install only what passes the same checks as an update: the Ed25519 signature on `SHA256SUMS.txt` and the file's SHA-256 (Linux, Windows), or the SHA-256 and Apple's notarized Developer ID signature for Annex's team (macOS, whose built-in tools can't check Ed25519). The scripts are served from `main`, so they are only as trustworthy as the repository |
| No accounts or telemetry | No accounts, ads or tracking. On your own network Annex talks to no server. The update check reads GitHub's releases list (once a day and again when you install an update; can be turned off) |
| Internet codes (opt-in) | The code server (`connect.getannex.dev`, a Cloudflare Worker, over TLS) swaps the two computers' names and addresses and forgets them; it never sees screen contents. Codes are 6 characters, single use, valid for 10 minutes, with guesses rate-limited per address. Punch packets carry a token derived from the code (SHA-256), so only the two computers can steer where a direct connection goes |
| Relay (only if a direct connection fails) | Cloudflare TURN forwards the session's QUIC packets, which stay TLS 1.3 between the two computers: the relay can't read or change them. Limits on our relay: 20-minute sessions, held to about 6 Mbit/s, 2 a day per sharing computer's address and 20 a day in total (reset at midnight UTC). Your own TURN server (Settings → Internet) has none of these limits. Annex opens no ports on routers (no UPnP) |
| Log file | Always on: `annex.log` in the data folder records connection steps (addresses, device names and IDs, errors), never PINs, keys or screen contents. Only this account can read it (0600 on Mac and Linux; the per-user AppData folder on Windows). It never leaves the computer; Annex doesn't upload it. Past 5 MB it moves to `annex.old.log` (about 10 MB at most). The user attaches it to a bug report if they choose. |
| Updates | Installed only if the release's `SHA256SUMS.txt` carries a valid Ed25519 signature (made in CI with a key kept in the protected `release` environment; public key in `core/annex-update`) and the download's SHA-256 is listed in it. Unsigned releases are never offered |

Tests: `core/annex-crypto/src/pairing.rs` covers right/wrong PIN, MITM across two TLS
sessions and substituted identities. `core/annex-session/tests/loopback.rs` covers pairing
success/failure/cancel/lockout, the headless PIN file and input rule, revocation, tampered
and replayed records, pin mismatch and busy handling. `core/annex-proto/tests/fuzz.rs` fuzzes every decoder.

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
