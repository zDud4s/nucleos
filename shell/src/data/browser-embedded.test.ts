import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

type Mod = typeof import("./browser-embedded");

// The token promise is module state, so each test gets a fresh module.
async function fresh(): Promise<Mod> {
  vi.resetModules();
  return await import("./browser-embedded");
}

function res(status: number, body?: unknown): Response {
  const text = body === undefined ? "" : JSON.stringify(body);
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText: "",
    text: async () => text,
    json: async () => JSON.parse(text),
  } as unknown as Response;
}

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue("token-abc");
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

function lastCall(): { url: string; method: string | undefined; body: unknown; auth: string | null } {
  const [url, init] = fetchMock.mock.calls[fetchMock.mock.calls.length - 1] as [string, RequestInit];
  return {
    url,
    method: init.method,
    body: init.body,
    auth: new Headers(init.headers).get("Authorization"),
  };
}

describe("browser-embedded", () => {
  it("embeddedPrepare posts the project", async () => {
    const m = await fresh();
    fetchMock.mockResolvedValue(res(200, { proxy_addr: "127.0.0.1:9" }));
    await expect(m.embeddedPrepare("p1")).resolves.toEqual({ proxy_addr: "127.0.0.1:9" });
    const c = lastCall();
    expect(c.url).toMatch(/\/browser\/embedded\/prepare$/);
    expect(c.method).toBe("POST");
    expect(JSON.parse(c.body as string)).toEqual({ project: "p1" });
    expect(c.auth).toBe("Bearer token-abc");
  });

  it("embeddedRegister posts project and cdp_http, 204 gives undefined", async () => {
    const m = await fresh();
    fetchMock.mockResolvedValue(res(204));
    await expect(m.embeddedRegister("p1", "http://127.0.0.1:1234")).resolves.toBeUndefined();
    const c = lastCall();
    expect(c.url).toMatch(/\/browser\/embedded\/register$/);
    expect(c.method).toBe("POST");
    expect(JSON.parse(c.body as string)).toEqual({ project: "p1", cdp_http: "http://127.0.0.1:1234" });
    expect(c.auth).toBe("Bearer token-abc");
  });

  it("embeddedRequests is an authenticated GET", async () => {
    const m = await fresh();
    const list = [{ request_id: "r1", kind: "open", project: "p1", url: "https://x" }];
    fetchMock.mockResolvedValue(res(200, list));
    await expect(m.embeddedRequests()).resolves.toEqual(list);
    const c = lastCall();
    expect(c.url).toMatch(/\/browser\/embedded\/requests$/);
    expect(c.method).toBeUndefined();
    expect(c.auth).toBe("Bearer token-abc");
  });

  it("embeddedAck encodes the id and posts the nonce", async () => {
    const m = await fresh();
    fetchMock.mockResolvedValue(res(200, { session_id: "s1" }));
    await expect(m.embeddedAck("a/b c", "n1")).resolves.toEqual({ session_id: "s1" });
    const c = lastCall();
    expect(c.url).toMatch(/\/browser\/embedded\/requests\/a%2Fb%20c\/ack$/);
    expect(c.method).toBe("POST");
    expect(JSON.parse(c.body as string)).toEqual({ nonce: "n1" });
    expect(c.auth).toBe("Bearer token-abc");
  });

  it("embeddedClosed posts an empty object, 204 gives undefined", async () => {
    const m = await fresh();
    fetchMock.mockResolvedValue(res(204));
    await expect(m.embeddedClosed("s/1")).resolves.toBeUndefined();
    const c = lastCall();
    expect(c.url).toMatch(/\/browser\/embedded\/sessions\/s%2F1\/closed$/);
    expect(c.method).toBe("POST");
    expect(c.body).toBe("{}");
    expect(c.auth).toBe("Bearer token-abc");
  });

  it("409 pillar_off is recognised by isPillarOff", async () => {
    const m = await fresh();
    fetchMock.mockResolvedValue(res(409, { refusal: "pillar_off" }));
    const e = await m.embeddedPrepare("p1").catch((x: unknown) => x);
    expect(m.isPillarOff(e)).toBe(true);
    expect(m.isRequestGone(e)).toBe(false);
  });

  it("410 is recognised by isRequestGone", async () => {
    const m = await fresh();
    fetchMock.mockResolvedValue(res(410, { error: "expired" }));
    const e = await m.embeddedAck("r1", "n").catch((x: unknown) => x);
    expect(m.isRequestGone(e)).toBe(true);
    expect(m.isPillarOff(e)).toBe(false);
  });

  it("helpers are false for non-refusals", async () => {
    const m = await fresh();
    expect(m.isPillarOff(new Error("x"))).toBe(false);
    expect(m.isRequestGone(undefined)).toBe(false);
  });
});
