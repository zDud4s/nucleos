import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const events = vi.hoisted(() => ({
  unlisten: vi.fn(),
  handlers: new Map<string, (e: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    events.handlers.set(name, cb);
    return events.unlisten;
  }),
}));

const emb = vi.hoisted(() => ({
  embeddedPrepare: vi.fn(),
  embeddedRegister: vi.fn(),
  embeddedRequests: vi.fn(),
  embeddedAck: vi.fn(),
  embeddedClosed: vi.fn(),
}));
vi.mock("./browser-embedded", () => emb);

import {
  getBrowserState,
  handleAgentClose,
  handleAgentFocus,
  handleAgentOpen,
  handleEnvRebuilt,
  consumeFocus,
  ensureProjectAgent,
  resetBrowserStore,
  setTabMap,
  startBrowserBridge,
} from "./browser-store";
import { newTab, addAgentTab } from "./browser-tabs";

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
  Object.values(emb).forEach((f) => f.mockReset());
  events.unlisten.mockReset();
  events.handlers.clear();
  resetBrowserStore();
});
afterEach(() => {
  vi.useRealTimers();
});

function seedAgent(project: string, id: string, sessionId: string) {
  setTabMap((m) => addAgentTab(m, project, newTab(id, "about:blank", "agent", sessionId)));
}

describe("handleAgentOpen", () => {
  it("creates the agent tab, acks and records the session id with no page mounted", async () => {
    emb.embeddedAck.mockResolvedValue({ session_id: "s1" });
    await handleAgentOpen({ request_id: "r1", kind: "open", project: "p", url: "u" });
    const create = invoke.mock.calls.find((c) => c[0] === "browser_tab_create");
    expect(create?.[1]).toMatchObject({
      project: "p",
      role: "agent",
      bounds: { x: 0, y: 0, width: 800, height: 600 },
    });
    const tabs = getBrowserState().tabMap.p.tabs;
    expect(tabs).toHaveLength(1);
    expect(tabs[0].sessionId).toBe("s1");
    expect(getBrowserState().created.has(tabs[0].id)).toBe(true);
    expect(emb.embeddedAck).toHaveBeenCalledWith("r1", expect.stringMatching(/^n/));
  });

  it("dedupes a request id while it is in flight", async () => {
    emb.embeddedAck.mockResolvedValue({ session_id: "s1" });
    const req = { request_id: "r1", kind: "open", project: "p", url: "u" };
    await Promise.all([handleAgentOpen(req), handleAgentOpen(req)]);
    expect(getBrowserState().tabMap.p.tabs).toHaveLength(1);
    expect(emb.embeddedAck).toHaveBeenCalledTimes(1);
  });

  it("trims the id from the de-dupe set once the request finishes (success and failure)", async () => {
    emb.embeddedAck.mockResolvedValueOnce({ session_id: "s1" });
    const ok = { request_id: "r1", kind: "open", project: "p", url: "u" };
    await handleAgentOpen(ok);
    // Finished: the id is no longer retained, so the set cannot grow without bound.
    emb.embeddedAck.mockResolvedValueOnce({ session_id: "s2" });
    await handleAgentOpen(ok);
    expect(emb.embeddedAck).toHaveBeenCalledTimes(2);

    emb.embeddedAck.mockRejectedValueOnce(new Error("nope"));
    const bad = { request_id: "r2", kind: "open", project: "p", url: "u" };
    await handleAgentOpen(bad);
    emb.embeddedAck.mockResolvedValueOnce({ session_id: "s3" });
    await handleAgentOpen(bad);
    expect(emb.embeddedAck).toHaveBeenCalledTimes(4);
  });

  it("drops the tab and sets a notice when the ack fails", async () => {
    emb.embeddedAck.mockRejectedValue(new Error("nope"));
    await handleAgentOpen({ request_id: "r2", kind: "open", project: "p", url: "u" });
    expect(getBrowserState().tabMap.p.tabs).toHaveLength(0);
    expect(getBrowserState().created.size).toBe(0);
    expect(getBrowserState().agentNotice.p).toBe("nope");
    expect(invoke.mock.calls.some((c) => c[0] === "browser_tab_close")).toBe(true);
  });
});

describe("handleAgentClose", () => {
  it("closes and removes the tab of the session", () => {
    seedAgent("p", "t1", "s1");
    handleAgentClose({ session_id: "s1" });
    expect(getBrowserState().tabMap.p.tabs).toHaveLength(0);
    expect(invoke).toHaveBeenCalledWith("browser_tab_close", { tabId: "t1" });
  });

  it("ignores an unknown session", () => {
    seedAgent("p", "t1", "s1");
    handleAgentClose({ session_id: "zz" });
    expect(getBrowserState().tabMap.p.tabs).toHaveLength(1);
  });
});

describe("handleAgentFocus", () => {
  it("activates the tab and sets a focus intent", () => {
    seedAgent("p", "t1", "s1");
    setTabMap((m) => addAgentTab(m, "p", newTab("t2", "about:blank", "agent", "s2")));
    expect(getBrowserState().tabMap.p.activeId).toBe("t1");
    handleAgentFocus({ session_id: "s2" });
    expect(getBrowserState().tabMap.p.activeId).toBe("t2");
    const first = getBrowserState().focusIntent;
    expect(first).toMatchObject({ project: "p", sessionId: "s2" });
    handleAgentFocus({ session_id: "s1" });
    expect(getBrowserState().focusIntent!.nonce).toBeGreaterThan(first!.nonce);
    consumeFocus();
    expect(getBrowserState().focusIntent).toBeNull();
  });

  it("ignores an unknown session", () => {
    handleAgentFocus({ session_id: "nope" });
    expect(getBrowserState().focusIntent).toBeNull();
  });
});

describe("handleEnvRebuilt", () => {
  it("drops the tabs and reports closed for agent tabs with a session only", async () => {
    emb.embeddedClosed.mockResolvedValue(undefined);
    seedAgent("p", "a1", "env-s1");
    setTabMap((m) => addAgentTab(m, "p", newTab("a2", "about:blank", "agent")));
    setTabMap((m) => addAgentTab(m, "p", newTab("o1", "https://x", "owner", "env-owner")));
    handleEnvRebuilt({ project: "p", role: "agent", closedTabIds: ["a1", "a2", "o1", "ghost"] });
    expect(getBrowserState().tabMap.p.tabs).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(0);
    expect(emb.embeddedClosed).toHaveBeenCalledTimes(1);
    expect(emb.embeddedClosed).toHaveBeenCalledWith("env-s1");
  });
});

describe("startBrowserBridge", () => {
  it("polls at 1 s, dispatches, and stop halts polling and unlistens", async () => {
    emb.embeddedRequests.mockResolvedValue([]);
    const stop = startBrowserBridge();
    await vi.advanceTimersByTimeAsync(999);
    expect(emb.embeddedRequests).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(emb.embeddedRequests).toHaveBeenCalledTimes(1);
    expect(events.handlers.has("browser://env-rebuilt")).toBe(true);
    stop();
    // env-rebuilt + tab-state + new-window + process-failed + runtime-missing.
    expect(events.unlisten).toHaveBeenCalledTimes(5);
    await vi.advanceTimersByTimeAsync(5000);
    expect(emb.embeddedRequests).toHaveBeenCalledTimes(1);
  });

  it("unlistens even when stopped before listen resolves", async () => {
    emb.embeddedRequests.mockResolvedValue([]);
    const stop = startBrowserBridge();
    stop();
    await vi.advanceTimersByTimeAsync(0);
    expect(events.unlisten).toHaveBeenCalledTimes(5);
  });

  function emit(name: string, payload: unknown) {
    events.handlers.get(`browser://${name}`)?.({ payload });
  }

  it("keeps tab state, crash and popups flowing with no page mounted", async () => {
    emb.embeddedRequests.mockResolvedValue([]);
    emb.embeddedClosed.mockResolvedValue(undefined);
    setTabMap((m) => addAgentTab(m, "p", newTab("o1", "about:blank", "owner")));
    seedAgent("p", "a1", "crash-s1");
    const stop = startBrowserBridge();
    await vi.advanceTimersByTimeAsync(0);

    emit("tab-state", {
      tabId: "a1",
      url: "https://x/",
      title: "X",
      loading: true,
      canGoBack: true,
      canGoForward: false,
      secure: true,
    });
    const agent = () => getBrowserState().tabMap.p.tabs.find((t) => t.id === "a1")!;
    expect(agent()).toMatchObject({ url: "https://x/", title: "X", loading: true, secure: true });

    // A popup from an agent tab is dropped; one from an owner tab opens an owner tab.
    emit("new-window", { fromTabId: "a1", url: "https://pop/" });
    expect(invoke.mock.calls.some((c) => c[0] === "browser_tab_create")).toBe(false);
    emit("new-window", { fromTabId: "o1", url: "https://pop/" });
    await vi.advanceTimersByTimeAsync(0);
    expect(invoke.mock.calls.find((c) => c[0] === "browser_tab_create")?.[1]).toMatchObject({
      project: "p",
      role: "owner",
      url: "https://pop/",
    });
    expect(getBrowserState().tabMap.p.tabs).toHaveLength(3);

    emit("runtime-missing", {});
    expect(getBrowserState().runtimeMissing).toBe(true);

    // A crash flags the tab, reports the session closed, then forgets the session id.
    emit("process-failed", { tabId: "a1", kind: "crashed" });
    expect(agent().failed).toBe(true);
    expect(emb.embeddedClosed).toHaveBeenCalledWith("crash-s1");
    await vi.advanceTimersByTimeAsync(0);
    expect(agent().sessionId).toBeUndefined();
    stop();
  });


  it("routes a focus request to the store", async () => {
    seedAgent("p", "t1", "s1");
    emb.embeddedRequests.mockResolvedValue([
      { request_id: "f1", kind: "focus", project: "p", session_id: "s1" },
    ]);
    const stop = startBrowserBridge();
    await vi.advanceTimersByTimeAsync(1000);
    expect(getBrowserState().focusIntent).toMatchObject({ project: "p", sessionId: "s1" });
    stop();
  });
});

describe("startBrowserBridge poller resilience", () => {
  it("does not stall GET /requests behind a slow ack", async () => {
    let release!: (v: { session_id: string }) => void;
    emb.embeddedAck.mockReturnValue(new Promise((r) => (release = r)));
    emb.embeddedRequests.mockResolvedValue([
      { request_id: "r1", kind: "open", project: "p", url: "u" },
    ]);
    const stop = startBrowserBridge();
    await vi.advanceTimersByTimeAsync(1000);
    expect(emb.embeddedAck).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(3000);
    // The ack is still pending, yet polling kept going.
    expect(emb.embeddedRequests.mock.calls.length).toBeGreaterThanOrEqual(4);
    release({ session_id: "s1" });
    await vi.advanceTimersByTimeAsync(0);
    stop();
  });

  it.each([
    ["a network error", new Error("down")],
    ["404", Object.assign(new Error("gone"), { status: 404 })],
    ["409", Object.assign(new Error("pillar_off"), { status: 409 })],
    ["410", Object.assign(new Error("expired"), { status: 410 })],
  ])("re-registers the agent env when GET requests fails with %s after working", async (_n, err) => {
    emb.embeddedPrepare.mockResolvedValue({ proxy_addr: "127.0.0.1:1" });
    emb.embeddedRegister.mockResolvedValue(undefined);
    invoke.mockResolvedValue({ cdp_http: "http://127.0.0.1:9" });
    await ensureProjectAgent("p");
    expect(emb.embeddedRegister).toHaveBeenCalledTimes(1);

    emb.embeddedRequests.mockResolvedValueOnce([]).mockRejectedValueOnce(err).mockResolvedValue([]);
    const stop = startBrowserBridge();
    await vi.advanceTimersByTimeAsync(1000);
    expect(emb.embeddedRegister).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1000);
    expect(emb.embeddedPrepare).toHaveBeenCalledTimes(2);
    expect(emb.embeddedRegister).toHaveBeenCalledTimes(2);
    // Recovered: no further re-registration while polls succeed.
    await vi.advanceTimersByTimeAsync(3000);
    expect(emb.embeddedRegister).toHaveBeenCalledTimes(2);
    stop();
  });

  it("clears the agent notice once the env is ready again", async () => {
    emb.embeddedPrepare.mockRejectedValueOnce(new Error("fenced off"));
    await ensureProjectAgent("p");
    expect(getBrowserState().agentNotice.p).toBe("fenced off");
    emb.embeddedPrepare.mockResolvedValue({ proxy_addr: "127.0.0.1:1" });
    emb.embeddedRegister.mockResolvedValue(undefined);
    invoke.mockResolvedValue({ cdp_http: "http://127.0.0.1:9" });
    await ensureProjectAgent("p");
    expect(getBrowserState().agentNotice.p).toBeUndefined();
  });

  it("does not re-register when requests never worked", async () => {
    emb.embeddedPrepare.mockResolvedValue({ proxy_addr: "x" });
    emb.embeddedRegister.mockResolvedValue(undefined);
    invoke.mockResolvedValue({ cdp_http: "http://127.0.0.1:9" });
    await ensureProjectAgent("p");
    emb.embeddedRequests.mockRejectedValue(new Error("down"));
    const stop = startBrowserBridge();
    await vi.advanceTimersByTimeAsync(3000);
    expect(emb.embeddedRegister).toHaveBeenCalledTimes(1);
    stop();
  });
});
