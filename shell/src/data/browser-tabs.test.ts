import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
const listen = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: (...a: unknown[]) => listen(...a) }));

import * as tabs from "./browser-tabs";

const bounds = { x: 1, y: 2, width: 3, height: 4 };

beforeEach(() => {
  invoke.mockReset();
  listen.mockReset();
  invoke.mockResolvedValue(undefined);
});

describe("browser tab commands", () => {
  it("envEnsure", async () => {
    invoke.mockResolvedValue({ cdp_http: null });
    expect(await tabs.envEnsure("p", "owner")).toEqual({ cdp_http: null });
    expect(invoke).toHaveBeenCalledWith("browser_env_ensure", { project: "p", role: "owner" });
  });

  it("tabCreate", async () => {
    await tabs.tabCreate("p", "agent", "t1", "https://a.b", bounds);
    expect(invoke).toHaveBeenCalledWith("browser_tab_create", {
      project: "p",
      role: "agent",
      tabId: "t1",
      url: "https://a.b",
      bounds,
    });
  });

  it("tabNavigate", async () => {
    await tabs.tabNavigate("t1", "https://x.y");
    expect(invoke).toHaveBeenCalledWith("browser_tab_navigate", { tabId: "t1", url: "https://x.y" });
  });

  it.each([
    ["tabBack", "browser_tab_back"],
    ["tabForward", "browser_tab_forward"],
    ["tabReload", "browser_tab_reload"],
    ["tabStop", "browser_tab_stop"],
    ["tabClose", "browser_tab_close"],
  ] as const)("%s", async (fn, cmd) => {
    await tabs[fn]("t1");
    expect(invoke).toHaveBeenCalledWith(cmd, { tabId: "t1" });
  });

  it("tabsShow", async () => {
    await tabs.tabsShow(null, bounds);
    expect(invoke).toHaveBeenCalledWith("browser_tabs_show", { tabId: null, bounds });
  });
});

describe("listenBrowserEvents", () => {
  it("routes payloads and unlistens all", async () => {
    const cbs: Record<string, (e: { payload: unknown }) => void> = {};
    const un = vi.fn();
    listen.mockImplementation(async (name: string, cb: (e: { payload: unknown }) => void) => {
      cbs[name] = cb;
      return un;
    });
    const h = {
      onTabState: vi.fn(),
      onNewWindow: vi.fn(),
      onProcessFailed: vi.fn(),
      onRuntimeMissing: vi.fn(),
    };
    const off = await tabs.listenBrowserEvents(h);
    cbs["browser://tab-state"]({ payload: "a" });
    cbs["browser://new-window"]({ payload: "b" });
    cbs["browser://process-failed"]({ payload: "c" });
    cbs["browser://runtime-missing"]({ payload: "d" });
    expect(h.onTabState).toHaveBeenCalledWith("a");
    expect(h.onNewWindow).toHaveBeenCalledWith("b");
    expect(h.onProcessFailed).toHaveBeenCalledWith("c");
    expect(h.onRuntimeMissing).toHaveBeenCalledWith("d");
    off();
    expect(un).toHaveBeenCalledTimes(4);
  });

  it("tolerates missing handlers", async () => {
    const cbs: Record<string, (e: { payload: unknown }) => void> = {};
    listen.mockImplementation(async (name: string, cb: (e: { payload: unknown }) => void) => {
      cbs[name] = cb;
      return vi.fn();
    });
    await tabs.listenBrowserEvents({});
    expect(() => cbs["browser://tab-state"]({ payload: 1 })).not.toThrow();
  });
});

describe("tab map helpers", () => {
  const mk = (id: string, session?: string) => tabs.newTab(id, `https://${id}.test`, "agent", session);
  const build = (): tabs.TabMap => ({
    p1: { tabs: [tabs.newTab("a", "https://a.test"), mk("b", "s-b"), mk("c", "s-c")], activeId: "b" },
    p2: { tabs: [mk("d", "s-d")], activeId: "d" },
  });

  it("newTab defaults to owner without a session", () => {
    const t = tabs.newTab("x", "https://x.test");
    expect(t.role).toBe("owner");
    expect(t.sessionId).toBeUndefined();
    expect(t.failed).toBe(false);
    expect(tabs.newTab("y", "u", "agent", "s").sessionId).toBe("s");
  });

  it("addAgentTab keeps the active tab when one exists", () => {
    const next = tabs.addAgentTab(build(), "p1", mk("e"));
    expect(next.p1.tabs.map((t) => t.id)).toEqual(["a", "b", "c", "e"]);
    expect(next.p1.activeId).toBe("b");
  });

  it("addAgentTab activates when there is none", () => {
    const next = tabs.addAgentTab({}, "p9", mk("e"));
    expect(next.p9.activeId).toBe("e");
    expect(next.p9.tabs).toHaveLength(1);
  });

  it("finds tabs by id and session across projects", () => {
    const map = build();
    expect(tabs.findTabById(map, "d")?.project).toBe("p2");
    expect(tabs.findTabBySessionId(map, "s-c")?.tab.id).toBe("c");
    expect(tabs.findTabById(map, "zz")).toBeUndefined();
    expect(tabs.findTabBySessionId(map, "zz")).toBeUndefined();
  });

  it("setTabSessionId sets the session without mutating the input", () => {
    const map = build();
    const next = tabs.setTabSessionId(map, "a", "s-a");
    expect(tabs.findTabBySessionId(next, "s-a")?.tab.id).toBe("a");
    expect(map.p1.tabs[0].sessionId).toBeUndefined();
    expect(tabs.setTabSessionId(map, "nope", "s")).toBe(map);
  });

  it("removeTabBySessionId removes the right tab and picks the neighbour when active", () => {
    const map = build();
    const res = tabs.removeTabBySessionId(map, "s-b");
    expect(res.removed?.project).toBe("p1");
    expect(res.removed?.tab.id).toBe("b");
    expect(res.map.p1.tabs.map((t) => t.id)).toEqual(["a", "c"]);
    expect(res.map.p1.activeId).toBe("c");
    expect(res.map.p2).toBe(map.p2);
  });

  it("removing the last remaining tab clears the active id", () => {
    const res = tabs.removeTabBySessionId(build(), "s-d");
    expect(res.map.p2).toEqual({ tabs: [], activeId: null });
  });

  it("removing a non-active tab keeps the active id", () => {
    const res = tabs.removeTabById(build(), "c");
    expect(res.map.p1.activeId).toBe("b");
  });

  it("removeTabBySessionId is a no-op for an unknown session", () => {
    const map = build();
    const res = tabs.removeTabBySessionId(map, "unknown");
    expect(res.map).toBe(map);
    expect(res.removed).toBeUndefined();
  });

  it("never mutates the input map", () => {
    const map = build();
    const snapshot = JSON.parse(JSON.stringify(map));
    tabs.addAgentTab(map, "p1", mk("e"));
    tabs.setTabSessionId(map, "a", "s-a");
    tabs.removeTabBySessionId(map, "s-b");
    expect(map).toEqual(snapshot);
  });
});
