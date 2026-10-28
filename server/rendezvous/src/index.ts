// Annex rendezvous: introduces two computers so they can connect directly across the internet.
// It never carries a session: it matches a short code to the sharing computer, then swaps the
// two computers' addresses. See docs/INTERNET-CONNECT.md.
//
// Protocol v1 (JSON text frames over WebSocket):
//   Sharing computer  GET /v1/host        ← {"type":"code","code":"K7P4QX","expires_in":600}
//                                         → {"type":"hello","name":"…","addrs":["203.0.113.9:47800",…],
//                                            "own_relay":false}
//                                         ← {"type":"peer","name":"…","addrs":[…]}   (a viewer joined)
//                                         ← {"type":"expired"}                       (10 minutes passed)
//   Viewer            GET /v1/join/K7P4QX → {"type":"join","name":"…","addrs":[…]}
//                                         ← {"type":"peer","name":"…","addrs":[…]}
// After the match both sockets stay open for MATCH_WINDOW, for the relay fallback:
//   Viewer (direct failed)                → {"type":"need_relay"}
//   Sharing computer                      ← {"type":"need_relay","turn":{"urls":[…],"username":"…",
//                                            "credential":"…","ttl":1200}}   (or "turn":null)
//                                         → {"type":"relay","addrs":["198.51.100.5:61000"]}
//                                           (its relayed address; or {"type":"no_relay"})
//   Viewer                                ← {"type":"relay","addrs":[…]}  or  {"type":"no_relay"}
// "turn" is a short-lived login for our relay (Cloudflare TURN), only if the sharing computer has
// no relay of its own ("own_relay"), the relay is configured, and that computer hasn't used up its
// logins for the day (see Budget). The relay is only ever the fallback, after a direct connection
// fails.
// A code works once. Unknown or used codes get HTTP 404 before the upgrade; too many attempts
// from one address get 429. Clients keep the socket alive with WebSocket pings, which the
// runtime answers without waking the Durable Object (so a waiting host costs nothing).

import { DurableObject } from "cloudflare:workers";

/** No 0/O or 1/I, so a code read aloud can't be mistyped that way. */
const ALPHABET = "23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
const CODE_LENGTH = 6;
const CODE_LIFETIME_MS = 10 * 60_000;
/** Per address: codes asked for, and join attempts (each attempt is a guess at a code). */
const HOST_LIMIT = { count: 30, windowMs: 60 * 60_000 };
const JOIN_LIMIT = { count: 10, windowMs: 10 * 60_000 };
const MAX_MESSAGE = 2048;
/** How long both sides stay connected after the match, for a relay request. */
const MATCH_WINDOW_MS = 45_000;
const MAX_ADDRS = 8;
const MAX_NAME = 64;

interface Peer {
  name: string;
  addrs: string[];
  /** Home-network addresses, passed on only to a computer on the same network. */
  local: string[];
}

interface HostInfo extends Peer {
  ownRelay: boolean;
  ip: string;
  /** The sharing computer's identity code (64 hex), if its app sent one: relay logins are counted
   * per device, else per address. */
  device?: string;
}

/** A device's identity code: its certificate's SHA-256 fingerprint, in hex. */
const DEVICE = /^[0-9a-f]{64}$/;

function newCode(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(CODE_LENGTH));
  return Array.from(bytes, (b) => ALPHABET[b % ALPHABET.length]).join("");
}

/** "k7p-4qx" or "K7P 4QX" → "K7P4QX"; null if it can't be a code. */
export function normalizeCode(raw: string): string | null {
  const code = decodeURIComponent(raw).toUpperCase().replace(/[\s-]/g, "");
  return code.length === CODE_LENGTH && [...code].every((c) => ALPHABET.includes(c)) ? code : null;
}

const ADDR = /^(\d{1,3}(\.\d{1,3}){3}|\[[0-9a-fA-F:.]+\]):\d{1,5}$/;

function parseObject(message: string | ArrayBuffer): Record<string, unknown> | null {
  if (typeof message !== "string" || message.length > MAX_MESSAGE) return null;
  try {
    const m: unknown = JSON.parse(message);
    return typeof m === "object" && m !== null ? (m as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

function parseAddrs(addrs: unknown): string[] | null {
  if (!Array.isArray(addrs) || addrs.length === 0 || addrs.length > MAX_ADDRS) return null;
  return addrs.every((a) => typeof a === "string" && ADDR.test(a)) ? (addrs as string[]) : null;
}

/** Home-network addresses: 10/8, 172.16/12, 192.168/16, 169.254/16, fc00::/7, fe80::/10. */
export function isLocal(addr: string): boolean {
  const v4 = addr.match(/^(\d+)\.(\d+)\./);
  if (v4) {
    const [a, b] = [Number(v4[1]), Number(v4[2])];
    return a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 169 && b === 254);
  }
  return /^\[(f[cd]|fe[89ab])/i.test(addr);
}

/** What one side may learn of the other: its home-network addresses only when both connect
 * from the same public address (the same home or office, where they're how the two reach each
 * other); anyone else gets just the public ones. */
export function shareable(peer: Peer, sameNetwork: boolean): string[] {
  const public_ = peer.addrs.filter((a) => !isLocal(a));
  return sameNetwork ? [...public_, ...peer.local].slice(0, MAX_ADDRS) : public_;
}

/** A message from a client: a name, up to 8 `ip:port` addresses and, optionally, up to 8
 * home-network ones (`local`; ones that aren't are dropped). */
export function parsePeer(message: string | ArrayBuffer, type: string): Peer | null {
  const m = parseObject(message);
  if (!m || m.type !== type || typeof m.name !== "string") return null;
  const addrs = parseAddrs(m.addrs);
  const local = m.local === undefined ? [] : parseAddrs(m.local);
  if (!addrs || !local) return null;
  return { name: m.name.slice(0, MAX_NAME), addrs, local: local.filter(isLocal) };
}

function upgradePair(): [WebSocket, WebSocket] {
  const pair = new WebSocketPair();
  return [pair[0], pair[1]];
}

/** One code: the sharing computer waiting on it, until a viewer joins or it expires; then both
 * for a short while, in case the viewer needs the relay. */
export class CodeRoom extends DurableObject<Env> {
  async fetch(request: Request): Promise<Response> {
    const role = request.headers.get("X-Annex-Role");
    const code = request.headers.get("X-Annex-Code") ?? "";
    if (role === "host") {
      if (await this.ctx.storage.get("code")) return new Response("code in use", { status: 409 });
      const [client, server] = upgradePair();
      this.ctx.acceptWebSocket(server, ["host"]);
      await this.ctx.storage.put({ code, ip: request.headers.get("CF-Connecting-IP") ?? "local" });
      await this.ctx.storage.setAlarm(Date.now() + CODE_LIFETIME_MS);
      server.send(JSON.stringify({ type: "code", code, expires_in: CODE_LIFETIME_MS / 1000 }));
      return new Response(null, { status: 101, webSocket: client });
    }
    // A viewer: only once the host has said where it is, and only one.
    if (!(await this.ctx.storage.get<HostInfo>("host")) || (await this.ctx.storage.get("matched"))) {
      return new Response("unknown code", { status: 404 });
    }
    const [client, server] = upgradePair();
    this.ctx.acceptWebSocket(server, ["viewer", `ip:${request.headers.get("CF-Connecting-IP") ?? "local"}`]);
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const [tag] = this.ctx.getTags(ws);
    const matched = await this.ctx.storage.get<boolean>("matched");
    if (tag === "host") return matched ? this.fromHostAfterMatch(message) : this.hello(ws, message);
    if (matched) return this.fromViewerAfterMatch(ws, message);
    return this.join(ws, message);
  }

  private async hello(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const hello = parsePeer(message, "hello");
    if (!hello) return ws.close(4400, "bad message");
    const m = parseObject(message);
    const ownRelay = m?.own_relay === true;
    const device = typeof m?.device === "string" && DEVICE.test(m.device) ? m.device : undefined;
    const ip = (await this.ctx.storage.get<string>("ip")) ?? "local";
    await this.ctx.storage.put("host", { ...hello, ownRelay, ip, device } satisfies HostInfo);
  }

  private async join(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const join = parsePeer(message, "join");
    const host = await this.ctx.storage.get<HostInfo>("host");
    const [hostWs] = this.ctx.getWebSockets("host");
    if (!join) return ws.close(4400, "bad message");
    if (!host || !hostWs) return ws.close(4404, "unknown code");
    // Single use: mark it before telling anyone, so a second viewer can't join.
    await this.ctx.storage.put("matched", true);
    await this.ctx.storage.setAlarm(Date.now() + MATCH_WINDOW_MS);
    const viewerIp = this.ctx.getTags(ws).find((t) => t.startsWith("ip:"))?.slice(3);
    const same = viewerIp === host.ip;
    ws.send(JSON.stringify({ type: "peer", name: host.name, addrs: shareable(host, same) }));
    hostWs.send(JSON.stringify({ type: "peer", name: join.name, addrs: shareable(join, same) }));
  }

  private async fromViewerAfterMatch(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const [hostWs] = this.ctx.getWebSockets("host");
    if (parseObject(message)?.type !== "need_relay" || (await this.ctx.storage.get("asked"))) {
      return ws.close(4400, "bad message");
    }
    if (!hostWs) return this.end("no_relay");
    await this.ctx.storage.put("asked", true);
    const host = await this.ctx.storage.get<HostInfo>("host");
    const who = host?.device ? `device:${host.device}` : `ip:${host?.ip}`;
    const turn = host && !host.ownRelay ? await relayLogin(this.env, who) : null;
    hostWs.send(JSON.stringify({ type: "need_relay", turn }));
  }

  private async fromHostAfterMatch(message: string | ArrayBuffer): Promise<void> {
    const m = parseObject(message);
    const addrs = m?.type === "relay" ? parseAddrs(m.addrs) : null;
    const [viewerWs] = this.ctx.getWebSockets("viewer");
    if (addrs && viewerWs) viewerWs.send(JSON.stringify({ type: "relay", addrs }));
    await this.end(addrs ? null : "no_relay");
  }

  async webSocketClose(ws: WebSocket): Promise<void> {
    const [tag] = this.ctx.getTags(ws);
    if (tag === "host") await this.end("no_relay");
    // A viewer that goes away before matching leaves the code usable; after matching, it's done.
    else if (await this.ctx.storage.get("matched")) await this.end(null);
  }

  async alarm(): Promise<void> {
    const matched = await this.ctx.storage.get("matched");
    if (!matched) for (const ws of this.ctx.getWebSockets()) ws.send(JSON.stringify({ type: "expired" }));
    await this.end(matched ? "no_relay" : null, matched ? 1000 : 4410);
  }

  /** Tells whoever is still connected `notice` (if any), closes everything and forgets the code. */
  private async end(notice: string | null, code = 1000): Promise<void> {
    for (const ws of this.ctx.getWebSockets()) {
      try {
        if (notice) ws.send(JSON.stringify({ type: notice }));
        ws.close(code, notice ?? "done");
      } catch {
        // Already closing.
      }
    }
    await this.ctx.storage.deleteAlarm();
    await this.ctx.storage.deleteAll();
  }
}

/** Relay logins per day, per sharing computer (its identity code, or its address for apps that
 * don't send one). There's no total cap: the relay is only the fallback. Each login works for
 * RELAY_TTL_SECONDS, then the relay drops the session. */
export class Budget extends DurableObject<Env> {
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.blockConcurrencyWhile(async () => {
      this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS logins (day TEXT NOT NULL, ip TEXT NOT NULL)");
    });
  }

  /** Counts one login for `who` today (UTC); false once it has had `perDay`. (The column is
   * still called `ip`: it holds `device:…` or `ip:…`.) */
  take(who: string, perDay: number): boolean {
    const day = new Date().toISOString().slice(0, 10);
    this.ctx.storage.sql.exec("DELETE FROM logins WHERE day <> ?", day);
    const used = this.ctx.storage.sql
      .exec<{ n: number }>("SELECT COUNT(*) AS n FROM logins WHERE day = ? AND ip = ?", day, who)
      .one().n;
    if (used >= perDay) return false;
    this.ctx.storage.sql.exec("INSERT INTO logins (day, ip) VALUES (?, ?)", day, who);
    return true;
  }
}

export interface TurnLogin {
  urls: string[];
  username: string;
  credential: string;
  ttl: number;
}

function numberVar(value: string | undefined, fallback: number): number {
  const n = Number(value);
  return Number.isFinite(n) && n >= 0 ? n : fallback;
}

/** A login for our relay, or null: relay not configured (no TURN key), or `who` (a device or an
 * address) has used up today's logins. */
/** Set with `wrangler secret put` (so not in wrangler.jsonc's types). Both unset: our relay is off. */
type Secrets = { TURN_KEY_ID?: string; TURN_KEY_API_TOKEN?: string };

export async function relayLogin(env: Env, who: string, fetcher: typeof fetch = fetch): Promise<TurnLogin | null> {
  const { TURN_KEY_ID, TURN_KEY_API_TOKEN } = env as Env & Secrets;
  if (!TURN_KEY_ID || !TURN_KEY_API_TOKEN) return null;
  const ttl = numberVar(env.RELAY_TTL_SECONDS, 86400);
  const perDevice = numberVar(env.RELAY_PER_DEVICE_PER_DAY, 10);
  if (ttl === 0 || !(await env.BUDGET.getByName("global").take(who, perDevice))) return null;
  const response = await fetcher(
    `https://rtc.live.cloudflare.com/v1/turn/keys/${TURN_KEY_ID}/credentials/generate-ice-servers`,
    {
      method: "POST",
      headers: { Authorization: `Bearer ${TURN_KEY_API_TOKEN}`, "Content-Type": "application/json" },
      body: JSON.stringify({ ttl }),
    },
  );
  if (!response.ok) return null;
  const body = (await response.json()) as { iceServers?: { urls: string[]; username?: string; credential?: string }[] };
  const turn = body.iceServers?.find((s) => s.username && s.credential);
  if (!turn?.username || !turn.credential) return null;
  // UDP only: that's what the core's relay client speaks.
  const urls = turn.urls.filter((u) => u.startsWith("turn:") && u.includes("transport=udp"));
  return urls.length ? { urls, username: turn.username, credential: turn.credential, ttl } : null;
}

/** Sliding-window counters for one client address. */
export class Limiter extends DurableObject<Env> {
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.blockConcurrencyWhile(async () => {
      this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS hits (bucket TEXT NOT NULL, at INTEGER NOT NULL)");
    });
  }

  /** Counts one attempt; false if `count` were already made within `windowMs`. */
  allow(bucket: string, count: number, windowMs: number): boolean {
    const now = Date.now();
    this.ctx.storage.sql.exec("DELETE FROM hits WHERE at < ?", now - windowMs);
    const used = this.ctx.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM hits WHERE bucket = ?", bucket).one().n;
    if (used >= count) return false;
    this.ctx.storage.sql.exec("INSERT INTO hits (bucket, at) VALUES (?, ?)", bucket, now);
    return true;
  }
}

async function allowed(env: Env, request: Request, bucket: string, limit: { count: number; windowMs: number }) {
  const ip = request.headers.get("CF-Connecting-IP") ?? "local";
  return env.LIMITS.getByName(ip).allow(bucket, limit.count, limit.windowMs);
}

function forward(stub: DurableObjectStub<CodeRoom>, request: Request, role: string, code: string) {
  const headers = new Headers(request.headers);
  headers.set("X-Annex-Role", role);
  headers.set("X-Annex-Code", code);
  return stub.fetch(new Request(request.url, { method: "GET", headers }));
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(request.url);
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("Annex rendezvous. See https://github.com/NRanjan-17/Annex\n", { status: pathname === "/" ? 200 : 404 });
    }
    if (pathname === "/v1/host") {
      if (!(await allowed(env, request, "host", HOST_LIMIT))) return new Response("too many codes", { status: 429 });
      // A fresh code; on the rare clash with a live one, draw again.
      for (let i = 0; i < 5; i++) {
        const code = newCode();
        const response = await forward(env.CODES.getByName(code), request, "host", code);
        if (response.status !== 409) return response;
      }
      return new Response("no free code", { status: 503 });
    }
    const join = pathname.match(/^\/v1\/join\/([^/]+)$/);
    if (join) {
      if (!(await allowed(env, request, "join", JOIN_LIMIT))) return new Response("too many attempts", { status: 429 });
      const code = normalizeCode(join[1]);
      if (!code) return new Response("unknown code", { status: 404 });
      return forward(env.CODES.getByName(code), request, "viewer", code);
    }
    return new Response("not found", { status: 404 });
  },
} satisfies ExportedHandler<Env>;
