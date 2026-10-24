// Annex rendezvous: introduces two computers so they can connect directly across the internet.
// It never carries a session: it matches a short code to the sharing computer, then swaps the
// two computers' addresses. See docs/INTERNET-CONNECT.md.
//
// Protocol v1 (JSON text frames over WebSocket):
//   Sharing computer  GET /v1/host        ← {"type":"code","code":"K7P4QX","expires_in":600}
//                                         → {"type":"hello","name":"…","addrs":["203.0.113.9:47800",…]}
//                                         ← {"type":"peer","name":"…","addrs":[…]}   (a viewer joined)
//                                         ← {"type":"expired"}                       (10 minutes passed)
//   Viewer            GET /v1/join/K7P4QX → {"type":"join","name":"…","addrs":[…]}
//                                         ← {"type":"peer","name":"…","addrs":[…]}
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
const MAX_ADDRS = 8;
const MAX_NAME = 64;

interface Peer {
  name: string;
  addrs: string[];
}

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

/** A message from a client: a name and up to 8 `ip:port` addresses. */
export function parsePeer(message: string | ArrayBuffer, type: string): Peer | null {
  if (typeof message !== "string" || message.length > MAX_MESSAGE) return null;
  let m: unknown;
  try {
    m = JSON.parse(message);
  } catch {
    return null;
  }
  if (typeof m !== "object" || m === null) return null;
  const { type: t, name, addrs } = m as Record<string, unknown>;
  if (t !== type || typeof name !== "string" || !Array.isArray(addrs)) return null;
  if (addrs.length === 0 || addrs.length > MAX_ADDRS) return null;
  if (!addrs.every((a) => typeof a === "string" && ADDR.test(a))) return null;
  return { name: name.slice(0, MAX_NAME), addrs: addrs as string[] };
}

function upgradePair(): [WebSocket, WebSocket] {
  const pair = new WebSocketPair();
  return [pair[0], pair[1]];
}

/** One code: the sharing computer waiting on it, until a viewer joins or it expires. */
export class CodeRoom extends DurableObject<Env> {
  async fetch(request: Request): Promise<Response> {
    const role = request.headers.get("X-Annex-Role");
    const code = request.headers.get("X-Annex-Code") ?? "";
    if (role === "host") {
      if (await this.ctx.storage.get("code")) return new Response("code in use", { status: 409 });
      const [client, server] = upgradePair();
      this.ctx.acceptWebSocket(server, ["host"]);
      await this.ctx.storage.put("code", code);
      await this.ctx.storage.setAlarm(Date.now() + CODE_LIFETIME_MS);
      server.send(JSON.stringify({ type: "code", code, expires_in: CODE_LIFETIME_MS / 1000 }));
      return new Response(null, { status: 101, webSocket: client });
    }
    // A viewer: only once the host has said where it is.
    if (!(await this.ctx.storage.get<Peer>("host"))) return new Response("unknown code", { status: 404 });
    const [client, server] = upgradePair();
    this.ctx.acceptWebSocket(server, ["viewer"]);
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const [tag] = this.ctx.getTags(ws);
    if (tag === "host") {
      const hello = parsePeer(message, "hello");
      if (!hello) return ws.close(4400, "bad message");
      await this.ctx.storage.put("host", hello);
      return;
    }
    const join = parsePeer(message, "join");
    const host = await this.ctx.storage.get<Peer>("host");
    const [hostWs] = this.ctx.getWebSockets("host");
    if (!join) return ws.close(4400, "bad message");
    if (!host || !hostWs) return ws.close(4404, "unknown code");
    // Single use: forget the code before telling anyone, so a second viewer can't join.
    await this.forget();
    ws.send(JSON.stringify({ type: "peer", ...host }));
    hostWs.send(JSON.stringify({ type: "peer", ...join }));
    ws.close(1000, "matched");
    hostWs.close(1000, "matched");
  }

  async webSocketClose(ws: WebSocket): Promise<void> {
    if (this.ctx.getTags(ws)[0] === "host") await this.forget();
  }

  async alarm(): Promise<void> {
    for (const ws of this.ctx.getWebSockets()) {
      ws.send(JSON.stringify({ type: "expired" }));
      ws.close(4410, "expired");
    }
    await this.forget();
  }

  private async forget(): Promise<void> {
    await this.ctx.storage.deleteAlarm();
    await this.ctx.storage.deleteAll();
  }
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
