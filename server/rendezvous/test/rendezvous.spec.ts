import { env, exports } from "cloudflare:workers";
import { runDurableObjectAlarm } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { normalizeCode, parsePeer } from "../src/index";

/** A test client: opens a WebSocket on a path and queues what it receives. */
async function open(path: string, ip = "198.51.100.7") {
  const response = await exports.default.fetch(`https://rendezvous.test${path}`, {
    headers: { Upgrade: "websocket", "CF-Connecting-IP": ip },
  });
  const ws = response.webSocket;
  if (!ws) return { status: response.status };
  ws.accept();
  const inbox: unknown[] = [];
  const waiters: ((m: unknown) => void)[] = [];
  let closed: number | undefined;
  ws.addEventListener("message", (e) => {
    const m = JSON.parse(e.data as string);
    const w = waiters.shift();
    w ? w(m) : inbox.push(m);
  });
  ws.addEventListener("close", (e) => {
    closed = e.code;
  });
  return {
    status: response.status,
    send: (m: unknown) => ws.send(JSON.stringify(m)),
    next: () => (inbox.length ? Promise.resolve(inbox.shift()) : new Promise<unknown>((r) => waiters.push(r))),
    closed: () => closed,
  };
}

const HOST = { name: "Dev's Mac", addrs: ["203.0.113.10:47800", "[2001:db8::5]:47800"] };
const VIEWER = { name: "Office PC", addrs: ["198.51.100.20:51234"] };

async function hostWithCode(ip?: string) {
  const host = await open("/v1/host", ip);
  expect(host.status).toBe(101);
  const first = (await host.next!()) as { type: string; code: string; expires_in: number };
  expect(first.type).toBe("code");
  expect(first.expires_in).toBe(600);
  host.send!({ type: "hello", ...HOST });
  // Let the hello land before a viewer looks for it.
  await new Promise((r) => setTimeout(r, 50));
  return { host, code: first.code };
}

describe("rendezvous", () => {
  it("matches a viewer to the code once, swapping addresses", async () => {
    const { host, code } = await hostWithCode();
    expect(code).toMatch(/^[2-9A-HJ-NP-Z]{6}$/);
    // Typed loosely, as people will.
    const viewer = await open(`/v1/join/${code.slice(0, 3).toLowerCase()}-${code.slice(3)}`);
    expect(viewer.status).toBe(101);
    viewer.send!({ type: "join", ...VIEWER });
    expect(await viewer.next!()).toEqual({ type: "peer", ...HOST });
    expect(await host.next!()).toEqual({ type: "peer", ...VIEWER });
    // Used: a second viewer finds nothing.
    expect((await open(`/v1/join/${code}`, "198.51.100.8")).status).toBe(404);
  });

  it("refuses unknown codes and codes whose host hasn't said where it is", async () => {
    expect((await open("/v1/join/ABCDEF", "192.0.2.1")).status).toBe(404);
    expect((await open("/v1/join/not-a-code", "192.0.2.1")).status).toBe(404);
    const host = await open("/v1/host", "192.0.2.2");
    const { code } = (await host.next!()) as { code: string };
    expect((await open(`/v1/join/${code}`, "192.0.2.1")).status).toBe(404);
  });

  it("limits guesses per address", async () => {
    const statuses = [];
    for (let i = 0; i < 11; i++) statuses.push((await open("/v1/join/ABCDEF", "192.0.2.50")).status);
    expect(statuses.slice(0, 10).every((s) => s === 404)).toBe(true);
    expect(statuses[10]).toBe(429);
    // Another address is unaffected.
    expect((await open("/v1/join/ABCDEF", "192.0.2.51")).status).toBe(404);
  });

  it("expires a code after its alarm", async () => {
    const { host, code } = await hostWithCode("192.0.2.60");
    expect(await runDurableObjectAlarm(env.CODES.getByName(code))).toBe(true);
    expect(await host.next!()).toEqual({ type: "expired" });
    expect((await open(`/v1/join/${code}`, "192.0.2.61")).status).toBe(404);
  });

  it("drops bad messages", async () => {
    const { host, code } = await hostWithCode("192.0.2.70");
    const viewer = await open(`/v1/join/${code}`, "192.0.2.71");
    viewer.send!({ type: "join", name: "x", addrs: ["not an address"] });
    await new Promise((r) => setTimeout(r, 50));
    expect(viewer.closed!()).toBe(4400);
    // The code is still good for a proper viewer.
    const again = await open(`/v1/join/${code}`, "192.0.2.72");
    again.send!({ type: "join", ...VIEWER });
    expect(await again.next!()).toEqual({ type: "peer", ...HOST });
    expect(await host.next!()).toEqual({ type: "peer", ...VIEWER });
  });
});

describe("validation", () => {
  it("normalizes codes", () => {
    expect(normalizeCode("k7p-4qx")).toBe("K7P4QX");
    expect(normalizeCode("K7P 4QX")).toBe("K7P4QX");
    expect(normalizeCode("K7P4Q0")).toBeNull(); // 0 isn't in the alphabet
    expect(normalizeCode("K7P4")).toBeNull();
  });

  it("accepts only names and ip:port addresses", () => {
    const ok = JSON.stringify({ type: "join", name: "n".repeat(100), addrs: ["1.2.3.4:5", "[::1]:6"] });
    expect(parsePeer(ok, "join")).toEqual({ name: "n".repeat(64), addrs: ["1.2.3.4:5", "[::1]:6"] });
    expect(parsePeer(ok, "hello")).toBeNull();
    expect(parsePeer(JSON.stringify({ type: "join", name: "n", addrs: [] }), "join")).toBeNull();
    expect(parsePeer(JSON.stringify({ type: "join", name: "n", addrs: Array(9).fill("1.2.3.4:5") }), "join")).toBeNull();
    expect(parsePeer(JSON.stringify({ type: "join", name: "n", addrs: ["evil.example:80"] }), "join")).toBeNull();
    expect(parsePeer("not json", "join")).toBeNull();
    expect(parsePeer("x".repeat(3000), "join")).toBeNull();
  });
});
