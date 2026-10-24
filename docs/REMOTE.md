# Connecting over the internet

Annex has no servers of its own: no accounts, no cloud and no relays ([PLAN.md](PLAN.md)). To
reach a computer somewhere else, put both computers on the same **private network over the
internet** with a mesh VPN such as [Tailscale](https://tailscale.com) or
[ZeroTier](https://www.zerotier.com). Annex then connects as if they were on one LAN. The
VPN carries packets; Annex's own TLS, certificate pinning and PIN pairing still decide who gets
a picture.

## Setting it up

1. Install the VPN on both computers and sign in to the same account (Tailscale) or join the
   same network (ZeroTier). Each computer gets a VPN address, such as `100.101.102.103` on
   Tailscale.
2. **On a Windows PC that shares**, allow Annex on the VPN's network. Annex's firewall rule
   covers *Private* and *Domain* networks. If Windows put the VPN adapter under *Public*, set
   it to Private (Settings → Network & internet → the adapter → Network profile → Private), or
   allow Annex on public networks when Windows asks.
3. On the viewing computer, open **Connect → Connect by address** and type the sharing
   computer's VPN address (or its name, such as `my-pc.tailnet-name.ts.net` with Tailscale
   MagicDNS). The first time, pair with the PIN as usual.

Nearby doesn't list computers on the VPN: discovery uses mDNS, which these VPNs don't carry.
Connect by address instead. After pairing once, the address is all you need.

## What to expect

- **Direct path:** both VPNs try to connect the two computers straight to each other through
  NAT. Then latency is your internet round trip plus encode and decode time. QUIC is used as
  on a LAN, and lost packets are rebuilt from FEC.
- **Relayed path:** if the networks don't allow a direct path, the VPN relays (Tailscale's
  DERP). It works, but with more delay and less bandwidth. Choose **Quality → Fast**.
  `tailscale ping <name>` shows which path you have.
- Upload speed on the sharing side is usually the limit. Best quality asks for tens of Mbit/s;
  the adaptive bitrate backs off when packets are lost.

## Status

Not yet tested end to end over Tailscale or ZeroTier. If you try it, the `connected` line in
`annex.log` on the viewer shows the address and transport used.
