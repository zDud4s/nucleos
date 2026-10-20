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
