# Wired connections

Annex uses the same protocol over a cable as over Wi-Fi. A cable gives lower and steadier
latency. When a direct link exists, viewers try its addresses first and show **"direct cable"**
in their status line.

## What works

| Cable | Mac ↔ Mac | Mac ↔ Windows | Windows ↔ Windows | Notes |
|---|---|---|---|---|
| **USB-Ethernet adapters + Ethernet cable** | ✅ | ✅ | ✅ | Works everywhere. Use one adapter per computer (or the built-in Ethernet port) and a normal Ethernet cable; modern ports auto-cross. 1 Gbit/s or 2.5 Gbit/s adapters are plenty. |
| **Thunderbolt 3/4/5 or USB4 cable (Thunderbolt Bridge / USB4 networking)** | ✅ (macOS creates "Thunderbolt Bridge") | ⚠️ Not reliable; depends on the PC's Thunderbolt/USB4 networking driver | ⚠️ Windows 11 24H2+ with USB4 host-to-host networking | Very fast when it works. Test with `ping` first. |
| Plain USB-C cable (USB 2/3, not Thunderbolt/USB4) | ❌ | ❌ | ❌ | Two USB hosts can't talk over a plain USB cable. |
| USB-A ↔ USB-A cable | ❌ | ❌ | ❌ | Needs a special "bridge" / data-transfer cable with a chip in the middle; those usually need their own drivers and are not supported. |
| Both computers on the same wired LAN (router/switch) | ✅ | ✅ | ✅ | Also counts as wired, but goes through your network ("network" in the status line). |

## Setting up a direct cable

With nothing else on the link, both computers give themselves **link-local** addresses
(IPv4 `169.254.x.x`, IPv6 `fe80::…`) after about 10–30 seconds. No configuration is needed:

1. Connect the cable (adapters on both sides if needed).
2. Wait until both show a link (macOS: System Settings → Network shows the adapter "Self-assigned IP";
   Windows: the adapter shows "Unidentified network").
3. On Windows, the first time, make sure the Annex firewall rule covers the new network.
   All-users installs add a rule for Private and Domain networks; "Unidentified network" is
   **Public** by default. Either set that network to Private (Settings → Network → the
   adapter → Network profile → Private) or allow Annex on public networks when Windows asks.
4. Discovery runs over the cable as well (mDNS works on link-local). The host then shows up as
   usual; its list of addresses includes the link-local one, which viewers try first. You can
   also type the address shown on the host (`169.254.x.y`) under *Connect*.

## Latency expectations (targets)

| Link | Target (p95, capture → display) |
|---|---|
| Direct cable | < 50 ms |
| Good 5 GHz Wi-Fi | < 80 ms |

The transport itself adds 1–3 ms (see `annex-bench`). Most of the budget goes to capture,
encode, decode and display refresh.
