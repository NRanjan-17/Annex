# Connecting over the internet without a VPN (proposal)

**Status:** the core, the rendezvous server and the relay fallback are built and tested on a
simulated internet and against a local server (`feat/internet-connect`); the Mac and Windows
screens aren't yet, and nothing has been tried between two real networks. Until it ships, the way
to reach a computer elsewhere is a mesh VPN; see [REMOTE.md](REMOTE.md).

## The problem

Two Macs 10 km apart, both on Nearby, can't see or reach each other. That is expected today:

- **Nearby uses mDNS (Bonjour)**, which only finds computers on the same network or router.
- **Mac to Mac (peer-to-peer Wi-Fi, AWDL)** only reaches a room or a building.
- **Annex has nothing for the internet.** The core has no rendezvous, STUN, hole punching or
  relay. Even with the other computer's public address, home routers (NAT) block an incoming
  connection unless a port is forwarded.

## Today: Tailscale or ZeroTier

About 10 minutes, once per computer. Full steps and caveats are in [REMOTE.md](REMOTE.md).

1. Install Tailscale on both computers and sign in to the **same** account.
2. On the sharing computer: Annex → **This Mac / This PC** → turn **Share** on.
3. Copy the sharing computer's Tailscale address (`100.x.y.z`) from the Tailscale menu.
4. On the viewing computer: Annex → Nearby → **Connect by Address…**, paste the address, then
   choose **Extend** or **Show Its Screen**.
5. Type the PIN the sharing computer shows. Only the first time.

Expect more delay than on a LAN: your internet round trip, typically 30–80 ms or more. Choose
**Quality → Fast**. Nearby won't list VPN computers, because the VPNs don't carry mDNS.

This is fine for a technical user. It is too much to ask of 10k users, which is why the options
below exist.

## Options for a built-in way

### Option 1: pairing code plus a relay we host (recommended for reach)

1. The sharing computer registers with an Annex rendezvous server and shows a short code.
2. The viewer types the code. The server swaps both computers' public and local addresses.
3. Both try to connect directly at the same time (UDP hole punching for QUIC).
4. If that fails (strict NAT, carrier-grade NAT, corporate firewalls), traffic falls back to a
   relay we run (TURN-style). The relay forwards encrypted QUIC packets. It never sees the picture,
   because Annex's TLS, certificate pinning and PIN pairing are unchanged.

- **Works:** close to everywhere.
- **Costs:** relay bandwidth. A relayed session can use tens of Mbit/s, so the relay bill grows
  with the number of relayed users, not just the total number of users.
- **Effort:** about 1–2 weeks: server, core changes for hole punching and relay transport, UI
  for the code, tests across NAT types.

### Option 2: rendezvous only, direct connections only

Steps 1–3 above, with no relay.

- **Works:** most home networks. Fails on roughly 10–20% of networks (symmetric or carrier-grade
  NAT, some mobile hotspots, strict corporate networks). Those users would need the VPN route.
- **Costs:** almost nothing; the server only swaps small messages.
- **Effort:** about 1 week.

### Option 3: build on a mesh VPN library

Embed a Tailscale-style client (for example `tsnet` or a WireGuard-based mesh) instead of
writing our own NAT traversal. Users don't install anything separately, but there is still a
coordination server (ours or a vendor's) and a relay fallback. It brings a large dependency
into the app. Effort and cost depend on the library; not estimated yet.

## Conflict with the plan

[PLAN.md](PLAN.md) (principles) says: *"No cloud, accounts, telemetry, or relays. Discovery
and connections stay on local links. The only internet traffic of its own is the update check…"*
(GitHub's releases list, on by default.)

Options 1–3 all break that rule: they need a server Annex talks to. Decide this before any
building:

- **Keep the rule:** stay with Tailscale/ZeroTier, and make it easier (a guide in the app, and
  remembering VPN addresses).
- **Change the rule:** update PLAN.md and SECURITY.md, keep internet connections **opt-in**
  (off until the user asks for a code), never send the picture unencrypted, and write down
  what the server sees: addresses and timing, never content.

## Open questions

1. Is breaking the "no servers" rule acceptable?
2. Who runs and pays for the server and relay, and in which regions?
3. Do codes expire after one use, or does a paired computer stay reachable over the internet
   (which needs it registered all the time)?
4. Does Windows need the same feature at launch?

## Plan for Option 1 (relay)

**Running the server:** deploy steps, free-plan limits and what it stores are in
[server/rendezvous/README.md](../server/rendezvous/README.md).

Written 2026-10-25 against the core as it is: QUIC (quinn) with self-signed Ed25519 identities,
certificate pinning and PIN pairing bound to the TLS exporter (`core/annex-session/src/transport.rs`).
Nothing here is built. Decision 1 is made (2026-10-25): the "no servers" rule is dropped for this
opt-in feature, and PLAN.md says so.

### What the user sees

1. **Sharing computer:** This Mac / This PC → **Share over the internet** (off by default). Turning it
   on shows a code like `K7P-4QX` and "Waiting for the other computer…". The code lasts 10 minutes
   and works once.
2. **Viewing computer:** Nearby → **Connect by Code…**, type the code, choose Extend or Show Its
   Screen.
3. **First time only:** the usual PIN. The two people read it to each other (phone, chat).
4. The session bar says **Direct** or **Relayed** next to the delay, so a slow session explains itself.
5. **Later (phase 3):** a paired computer with "Stay reachable over the internet" on shows up in
   Nearby as "Away · over the internet", with no code.

### Why the server can't see or take over a session

- The picture, input and pairing are inside the same TLS 1.3 session as on a LAN. The relay
  forwards UDP packets it can't read.
- A paired viewer still pins the host's certificate, so a server that hands out the wrong address
  only gets a failed handshake.
- A new pairing still needs the PIN, which is bound to the TLS exporter. A server in the middle
  would hold two different TLS sessions, so the PINs wouldn't match.
- The code itself only finds the computer. Guessing one gets a stranger to the PIN prompt, not the
  screen; the server also limits code attempts per address.
- **What the other computer learns:** the public address of your internet connection (and your
  public IPv6 address, if you have one), as soon as it uses your code, before the PIN. That
  shows your rough location and provider, not your street. Your home-network (LAN) addresses
  aren't sent. Share codes only with people you mean to connect to. A wrong guess is very
  unlikely: about a billion codes, 10 tries per 10 minutes per address, 10-minute lifetime. Using
  the relay for every session would hide your address (later, with the relay).
- The server sees: both public addresses, a hash of each computer's identity, when sessions start
  and end, and on the relay how many bytes passed. Never content. This list goes into SECURITY.md.

### Pieces

| Piece | Where | What it does |
|---|---|---|
| Rendezvous | Cloudflare Worker + one Durable Object per code (`server/rendezvous`) | Holds registrations (code → host) over a WebSocket while the host is sharing, swaps each side's addresses, rate-limits codes. Workers can't receive UDP, so it never sees the session socket. |
| Public address | `stun.cloudflare.com` (free, no cap) | Each computer asks STUN, from its QUIC socket, which public address and port its router gave that socket. |
| Relay (phase 2) | Cloudflare Realtime TURN | Standard TURN: forwards packets unchanged. Nothing of ours to run; the core needs a TURN client. |
| Punching socket | `annex-session`, a custom `quinn::AsyncUdpSocket` | The session socket also sends STUN requests and the small "punch" packets that open home routers, and passes STUN replies aside (RFC 7983 tells them apart from QUIC). quinn supports custom sockets, so QUIC itself is unchanged. |
| `connect_internet(code)` | `annex-session` viewer, plus the host's registration | Ask for the peer, punch for about 3 seconds, then fall back to the relay. Uses the existing `Established` path, so everything after the handshake is untouched. |
| UI | Mac and Windows | The switch and code above, Connect by Code…, Direct/Relayed in the session bar. |

TCP fallback stays LAN-only; internet sessions are QUIC only.

### Phases

| Phase | Scope | Time | Done when |
|---|---|---|---|
| 1 | Rendezvous server, codes, hole punching, Mac + Windows UI | about 1 week | Two computers on different home networks connect directly by code. |
| 2 | Relay fallback | 3–4 days | A computer behind carrier-grade NAT (a phone hotspot) connects through the relay. |
| 3 | Paired computers stay reachable (no code) | about 3 days | A paired computer shows in Nearby while it's away. |

Phase 1 without phase 2 is Option 2. It's a working stopping point if relay costs are a worry.

### Tests

- **Core:** loopback tests with a fake NAT between the computers, like the lossy proxy in
  `tests/loopback.rs`: one that allows punching (expect Direct) and one "symmetric" that doesn't
  (expect Relayed). Plus tests for codes (expiry, single use, rate limit) and a server that lies
  about addresses (expect a failed handshake).
- **By hand:** two home networks; a phone hotspot (carrier-grade NAT); an office or guest network.
  Record delay for Direct and Relayed in TESTING.md.

### Running it

- **Bandwidth is the cost.** The apps' normal rates are about 12 Mbit/s mirroring 1080p60 and
  about 30 Mbit/s extending 2560×1600 at 60 Hz, so a relayed session moves roughly 5–14 GB an hour.
  **Quality → Fast** lowers that.
- Only relayed sessions cost much (expect 10–20% of internet sessions). Rendezvous traffic is tiny.
- **Hosting: Cloudflare** (decided 2026-10-25, replacing "a small server").
  - Rendezvous: Workers and Durable Objects. Signalling is a few small messages per session, so
    this should stay inside the free or $5/month Workers plan.
  - STUN: free.
  - Relay (phase 2): Cloudflare TURN, first 1,000 GB a month free, then $0.05/GB. That's about 70–200
    relayed hours a month free, then roughly $0.25–0.70 per relayed hour.
  - Anycast: computers reach the nearest Cloudflare location, so there's no region to choose.
- **What Cloudflare sees:** the same as "the server" above (addresses, timing, relay byte counts).
  SECURITY.md names Cloudflare.
- **Self-hosting:** the server address is a setting. Running your own means deploying the Worker
  to your own Cloudflare account with `wrangler`; any TURN server works for the relay.

### Doc changes before building

- PLAN.md: replace "No cloud, accounts, telemetry, or relays…" with: internet connections are
  opt-in, the server and relay see addresses and timing but never content, and LAN use never
  contacts them.
- SECURITY.md: what the server sees, code rate limits, and why the PIN still protects a new pairing.
- REMOTE.md: keep the Tailscale route as the no-server option.

### Decisions needed

1. ~~**Change the "no servers" rule?**~~ **Yes** (2026-10-25). PLAN.md updated.
2. **Hosting:** Cloudflare Workers + Durable Objects for rendezvous, Cloudflare STUN, and
   Cloudflare TURN when the relay comes. Whose Cloudflare account is decided before release; built
   with the server address as a setting and tested with `wrangler dev`.
3. **Phase 2:** ~~later~~ built with phase 1 (2026-10-25): the relay is used only when the
   direct connection fails. Two kinds: the user's own TURN server (a setting on the sharing
   computer, no limits), or ours, which is **off unless its TURN secrets are set** and capped at
   8 logins a day, 2 per address, 30 minutes each, about 6 Mbit/s (worst case ~460 GB a month,
   inside the free 1,000 GB). Turning ours on waits on confirming with Cloudflare that a no-card
   account can't be billed past the free allowance. Details:
   [server/rendezvous/README.md](../server/rendezvous/README.md).
4. **Platforms:** Mac and Windows together.

Decisions 2–4 made 2026-10-25.

## Next step

Build phase 1. Separately, test the Tailscale route end to end between two Macs on different
networks and note the delay; REMOTE.md's Status section says it hasn't been tested yet.
