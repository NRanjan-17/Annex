# Connecting over the internet without a VPN (proposal)

**Status:** proposal, not built. Today the only way to reach a computer elsewhere is a mesh VPN;
see [REMOTE.md](REMOTE.md).

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
and connections stay on local links. The app does no outbound internet traffic of its own."*

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

## Next step

Test the Tailscale route end to end between two Macs on different networks and note the delay.
REMOTE.md's Status section says it hasn't been tested yet.
