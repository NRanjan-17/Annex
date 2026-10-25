# Annex rendezvous server

Lets two Annex computers find each other across the internet. The sharing computer gets a short
code (`K7P-4QX`); the viewer types it; the server gives each computer the other's public address,
and they connect **directly**. The screen, input and PIN never pass through this server. Why it
exists and how it fits: [docs/INTERNET-CONNECT.md](../../docs/INTERNET-CONNECT.md).

It's a Cloudflare Worker with Durable Objects, and runs on Cloudflare's **free plan**. Anyone can
run their own: the address is a setting in Annex, and nothing else changes.

## What you need

- A Cloudflare account. The free plan is enough; no card needed.
- Node.js 22 or newer.
- About 10 minutes.

## Deploy

1. Install the tools:
   ```sh
   cd server/rendezvous
   npm ci
   ```
2. Sign in to Cloudflare (opens a browser):
   ```sh
   npx wrangler login
   ```
3. Deploy:
   ```sh
   npx wrangler deploy
   ```
   The first time, Wrangler asks you to pick a `workers.dev` subdomain for your account. It prints
   the Worker's address at the end, for example:
   ```
   https://annex-rendezvous.your-subdomain.workers.dev
   ```
4. Check it answers:
   ```sh
   curl https://annex-rendezvous.your-subdomain.workers.dev/
   ```
   Expect `Annex rendezvous. See https://github.com/NRanjan-17/Annex`.

Your server address for Annex is the same with `wss://` in front:
`wss://annex-rendezvous.your-subdomain.workers.dev`.

## Try it between two computers

Until the apps have the setting, use the `annex` command-line tool from `core/` on both
computers (on different networks, e.g. home Wi-Fi and a phone hotspot):

```sh
export ANNEX_RENDEZVOUS=wss://annex-rendezvous.your-subdomain.workers.dev

# Sharing computer: prints "internet code: K7P-4QX"
cargo run -p annex-cli -- host --synthetic --internet

# Viewing computer: use the code it printed
cargo run -p annex-cli -- viewer --code K7P-4QX --seconds 20
```

The first time, the sharing computer prints a PIN and the viewer asks for it, as on a LAN. The
viewer then prints a `session_started` line with `"link":"internet"`.

If one of the networks blocks direct connections (some mobile and office networks do), the viewer
gives up on direct after about 10 seconds and asks for the relay. With a relay available the
session starts with `"link":"relayed"`; without one, it ends. To test with your own relay
(e.g. [coturn](https://github.com/coturn/coturn)), add to the sharing computer's command:

```sh
--turn turn:relay.example.com:3478 --turn-user annex --turn-pass secret
```

## Turn on our relay (optional, off by default)

The rendezvous server can hand out short-lived logins for Cloudflare's relay (TURN) to sharing
computers that have no relay of their own. **Leave this off unless you have confirmed with
Cloudflare that TURN usage over the free allowance can't be billed to your account.** Cloudflare's
TURN pricing is 1,000 GB a month free, then $0.05/GB; whether an account with no card can go over
is not stated in their docs.

1. In the dashboard: **Realtime → TURN Server → Create**. Note the **Turn Token ID** and
   **API Token**.
2. Store them as secrets (never in `wrangler.jsonc`):
   ```sh
   npx wrangler secret put TURN_KEY_ID
   npx wrangler secret put TURN_KEY_API_TOKEN
   ```
3. Check the limits in `wrangler.jsonc` (`vars`), then `npx wrangler deploy`.

To turn it off again: `npx wrangler secret delete TURN_KEY_ID`.

### How usage stays capped

| Limit | Default | Effect |
|---|---|---|
| `RELAY_TTL_SECONDS` | 1200 | Each login works 20 minutes; then Cloudflare drops the session. |
| `RELAY_PER_DAY` | 20 | Logins a day in total (UTC). After that, no relay until tomorrow. |
| `RELAY_PER_IP_PER_DAY` | 2 | Logins a day per sharing computer's address. |
| Bitrate (in the app) | ~6 Mbit/s | Sessions on our relay are held to about 6 Mbit/s, whatever the screen size. Known gap: it's worked out when the session starts, so switching an Extend session to a bigger resolution afterwards goes over it. |

Worst case with the defaults: 20 sessions × 20 minutes × 6 Mbit/s ≈ 18 GB a day, plus error
correction (up to 40 %) ≈ 25 GB a day, about **760 GB a month**, under the 1,000 GB free allowance.
(20 sessions at 30 minutes would be ~1,130 GB, over it.)
If you raise a limit, redo this sum. Setting `RELAY_PER_DAY` to `0` turns the relay off without
removing the secrets.

The rendezvous server only issues the login; the session goes from the computers to Cloudflare's
relay, encrypted end to end as always. The relay can't read it.

## Use your own domain (optional)

In the Cloudflare dashboard: **Workers & Pages → annex-rendezvous → Settings → Domains & Routes →
Add → Custom domain**, e.g. `rendezvous.example.com` (the domain must be on Cloudflare). Then use
`wss://rendezvous.example.com`.

## Free plan limits

| Limit (per day, resets 00:00 UTC) | What it means for Annex |
|---|---|
| 100,000 Worker requests | Each code and each join is one request. |
| 100,000 Durable Object requests | Each code and each join is a few. Roughly 5,000+ connections a day. |
| 13,000 GB-s Durable Object time | A waiting computer's connection sleeps between messages and uses none. |
| 100,000 SQLite rows written | One per code or join attempt (rate limiting). |

If a limit runs out, new codes fail until the reset. **Nothing is ever charged** on the free plan.
Sessions already connected aren't affected; they don't use the server.

## What the server sees and keeps

- **Sees:** each computer's public IP address, the name it shows ("Dev's Mac"), when codes are
  made and used.
- **Keeps:** a code's details only until it's used, expires (10 minutes) or the sharing computer
  goes away. Rate-limit counters keep each address's recent attempts for up to an hour.
- **Never sees:** the screen, input, PIN or anything else from the session.
- **Logs:** `observability` is on in `wrangler.jsonc`, so Cloudflare keeps request logs (paths
  include codes, never session content). Set it to `false` before deploying if you'd rather not.

## Limits built into the server

- Codes: 6 characters from 32 (no 0/O or 1/I), about a billion possibilities, 10 minutes, one use.
- Per address: 30 codes an hour, 10 join attempts per 10 minutes (each attempt is a guess).
- Messages: at most 2 KB, up to 8 addresses, names cut at 64 characters.
- After a viewer joins, both stay connected 45 seconds at most, for one relay request.

Change them at the top of `src/index.ts`.

## Update, watch, remove

- **Update:** pull the latest code, then `npm ci && npx wrangler deploy`. Codes in progress
  carry on.
- **Watch:** `npx wrangler tail` streams live requests; the dashboard shows usage against the
  free limits.
- **Remove:** `npx wrangler delete`. Annex computers using this address then can't make codes.

## Develop and test

```sh
npm run check   # types
npm test        # Worker tests (vitest, in the Workers runtime)
npm run dev     # local server on http://localhost:8787 (Annex address: ws://localhost:8787)
```

The Rust client's test against a local server:

```sh
npx wrangler dev --port 8788 &
cd ../../core && ANNEX_RENDEZVOUS=ws://127.0.0.1:8788 cargo test -p annex-session --test rendezvous
```

CI runs all of this on every push (`.github/workflows/ci.yml`, job `rendezvous`).

## Troubleshooting

| Problem | Fix |
|---|---|
| `wrangler deploy` says Durable Objects need a paid plan | The class must be SQLite-backed (`new_sqlite_classes` in `wrangler.jsonc`, already set). If you changed the migration, put it back. |
| `curl` works but Annex says it can't reach the server | Use `wss://`, not `https://`, and no path after the domain. |
| "too many attempts" | 10 wrong codes from one address in 10 minutes. Wait, or check the code. |
| The code works but the computers never connect | One network blocks direct connections and no relay was available: set up your own (`--turn`), or turn on ours (above). |
| "relaying through …" on the sharing computer, but the session never starts | The viewer's network uses more than one public address (some mobile carriers), so the relay drops its packets. No fix yet. |
| "no relay available" on the sharing computer | Ours is off (no TURN secrets) or today's logins are used up. |
