import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const emb = vi.hoisted(() => ({
  embeddedPrepare: vi.fn(),
  embeddedRegister: vi.fn(),
  embeddedRequests: vi.fn(),
  embeddedAck: vi.fn(),
  embeddedClosed: vi.fn(),
}));
vi.mock("./browser-embedded", () => emb);

import {
  agentTabUrl,
  ensureAgentEnv,
  mintNonce,
  reportAgentTabClosed,
  startAgentPoller,
} from "./browser-agent";

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockReset();
  Object.values(emb).forEach((f) => f.mockReset());
});
afterEach(() => {
  vi.useRealTimers();
});

describe("startAgentPoller", () => {
  const mk = () => ({ onOpen: vi.fn(), onClose: vi.fn() });

  it("polls at a 1 s cadence", async () => {
    emb.embeddedRequests.mockResolvedValue([]);
    const stop = startAgentPoller(mk());
    await vi.advanceTimersByTimeAsync(999);
    expect(emb.embeddedRequests).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(emb.embeddedRequests).toHaveBeenCalledTimes(1);
    stop();
  });

  it("does not overlap ticks", async () => {
    let resolve!: (v: never[]) => void;
    emb.embeddedRequests.mockReturnValue(new Promise((r) => (resolve = r)));
    const stop = startAgentPoller(mk());
    await vi.advanceTimersByTimeAsync(5000);
    expect(emb.embeddedRequests).toHaveBeenCalledTimes(1);
    resolve([]);
    emb.embeddedRequests.mockResolvedValue([]);
    await vi.advanceTimersByTimeAsync(1000);
    expect(emb.embeddedRequests).toHaveBeenCalledTimes(2);
    stop();
  });

  it("handles a request_id once and routes by kind", async () => {
    const h = mk();
    emb.embeddedRequests.mockResolvedValue([
      { request_id: "a", kind: "open", project: "p", url: "http://x" },
      { request_id: "b", kind: "close", project: "p", session_id: "s1" },
    ]);
    const stop = startAgentPoller(h);
    await vi.advanceTimersByTimeAsync(3000);
    expect(h.onOpen).toHaveBeenCalledTimes(1);
    expect(h.onClose).toHaveBeenCalledTimes(1);
    expect(h.onClose.mock.calls[0][0].session_id).toBe("s1");
    stop();
  });

  it("routes a focus request once to onFocus", async () => {
    const onFocus = vi.fn();
    emb.embeddedRequests.mockResolvedValue([
      { request_id: "f", kind: "focus", project: "p", session_id: "s1" },
    ]);
    const stop = startAgentPoller({ ...mk(), onFocus });
    await vi.advanceTimersByTimeAsync(3000);
    expect(onFocus).toHaveBeenCalledTimes(1);
    expect(onFocus.mock.calls[0][0].session_id).toBe("s1");
    stop();
  });

  it("a focus without a handler is harmless", async () => {
    const h = mk();
    emb.embeddedRequests.mockResolvedValue([
      { request_id: "f", kind: "focus", project: "p", session_id: "s1" },
      { request_id: "a", kind: "open", project: "p", url: "u" },
    ]);
    const stop = startAgentPoller(h);
    await vi.advanceTimersByTimeAsync(1000);
    expect(h.onOpen).toHaveBeenCalledTimes(1);
    stop();
  });

  it("ignores unknown kinds and incomplete requests", async () => {
    const h = mk();
    emb.embeddedRequests.mockResolvedValue([
      { request_id: "a", kind: "weird", project: "p", url: "u" },
      { request_id: "b", kind: "open", project: "p" },
      { request_id: "c", kind: "close", project: "p" },
    ]);
    const stop = startAgentPoller(h);
    await vi.advanceTimersByTimeAsync(1000);
    expect(h.onOpen).not.toHaveBeenCalled();
    expect(h.onClose).not.toHaveBeenCalled();
    stop();
  });

  it("does not await handlers: a slow open does not block the batch or the next poll", async () => {
    const h = mk();
    h.onOpen.mockReturnValue(new Promise(() => {}));
    emb.embeddedRequests.mockResolvedValue([
      { request_id: "a", kind: "open", project: "p", url: "u" },
      { request_id: "b", kind: "close", project: "p", session_id: "s1" },
    ]);
    const stop = startAgentPoller(h);
    await vi.advanceTimersByTimeAsync(3000);
    expect(h.onClose).toHaveBeenCalledTimes(1);
    expect(emb.embeddedRequests.mock.calls.length).toBeGreaterThanOrEqual(3);
    stop();
  });

  it("calls onLost once per outage, only after a poll worked", async () => {
    const onLost = vi.fn();
    emb.embeddedRequests
      .mockRejectedValueOnce(new Error("early"))
      .mockResolvedValueOnce([])
      .mockRejectedValueOnce(new Error("404"))
      .mockRejectedValueOnce(new Error("404"))
      .mockResolvedValueOnce([])
      .mockRejectedValue(new Error("410"));
    const stop = startAgentPoller({ ...mk(), onLost });
    await vi.advanceTimersByTimeAsync(1000);
    expect(onLost).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(2000);
    expect(onLost).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(3000);
    expect(onLost).toHaveBeenCalledTimes(2);
    stop();
  });

  it("survives a rejecting onLost", async () => {
    emb.embeddedRequests.mockResolvedValueOnce([]).mockRejectedValue(new Error("x"));
    const stop = startAgentPoller({ ...mk(), onLost: () => Promise.reject(new Error("boom")) });
    await vi.advanceTimersByTimeAsync(3000);
    stop();
  });

  it("survives a throwing handler", async () => {
    const h = mk();
    h.onOpen.mockRejectedValueOnce(new Error("boom"));
    emb.embeddedRequests
      .mockResolvedValueOnce([
        { request_id: "a", kind: "open", project: "p", url: "u" },
        { request_id: "b", kind: "open", project: "p", url: "v" },
      ])
      .mockResolvedValueOnce([
        { request_id: "c", kind: "open", project: "p", url: "w" },
      ])
      .mockResolvedValue([]);
    const stop = startAgentPoller(h);
    await vi.advanceTimersByTimeAsync(2000);
    expect(h.onOpen.mock.calls.map((c) => c[0].request_id)).toEqual([
      "a",
      "b",
      "c",
    ]);
    stop();
  });

  it("survives embeddedRequests rejecting", async () => {
    const h = mk();
    emb.embeddedRequests
      .mockRejectedValueOnce(new Error("down"))
      .mockResolvedValue([
        { request_id: "a", kind: "open", project: "p", url: "u" },
      ]);
    const stop = startAgentPoller(h);
    await vi.advanceTimersByTimeAsync(2000);
    expect(h.onOpen).toHaveBeenCalledTimes(1);
    stop();
  });

  it("stop halts polling and is idempotent", async () => {
    emb.embeddedRequests.mockResolvedValue([]);
    const stop = startAgentPoller(mk());
    await vi.advanceTimersByTimeAsync(1000);
    stop();
    stop();
    await vi.advanceTimersByTimeAsync(5000);
    expect(emb.embeddedRequests).toHaveBeenCalledTimes(1);
  });
});

describe("ensureAgentEnv", () => {
  it("runs prepare, ensure, register in order", async () => {
    const order: string[] = [];
    emb.embeddedPrepare.mockImplementation(async () => {
      order.push("prepare");
      return { proxy_addr: "127.0.0.1:1" };
    });
    invoke.mockImplementation(async () => {
      order.push("ensure");
      return { cdp_http: "http://127.0.0.1:9" };
    });
    emb.embeddedRegister.mockImplementation(async () => {
      order.push("register");
    });
    const r = await ensureAgentEnv("p");
    expect(r).toEqual({ ok: true, cdpHttp: "http://127.0.0.1:9" });
    expect(order).toEqual(["prepare", "ensure", "register"]);
    expect(invoke).toHaveBeenCalledWith("browser_env_ensure", {
      project: "p",
      role: "agent",
      proxyAddr: "127.0.0.1:1",
    });
    expect(emb.embeddedRegister).toHaveBeenCalledWith("p", "http://127.0.0.1:9");
  });

  it("returns ok:false at each failing step", async () => {
    emb.embeddedPrepare.mockRejectedValueOnce(new Error("e1"));
    expect(await ensureAgentEnv("p1")).toEqual({ ok: false, reason: "e1" });

    emb.embeddedPrepare.mockResolvedValue({ proxy_addr: "x" });
    invoke.mockRejectedValueOnce("e2");
    expect(await ensureAgentEnv("p2")).toEqual({ ok: false, reason: "e2" });

    invoke.mockResolvedValue({ cdp_http: "c" });
    emb.embeddedRegister.mockRejectedValueOnce(new Error("e3"));
    expect(await ensureAgentEnv("p3")).toEqual({ ok: false, reason: "e3" });
  });

  it("dedupes in-flight calls per project", async () => {
    let release!: () => void;
    emb.embeddedPrepare.mockReturnValue(
      new Promise((r) => (release = () => r({ proxy_addr: "x" }))),
    );
    invoke.mockResolvedValue({ cdp_http: "c" });
    emb.embeddedRegister.mockResolvedValue(undefined);
    const a = ensureAgentEnv("p");
    const b = ensureAgentEnv("p");
    expect(a).toBe(b);
    release();
    await a;
    expect(emb.embeddedPrepare).toHaveBeenCalledTimes(1);
    emb.embeddedPrepare.mockResolvedValue({ proxy_addr: "x" });
    await ensureAgentEnv("p");
    expect(emb.embeddedPrepare).toHaveBeenCalledTimes(2);
  });
});

describe("nonce and tab url", () => {
  it("mints unique fragment-safe nonces", () => {
    const set = new Set(Array.from({ length: 50 }, mintNonce));
    expect(set.size).toBe(50);
    for (const n of set) expect(n).toMatch(/^[A-Za-z0-9]+$/);
  });
  it("builds the tab url", () => {
    expect(agentTabUrl("abc")).toBe("about:blank#abc");
  });
});

describe("reportAgentTabClosed", () => {
  it("dedupes and swallows errors, retrying after failure", async () => {
    emb.embeddedClosed.mockResolvedValue(undefined);
    await reportAgentTabClosed("s1");
    await reportAgentTabClosed("s1");
    expect(emb.embeddedClosed).toHaveBeenCalledTimes(1);

    emb.embeddedClosed.mockReset();
    emb.embeddedClosed.mockRejectedValueOnce(new Error("x"));
    await expect(reportAgentTabClosed("s2")).resolves.toBeUndefined();
    emb.embeddedClosed.mockResolvedValue(undefined);
    await reportAgentTabClosed("s2");
    expect(emb.embeddedClosed).toHaveBeenCalledTimes(2);
  });

  it("does not re-send in-flight duplicates", async () => {
    let release!: () => void;
    emb.embeddedClosed.mockReturnValue(new Promise<void>((r) => (release = r)));
    const a = reportAgentTabClosed("s3");
    const b = reportAgentTabClosed("s3");
    release();
    await Promise.all([a, b]);
    expect(emb.embeddedClosed).toHaveBeenCalledTimes(1);
  });
});
