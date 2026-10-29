import { env, exports } from "cloudflare:workers";
import { runDurableObjectAlarm } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { isLocal, normalizeCode, parsePeer, relayLogin } from "../src/index";

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

  it("passes home-network addresses only between computers on the same network", async () => {
    const lan = { type: "join", name: "Office PC", addrs: ["198.51.100.20:51234"], local: ["192.168.1.5:51234"] };
    // A local address in `addrs` (as a client might send by mistake) is kept from strangers too.
    const hostLan = { type: "hello", name: "Dev's Mac", addrs: ["203.0.113.10:47800", "10.0.0.3:47800"], local: ["10.0.0.2:47800"] };
    // Different networks: public addresses only, both ways.
    let host = await open("/v1/host", "203.0.113.10");
    let code = ((await host.next!()) as { code: string }).code;
    host.send!(hostLan);
    await new Promise((r) => setTimeout(r, 50));
    let viewer = await open(`/v1/join/${code}`, "198.51.100.20");
    viewer.send!(lan);
    expect(await viewer.next!()).toEqual({ type: "peer", name: "Dev's Mac", addrs: ["203.0.113.10:47800"] });
    expect(await host.next!()).toEqual({ type: "peer", name: "Office PC", addrs: ["198.51.100.20:51234"] });
    // Same public address: everything, so they can reach each other at home.
    host = await open("/v1/host", "203.0.113.11");
    code = ((await host.next!()) as { code: string }).code;
    host.send!(hostLan);
    await new Promise((r) => setTimeout(r, 50));
    viewer = await open(`/v1/join/${code}`, "203.0.113.11");
    viewer.send!(lan);
    expect(await viewer.next!()).toEqual({ type: "peer", name: "Dev's Mac", addrs: ["203.0.113.10:47800", "10.0.0.2:47800"] });
    expect(await host.next!()).toEqual({ type: "peer", name: "Office PC", addrs: ["198.51.100.20:51234", "192.168.1.5:51234"] });
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

describe("relay fallback", () => {
  async function matched(hostIp: string, viewerIp: string) {
    const { host, code } = await hostWithCode(hostIp);
    const viewer = await open(`/v1/join/${code}`, viewerIp);
    viewer.send!({ type: "join", ...VIEWER });
    expect(await viewer.next!()).toEqual({ type: "peer", ...HOST });
    expect(await host.next!()).toEqual({ type: "peer", ...VIEWER });
    return { host, viewer, code };
  }

  it("passes a relay request to the host and its relayed address back", async () => {
    const { host, viewer } = await matched("192.0.2.80", "192.0.2.81");
    viewer.send!({ type: "need_relay" });
    // No TURN key in tests, so no login from us; a host with its own relay still answers.
    expect(await host.next!()).toEqual({ type: "need_relay", turn: null });
    host.send!({ type: "relay", addrs: ["198.51.100.5:61000"] });
    expect(await viewer.next!()).toEqual({ type: "relay", addrs: ["198.51.100.5:61000"] });
  });

  it("tells the viewer when the host has no relay", async () => {
    const { host, viewer } = await matched("192.0.2.82", "192.0.2.83");
    viewer.send!({ type: "need_relay" });
    await host.next!();
    host.send!({ type: "no_relay" });
    expect(await viewer.next!()).toEqual({ type: "no_relay" });
  });

  it("closes the match window after its alarm", async () => {
    const { viewer, code } = await matched("192.0.2.84", "192.0.2.85");
    expect(await runDurableObjectAlarm(env.CODES.getByName(code))).toBe(true);
    expect(await viewer.next!()).toEqual({ type: "no_relay" });
  });
});

describe("relay budget", () => {
  const cloudflare = (async () =>
    Response.json(
      {
        iceServers: [
          { urls: ["stun:stun.cloudflare.com:3478"] },
          {
            urls: ["turn:turn.cloudflare.com:3478?transport=udp", "turns:turn.cloudflare.com:5349?transport=tcp"],
            username: "u",
            credential: "c",
          },
        ],
      },
      { status: 201 },
    )) as unknown as typeof fetch;

  function configured(name: string, vars: Record<string, string> = {}) {
    // A fresh Budget per test, so the counts don't carry over.
    const budget = { getByName: () => env.BUDGET.getByName(name) };
    return { ...env, BUDGET: budget, TURN_KEY_ID: "key", TURN_KEY_API_TOKEN: "token", ...vars } as unknown as Env;
  }

  it("is off without a TURN key", async () => {
    expect(await relayLogin(env, "ip:192.0.2.1", cloudflare)).toEqual({ turn: null });
  });

  it("hands out UDP logins with the configured lifetime", async () => {
    let asked: RequestInit | undefined;
    const spy = (async (_url: string, init: RequestInit) => {
      asked = init;
      return cloudflare(_url, init);
    }) as unknown as typeof fetch;
    const grant = await relayLogin(configured("budget-a"), "ip:192.0.2.1", spy);
    expect(grant).toEqual({
      turn: { urls: ["turn:turn.cloudflare.com:3478?transport=udp"], username: "u", credential: "c", ttl: 86400 },
      relay_left: 9,
      relay_per_day: 10,
    });
    expect(JSON.parse(asked!.body as string)).toEqual({ ttl: 86400 });
  });

  it("stops at the per-device cap, with no total cap", async () => {
    const e = configured("budget-b", { RELAY_PER_DEVICE_PER_DAY: "2" });
    const got = [];
    const left = [];
    for (const who of ["device:a", "device:a", "device:a", "device:b", "ip:192.0.2.3", "device:c"]) {
      const grant = await relayLogin(e, who, cloudflare);
      got.push(grant.turn !== null);
      left.push(grant.relay_left);
    }
    expect(got).toEqual([true, true, false, true, true, true]);
    // How many each computer has left after each login; 0 once used up, so the app can say so.
    expect(left).toEqual([1, 0, 0, 1, 1, 1]);
  });

  it("gives nothing if Cloudflare refuses", async () => {
    const refuse = (async () => new Response("no", { status: 403 })) as unknown as typeof fetch;
    expect((await relayLogin(configured("budget-c"), "ip:192.0.2.1", refuse)).turn).toBeNull();
  });
});

describe("validation", () => {
  it("normalizes codes", () => {
    expect(normalizeCode("k7p-4qx")).toBe("K7P4QX");
    expect(normalizeCode("K7P 4QX")).toBe("K7P4QX");
    expect(normalizeCode("K7P4Q0")).toBeNull(); // 0 isn't in the alphabet
    expect(normalizeCode("K7P4")).toBeNull();
  });

  it("knows home-network addresses", () => {
    for (const a of ["10.1.2.3:1", "172.16.0.1:1", "172.31.9.9:1", "192.168.0.4:1", "169.254.1.1:1", "[fd12::1]:1", "[fe80::1]:1"]) {
      expect(isLocal(a), a).toBe(true);
    }
    for (const a of ["203.0.113.10:1", "172.32.0.1:1", "11.0.0.1:1", "[2001:db8::5]:1"]) expect(isLocal(a), a).toBe(false);
  });

  it("accepts only names and ip:port addresses", () => {
    const ok = JSON.stringify({ type: "join", name: "n".repeat(100), addrs: ["1.2.3.4:5", "[::1]:6"] });
    expect(parsePeer(ok, "join")).toEqual({ name: "n".repeat(64), addrs: ["1.2.3.4:5", "[::1]:6"], local: [] });
    const withLocal = JSON.stringify({ type: "join", name: "n", addrs: ["1.2.3.4:5"], local: ["192.168.0.2:5", "8.8.8.8:5"] });
    expect(parsePeer(withLocal, "join")?.local).toEqual(["192.168.0.2:5"]);
    expect(parsePeer(ok, "hello")).toBeNull();
    expect(parsePeer(JSON.stringify({ type: "join", name: "n", addrs: [] }), "join")).toBeNull();
    expect(parsePeer(JSON.stringify({ type: "join", name: "n", addrs: Array(9).fill("1.2.3.4:5") }), "join")).toBeNull();
    expect(parsePeer(JSON.stringify({ type: "join", name: "n", addrs: ["evil.example:80"] }), "join")).toBeNull();
    expect(parsePeer("not json", "join")).toBeNull();
    expect(parsePeer("x".repeat(3000), "join")).toBeNull();
  });
});
