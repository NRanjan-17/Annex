# Annex wire protocol

Version **1.0** (ALPN `annex/1`). Implemented in `core/annex-proto` (format) and
`core/annex-session` (state machines). This document and the code must change together.

## Versioning

- **Major** version is in the TLS ALPN string (`annex/1`). Peers with different majors fail the
  TLS handshake (no common ALPN) and never exchange application data.
- **Minor** version and capabilities travel in `Hello`. Receivers **ignore unknown CBOR map
  keys** and **skip control messages with unknown `t`** values, so minor versions can add
  fields and message types without breaking older peers.
- Record types are closed in 1.x: an unknown record type is a protocol error.

## Transports

Hosts listen on port **47800** for both:

- **QUIC** (UDP, preferred): TLS 1.3 inside QUIC (same certificates, ALPN and verifiers), BBR
  congestion control, keep-alive 2 s, idle timeout 15 s. Control messages use the first
  bidirectional stream (opened by the viewer) with the record framing below. **Video goes as
  unreliable datagrams with FEC** (see "Video datagrams").
- **TCP + TLS 1.3** (fallback when UDP is blocked): control and video records on one stream.

A viewer resolves all candidate addresses, sorts link-local ones (a direct cable) first, races
QUIC handshakes (150 ms stagger, 3 s budget), and falls back to the same race over TCP. Both
sides report the result in `session_started` as `transport` (`quic`/`tcp`) and `link`
(`direct`/`network`).

The TCP and QUIC handshakes are both TLS 1.3 with mutual authentication (both sides present a
certificate). Session resumption, tickets and 0-RTT are disabled. Every connection runs a
full handshake with fresh ephemeral keys, so captured traffic can't be replayed into a new
session and each session has its own keys (forward secrecy). Within a session TLS's
per-record sequence numbers and AEAD (AES-GCM / ChaCha20-Poly1305) reject replayed, reordered
or modified records. `annex-session/tests/loopback.rs` checks this with an on-path attacker
that flips a byte or replays a TLS record.

For QUIC the exporter comes from the QUIC connection's TLS session (same label, empty context).

## Identity and trust

- Each device has a long-lived **Ed25519** key and a self-signed X.509 certificate
  (subject `CN=Annex Device`, SAN `annex.local`).
- **Device ID** = lowercase hex SHA-256 of the DER certificate (64 chars).
- Display form for humans: first 20 hex digits, upper case, in groups of 4
  (`AB12 CD34 EF56 7890 1234`).
- TLS verifiers check the handshake signature (proof of key possession) but not trust. After
  the handshake each side looks the peer's device ID up in its trust store and reports the
  result in `Hello.knows_peer`. If **either** side sends `false`, the two run **PIN pairing**
  (below) before anything else. Afterwards both IDs are pinned. A viewer connecting to an
  already-trusted host passes the expected ID so the TLS handshake itself fails on any other
  certificate.
- `Hello.device_id` must equal the TLS peer's device ID or the session is aborted.

## PIN pairing (SPAKE2)

The host generates a uniformly random 6-digit PIN (OS CSPRNG) and shows it. The viewer's user
types it. Both sides run SPAKE2 over Ed25519 (RustCrypto `spake2`; viewer = side A, host =
side B, identities = the two device IDs) with the PIN as the password:

```
viewer                                              host
  |------- pair_spake { msg: A } ------------------->|
  |<------ pair_spake { msg: B } --------------------|
  |------- pair_confirm { mac: MAC_viewer } -------->|   host verifies; mismatch → bye{pairing_failed}
  |<------ pair_confirm { mac: MAC_host } -----------|   viewer verifies
```

- `E` = 32 bytes of TLS keying material, label `EXPORTER-annex-pairing-v1`, no context.
- `K` = SPAKE2 output. `k_v`, `k_h` = HKDF-SHA256(salt = `E`, ikm = `K`), info
  `annex/1 pairing confirm viewer` / `... host`.
- transcript = length-prefixed (`u32` BE) concatenation of `annex/1 pairing transcript`,
  viewer ID, host ID, `A`, `B`. `MAC_x` = HMAC-SHA256(`k_x`, transcript), compared in
  constant time.
- An eavesdropper learns nothing it can brute-force offline. An active attacker gets one PIN
  guess per run. Binding to `E` defeats a relay between two TLS sessions, and binding to both
  IDs ties the pinned keys to the exchange.
- The host allows **5 failed runs per 10 minutes**; after that it answers `bye{pairing_locked}`
  without showing a PIN. Either side may cancel (`bye{pairing_cancelled}`). The viewer has
  120 s to enter the PIN (`bye{pairing_timeout}`).
- QR pairing is not implemented. The PIN covers the requirement and needs no camera.

## Record framing

```
+--------+----------------+------------------------+
| type:1 | length:4 (BE)  | payload (length bytes) |
+--------+----------------+------------------------+
```

| Type | Meaning | Max payload |
|---|---|---|
| `0x01` | Control message (CBOR) | 64 KiB |
| `0x02` | Video access unit | 16 MiB |

## Control messages

A CBOR map with a text key `t` holding the message type. Field names are snake_case.

| `t` | Direction | Fields |
|---|---|---|
| `hello` | both, first message | `proto_major` u16, `proto_minor` u16, `device_name` text (≤ 64 bytes), `device_id` text (64 hex), `app_version` text, `roles` [`host`\|`viewer`], `caps` [text], `knows_peer` bool |
| `pair_spake` | both, pairing only | `msg` bytes (1–128) |
| `pair_confirm` | both, pairing only | `mac` bytes (32) |
| `session_request` | viewer → host | `mode` (`mirror`\|`extend`), `display` DisplayInfo, `codecs` [`h264`\|`hevc`\|`raw`], `input` bool |
| `session_accept` | host → viewer | `stream` StreamConfig |
| `session_reject` | host → viewer | `reason` text (`busy`, `extend_not_supported`, `no_common_codec`, ...) |
| `stream_config` | host → viewer | StreamConfig (sent when resolution/fps change mid-session) |
| `keyframe_request` | viewer → host | — |
| `clock_ping` | viewer → host | `seq` u32, `t_viewer_us` i64 |
| `clock_pong` | host → viewer | `seq` u32, `t_viewer_us` i64 (echoed), `t_host_us` i64 |
| `receiver_report` | viewer → host, every second | `frames` u32, `lost` u32, `shards` u32, `recovered` u32, `rtt_us` u32 |
| `bye` | both | `reason` text (`user`, `host_stopped`, `not_trusted`, `pairing_failed`, `pairing_cancelled`, `pairing_locked`, `pairing_timeout`, ...) |

**DisplayInfo**: `width_px`, `height_px` (required); `width_mm`, `height_mm`, `refresh_hz`,
`scale_percent` (100 = 1x, 200 = Retina), optional with defaults 0/0/0/100; `refresh_rates`
(every rate the screen supports at its current size, so an Extend display offers the same ones),
optional and left out when empty.

**StreamConfig**: `codec`, `width`, `height`, `fps`, all required and non-zero.

## Video records

```
+-------------+-----------------+---------+-------------+------------------+
| frame_id:4  | capture_ts_us:8 | flags:1 | reserved:3  | access unit ...  |
+-------------+-----------------+---------+-------------+------------------+
```

- Big-endian. `flags` bit 0 = keyframe (IDR). Reserved bytes are zero and ignored.
- `frame_id` increases by one per frame the host accepted from its encoder. Gaps mean frames
  were dropped on congestion.
- `capture_ts_us` is on the host's monotonic clock.
- H.264/HEVC payloads are **Annex B** byte streams (start codes). Keyframes carry SPS/PPS
  (and VPS for HEVC) so decoding can start at any keyframe.

## Video datagrams (QUIC)

Each access unit is split into `data_count` equal shards (the last one zero-padded, even
length) sized to the path MTU, plus `parity_count` Reed-Solomon parity shards
(`reed-solomon-simd`). Each datagram carries one shard after a 24-byte header:

```
+-----------+--------------+---------+------------+-------------+--------------+---------------+------------+-----------
| frame_id:4| capture_us:8 | flags:1 | reserved:1 | shard_idx:2 | data_count:2 | parity_count:2 | frame_len:4 | shard ...
+-----------+--------------+---------+------------+-------------+--------------+---------------+------------+-----------
```

Any `data_count` shards rebuild the frame. Frames are delivered in order. A frame that is
still incomplete when a newer frame completes is counted as lost, as is a frame none of whose
datagrams arrived (a gap in `frame_id`), and the viewer asks for a keyframe (at most every
250 ms). At most 32 incomplete frames are kept; beyond that the oldest is counted as lost. Parity starts at 15 % and is adapted between 10 % and 40 %.

## Adaptive bitrate

Every second the viewer sends `receiver_report` with frames delivered, frames lost, datagrams
received, datagrams rebuilt from parity, and the RTT. The host's controller (`annex-session/src/rate.rs`):

| Condition | Bitrate scale | FEC |
|---|---|---|
| a frame lost, > 5 % datagrams rebuilt, or RTT > 2× best (of the last 10 reports) + 15 ms | × 0.8 (min 20 %) | +5 points (max 40 %) |
| 2–5 % datagrams rebuilt | hold | +2 points |
| clean interval | +5 points (max 150 %) | −1 point (min 10 %) |

Scale changes are delivered to the host app as `bitrate_hint {session_id, scale_percent}`;
apps set their encoder to that percentage of their normal bitrate (mirror encoders shared by
several viewers follow the lowest hint).

## Session flow

```
viewer                                   host
  | TCP connect + TLS 1.3 (mutual auth)    |
  |--------------- hello ----------------->|
  |<-------------- hello ------------------|
  |  (PIN pairing if either knows_peer=false)|
  |---------- session_request ------------>|
  |<--------- session_accept --------------|   or session_reject / bye{not_trusted}
  |<===== video (keyframe first) ==========|
  |----- clock_ping / keyframe_request --->|
  |<----- clock_pong / stream_config ------|
  |------------------ bye ---------------->|   either side may send bye, then close
```

The host serves one viewer at a time in M1; others get `session_reject{busy}`. After dropping
frames under congestion, the host withholds delta frames until the encoder produces a keyframe.

## Clock synchronisation and latency

The viewer sends `clock_ping` every 250 ms for 5 s, then every second. For each pong:
`rtt = t1 - t0`, `offset = t_host - (t0 + rtt/2)`. The estimate uses the sample with the
lowest RTT among the last 16. A frame's capture time on the viewer clock is
`capture_ts_us - offset`; capture→display latency is measured when the app reports the frame
presented.

## Discovery (mDNS / DNS-SD)

Service type `_annex._tcp.local.`. Instance name `<device name> (<first 4 hex of id>)`.
TXT records:

| Key | Value |
|---|---|
| `v` | protocol major (`1`) |
| `id` | device ID (64 hex). Public; identifies, never authenticates |
| `name` | device name |
| `role` | `host` |

No secrets are ever placed in TXT records.

## FFI events (core ↔ native apps)

JSON objects with a `type` field, delivered through the C ABI (`annex.h`):

| `type` | Fields |
|---|---|
| `listening` | `port` |
| `connecting` | `address` |
| `pairing_pin` | `request_id`, `pin` (`"123 456"`), `peer_name`, `fingerprint_display`: host shows it; `annex_core_cancel_request` to refuse |
| `pairing_pin_required` | `request_id`, `peer_name`, `fingerprint_display`: viewer asks the user; answer with `annex_core_submit_pin` |
| `pairing_finished` | `request_id`, `success`, `peer_name` |
| `session_started` | `session_id` (0 on viewers), `mode`, `peer_name`, `fingerprint`, `stream`, `transport` (`quic`/`tcp`), `link` (`direct`/`network`), `viewer_display` (host side only) |
| `stream_config` | `stream` |
| `keyframe_requested` | `session_id` |
| `bitrate_hint` | `session_id`, `scale_percent` (host: set the session's encoder bitrate) |
| `latency_stats` | `frames`, `fps`, `p50_ms`, `p95_ms`, `p99_ms`, `max_ms`, `clock_rtt_ms`? |
| `session_ended` | `session_id`, `reason` (always the last event of a session) |
| `error` | `message` |
| `peer_found` | `instance`, `name`, `addresses`, `port`, `fingerprint`?, `proto_major`? |
| `peer_lost` | `instance` |
