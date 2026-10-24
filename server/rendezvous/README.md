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
viewer then prints a `session_started` line with `"link":"internet"`. If it times out, one
of the networks probably blocks direct connections (some mobile and office networks do); those
need the relay, which isn't built yet.

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
| The code works but the computers never connect | The server's job is done. One network blocks direct connections; see [INTERNET-CONNECT.md](../../docs/INTERNET-CONNECT.md). |
