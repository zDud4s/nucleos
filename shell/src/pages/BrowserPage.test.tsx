// §spec pilar-de-browser
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  handleAgentClose,
  handleAgentOpen,
  markCreated,
  requestFocus,
  resetBrowserStore,
  setTabMap,
  startBrowserBridge,
} from "../data/browser-store";
import { NO_PROJECT, addAgentTab, newTab, setTabSessionId } from "../data/browser-tabs";
import { resetAgentControlState } from "./AgentControlBar";
import { BrowserPage } from "./BrowserPage";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, (e: { payload: unknown }) => void>(),
  unlisten: vi.fn(),
  overlay: { open: false },
  ensureAgentEnv: vi.fn(),
  projects: [{ project_id: "alpha" }, { project_id: "beta" }] as { project_id: string }[],
  reportAgentTabClosed: vi.fn(),
  embeddedAck: vi.fn(),
  embeddedOwner: vi.fn(),
  embeddedKeep: vi.fn(),
}));

vi.mock("../data/browser-agent", () => ({
  ensureAgentEnv: mocks.ensureAgentEnv,
  mintNonce: () => "nonce-1",
  agentTabUrl: (n: string) => `about:blank#${n}`,
  reportAgentTabClosed: mocks.reportAgentTabClosed,
  startAgentPoller: () => () => {},
}));
vi.mock("../data/browser-embedded", () => ({
  embeddedAck: mocks.embeddedAck,
  embeddedOwner: mocks.embeddedOwner,
  embeddedKeep: mocks.embeddedKeep,
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    mocks.listeners.set(name, cb);
    return mocks.unlisten;
  }),
}));
vi.mock("../ui/overlay", () => ({ useOverlayOpen: () => mocks.overlay.open }));
vi.mock("../data/system", () => ({
  useProjects: () => ({ data: mocks.projects }),
}));

class FakeResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const rect = { width: 800, height: 600, y: 100 };

function emit(name: string, payload: unknown) {
  act(() => mocks.listeners.get(`browser://${name}`)?.({ payload }));
}

function calls(command: string): Record<string, unknown>[] {
  return mocks.invoke.mock.calls.filter(([c]) => c === command).map(([, args]) => args);
}

function last<T>(items: T[]): T | undefined {
  return items[items.length - 1];
}

async function openTab(): Promise<string> {
  fireEvent.click(screen.getAllByRole("button", { name: "New tab" })[0]);
  await waitFor(() => expect(calls("browser_tab_create").length).toBeGreaterThan(0));
  return last(calls("browser_tab_create"))!.tabId as string;
}

function state(tabId: string, over: Record<string, unknown> = {}) {
  return {
    tabId,
    url: "https://example.com/",
    title: "Example",
    loading: false,
    canGoBack: false,
    canGoForward: false,
    secure: true,
    ...over,
  };
}

let stopBridge: () => void = () => {};

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  mocks.invoke.mockReset();
  mocks.invoke.mockResolvedValue(undefined);
  mocks.listeners.clear();
  mocks.unlisten.mockClear();
  mocks.overlay.open = false;
  mocks.projects = [{ project_id: "alpha" }, { project_id: "beta" }];
  mocks.ensureAgentEnv.mockReset();
  mocks.ensureAgentEnv.mockResolvedValue({ ok: true, cdpHttp: "http://127.0.0.1:1" });
  resetBrowserStore();
  resetAgentControlState();
  // The shell root runs the bridge for the whole session; the page only reads the store.
  stopBridge = startBrowserBridge();
  mocks.embeddedOwner.mockReset();
  mocks.embeddedOwner.mockResolvedValue(undefined);
  mocks.embeddedKeep.mockReset();
  mocks.reportAgentTabClosed.mockReset();
  mocks.reportAgentTabClosed.mockResolvedValue(undefined);
  mocks.embeddedAck.mockReset();
  mocks.embeddedAck.mockResolvedValue({ session_id: "s1" });
  rect.width = 800;
  rect.height = 600;
  rect.y = 100;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
    this: HTMLElement,
  ) {
    // Only the placeholder has a layout in these tests.
    const on = this.dataset.testid === "browser-placeholder";
    const w = on ? rect.width : 0;
    const h = on ? rect.height : 0;
    const y = on ? rect.y : 0;
    return { x: 0, y, width: w, height: h, top: y, left: 0, right: w, bottom: y + h, toJSON() {} };
  });
});

afterEach(() => {
  stopBridge();
  vi.restoreAllMocks();
});

/** Make `browser_tab_create` hang until the returned function resolves it. */
function deferCreate(): () => void {
  let release: () => void = () => {};
  mocks.invoke.mockImplementation((command: string) =>
    command === "browser_tab_create"
      ? new Promise<void>((resolve) => {
          release = resolve;
        })
      : Promise.resolve(undefined),
  );
  return () => release();
}

describe("BrowserPage", () => {
  it("draws the page's actions at the end of the address row", async () => {
    render(<BrowserPage actions={<button type="button">Archive</button>} />);
    const address = await screen.findByRole("textbox", { name: "Address" });
    const row = address.closest(".browser-toolbar");
    expect(row).not.toBeNull();
    expect(row!.lastElementChild!.textContent).toBe("Archive");
  });

  it("creates an owner tab with a uuid and the placeholder bounds", async () => {
    render(<BrowserPage />);
    const tabId = await openTab();
    const args = calls("browser_tab_create")[0];
    expect(args).toMatchObject({ project: "alpha", role: "owner", url: "about:blank" });
    expect(tabId).toMatch(/^[0-9a-f-]{36}$/);
    expect(args.bounds).toEqual({ x: 0, y: 100, width: 800, height: 600 });
  });

  it("shows a new tab only after its creation resolves, hiding the old one meanwhile", async () => {
    render(<BrowserPage />);
    const first = await openTab();
    await waitFor(() => expect(last(calls("browser_tabs_show"))?.tabId).toBe(first));
    const release = deferCreate();
    mocks.invoke.mockClear();
    fireEvent.click(screen.getAllByRole("button", { name: "New tab" })[0]);
    await waitFor(() => expect(calls("browser_tab_create")).toHaveLength(1));
    const second = calls("browser_tab_create")[0].tabId as string;
    // While the second webview does not exist yet: nothing is shown, never the new id.
    await waitFor(() => expect(last(calls("browser_tabs_show"))?.tabId).toBeNull());
    expect(calls("browser_tabs_show").some((c) => c.tabId === second)).toBe(false);
    expect(calls("browser_tabs_show").some((c) => c.tabId === first)).toBe(false);
    await act(async () => release());
    await waitFor(() => expect(last(calls("browser_tabs_show"))?.tabId).toBe(second));
  });

  it("hides everything when the placeholder has no size", async () => {
    rect.width = 0;
    rect.height = 0;
    render(<BrowserPage />);
    await openTab();
    await waitFor(() => expect(calls("browser_tabs_show").length).toBeGreaterThan(0));
    expect(calls("browser_tabs_show").every((c) => c.tabId === null)).toBe(true);
  });

  it("re-syncs the bounds when an error banner shifts the placeholder", async () => {
    render(<BrowserPage />);
    await openTab();
    await waitFor(() => expect(last(calls("browser_tabs_show"))?.bounds).toMatchObject({ y: 100 }));
    // Make the next tabs_show fail: the error note appears and pushes the placeholder down.
    rect.y = 140;
    mocks.invoke.mockImplementation((command: string) =>
      command === "browser_tabs_show" && rect.y === 140 && calls("browser_tabs_show").length < 99
        ? Promise.reject(new Error("boom"))
        : Promise.resolve(undefined),
    );
    fireEvent.click(screen.getAllByRole("button", { name: "New tab" })[0]);
    await screen.findByText("boom");
    mocks.invoke.mockResolvedValue(undefined);
    await waitFor(() => expect(last(calls("browser_tabs_show"))?.bounds).toMatchObject({ y: 140 }));
  });

  it("navigates to the resolved omnibox url", async () => {
    render(<BrowserPage />);
    const tabId = await openTab();
    const input = screen.getByRole("textbox", { name: "Address" });
    fireEvent.change(input, { target: { value: "example.com" } });
    fireEvent.submit(input.closest("form")!);
    await waitFor(() => expect(calls("browser_tab_navigate")).toHaveLength(1));
    const sent = calls("browser_tab_navigate")[0];
    expect(sent.tabId).toBe(tabId);
    expect(String(sent.url)).toMatch(/^https?:\/\//);
  });

  it("reflects tab-state events in the title and the padlock", async () => {
    render(<BrowserPage />);
    const tabId = await openTab();
    expect(screen.getByRole("img", { name: "Connection not secure" })).toBeTruthy();
    emit("tab-state", state(tabId));
    expect(screen.getByRole("tab", { name: "Example" })).toBeTruthy();
    expect(screen.getByRole("img", { name: "Secure connection" })).toBeTruthy();
  });

  it("hides every webview while an overlay is open", async () => {
    mocks.overlay.open = true;
    const { rerender } = render(<BrowserPage />);
    await openTab();
    rerender(<BrowserPage />);
    await waitFor(() => expect(last(calls("browser_tabs_show"))?.tabId).toBeNull());
    mocks.overlay.open = false;
    rerender(<BrowserPage />);
    await waitFor(() => expect(last(calls("browser_tabs_show"))?.tabId).not.toBeNull());
  });

  it("hides every webview on unmount", async () => {
    const { unmount } = render(<BrowserPage />);
    await openTab();
    mocks.invoke.mockClear();
    unmount();
    expect(last(calls("browser_tabs_show"))?.tabId).toBeNull();
  });

  it("opens a new tab for a new-window event", async () => {
    render(<BrowserPage />);
    const tabId = await openTab();
    await waitFor(() => expect(mocks.listeners.size).toBe(6));
    emit("new-window", { fromTabId: tabId, url: "https://popup.example/" });
    await waitFor(() => expect(calls("browser_tab_create")).toHaveLength(2));
    expect(calls("browser_tab_create")[1]).toMatchObject({
      project: "alpha",
      role: "owner",
      url: "https://popup.example/",
    });
  });

  it("offers a reload after a process failure", async () => {
    render(<BrowserPage />);
    const tabId = await openTab();
    emit("process-failed", { tabId, kind: "crashed" });
    expect(screen.getByText("This tab crashed")).toBeTruthy();
    fireEvent.click(within(screen.getByRole("alert")).getByRole("button", { name: "Reload" }));
    expect(calls("browser_tab_reload")).toEqual([{ tabId }]);
    expect(screen.queryByText("This tab crashed")).toBeNull();
  });

  it("explains a missing WebView2 Runtime", async () => {
    render(<BrowserPage />);
    await waitFor(() => expect(mocks.listeners.size).toBe(6));
    emit("runtime-missing", {});
    expect(screen.getByText(/WebView2 Runtime is missing/)).toBeTruthy();
  });

  it("hides the old project's tab when switching project", async () => {
    render(<BrowserPage />);
    await openTab();
    mocks.invoke.mockClear();
    fireEvent.change(screen.getByRole("combobox", { name: "Project" }), { target: { value: "beta" } });
    await waitFor(() => expect(calls("browser_tabs_show").length).toBeGreaterThan(0));
    expect(calls("browser_tabs_show")[0].tabId).toBeNull();
    expect(screen.getByText("No tabs open in this project.")).toBeTruthy();
    expect(last(calls("browser_tabs_show"))?.tabId).toBeNull();
  });

  describe("no project", () => {
    beforeEach(() => {
      mocks.projects = [];
    });

    it("the project selector lists No project first and picks it when there are no projects", async () => {
      render(<BrowserPage />);
      const combo = screen.getByRole("combobox", { name: "Project" }) as HTMLSelectElement;
      expect(combo.options[0].textContent).toBe("No project");
      expect(combo.options[0].value).toBe(NO_PROJECT);
      expect(combo.value).toBe(NO_PROJECT);
      expect(screen.queryByText("No project to browse in yet.")).toBeNull();
      expect(
        (screen.getAllByRole("button", { name: "New tab" })[0] as HTMLButtonElement).disabled,
      ).toBe(false);
    });

    it("a new tab in No project is an owner tab with a null project", async () => {
      render(<BrowserPage />);
      await openTab();
      expect(calls("browser_tab_create")[0]).toMatchObject({
        project: null,
        role: "owner",
        url: "about:blank",
      });
    });

    it("an agent tab under No project shows the agent control bar", async () => {
      setTabMap((m) =>
        setTabSessionId(addAgentTab(m, NO_PROJECT, newTab("n-1", "https://a.example/", "agent")), "n-1", "s7"),
      );
      markCreated("n-1");
      render(<BrowserPage />);
      expect(screen.getByRole("combobox", { name: "Project" })).toHaveProperty("value", NO_PROJECT);
      expect(screen.getByText("Agent is driving")).toBeTruthy();
      expect(screen.getByRole("button", { name: "Take control" })).toBeTruthy();
    });
  });

  describe("agent tabs", () => {
    const openReq = { request_id: "r1", project: "alpha", url: "https://a.example/", kind: "embedded" };

    async function openAgent(): Promise<string> {
      await waitFor(() => expect(mocks.listeners.size).toBe(6));
      act(() => {
        void handleAgentOpen(openReq);
      });
      await waitFor(() => expect(mocks.embeddedAck).toHaveBeenCalled());
      await waitFor(() => expect(document.querySelector('[data-role="agent"]')).not.toBeNull());
      // Let the ack resolve and the session id land in the tab.
      await act(async () => {});
      return last(calls("browser_tab_create"))!.tabId as string;
    }

    it("ensures the agent environment once per project", async () => {
      render(<BrowserPage />);
      await waitFor(() => expect(mocks.ensureAgentEnv).toHaveBeenCalledWith("alpha"));
      const combo = screen.getByRole("combobox", { name: "Project" });
      fireEvent.change(combo, { target: { value: "beta" } });
      await waitFor(() => expect(mocks.ensureAgentEnv).toHaveBeenCalledWith("beta"));
      fireEvent.change(combo, { target: { value: "alpha" } });
      expect(mocks.ensureAgentEnv).toHaveBeenCalledTimes(2);
    });

    it("shows a quiet notice when the environment is unavailable, owner tabs still work", async () => {
      mocks.ensureAgentEnv.mockResolvedValue({ ok: false, reason: "no chrome" });
      render(<BrowserPage />);
      expect(await screen.findByText(/Agent browser unavailable: no chrome/)).toBeTruthy();
      await openTab();
      expect(calls("browser_tab_create")[0]).toMatchObject({ role: "owner" });
    });

    it("opens an agent tab and acknowledges it with the nonce", async () => {
      render(<BrowserPage />);
      await openAgent();
      expect(last(calls("browser_tab_create"))).toMatchObject({
        project: "alpha",
        role: "agent",
        url: "about:blank#nonce-1",
      });
      expect(mocks.embeddedAck).toHaveBeenCalledWith("r1", "nonce-1");
      expect(document.querySelector('[data-role="agent"]')).not.toBeNull();
    });

    it("shows the driving bar only while the agent tab is active", async () => {
      render(<BrowserPage />);
      expect(screen.queryByText("Agent is driving")).toBeNull();
      await openAgent();
      expect(screen.getByText("Agent is driving")).toBeTruthy();
      fireEvent.click(screen.getAllByRole("button", { name: "New tab" })[0]);
      await waitFor(() => expect(screen.queryByText("Agent is driving")).toBeNull());
    });

    it("closes the tab when the daemon asks", async () => {
      render(<BrowserPage />);
      const tabId = await openAgent();
      await waitFor(() => expect(mocks.embeddedAck).toHaveBeenCalled());
      act(() => handleAgentClose({ session_id: "s1" }));
      await waitFor(() => expect(calls("browser_tab_close")).toEqual([{ tabId }]));
      expect(document.querySelector('[data-role="agent"]')).toBeNull();
      expect(mocks.reportAgentTabClosed).not.toHaveBeenCalled();
    });

    it("reports a user close of an agent tab", async () => {
      render(<BrowserPage />);
      await openAgent();
      fireEvent.click(screen.getByRole("button", { name: /^Close/ }));
      expect(mocks.reportAgentTabClosed).toHaveBeenCalledWith("s1");
    });

    it("reports a process failure of an agent tab", async () => {
      render(<BrowserPage />);
      const tabId = await openAgent();
      await waitFor(() => expect(mocks.listeners.size).toBe(6));
      emit("process-failed", { tabId, kind: "crashed" });
      expect(mocks.reportAgentTabClosed).toHaveBeenCalledWith("s1");
    });

    /** Seed an agent tab as if it was opened while the page was not mounted. */
    function seedAgent(project: string, id: string, sessionId: string, url = "https://a.example/") {
      setTabMap((m) => setTabSessionId(addAgentTab(m, project, newTab(id, url, "agent")), id, sessionId));
      markCreated(id);
    }

    it("shows the control bar for an agent tab already in the store", async () => {
      seedAgent("alpha", "a-1", "s9");
      render(<BrowserPage />);
      expect(screen.getByText("Agent is driving")).toBeTruthy();
      expect(screen.getByRole("button", { name: "Take control" })).toBeTruthy();
      await waitFor(() => expect(last(calls("browser_tabs_show"))?.tabId).toBe("a-1"));
    });

    it("applies a focus intent: switches project, activates the tab, highlights the control", async () => {
      seedAgent("beta", "b-1", "s5");
      render(<BrowserPage />);
      expect(screen.queryByText("Agent is driving")).toBeNull();
      act(() => requestFocus("beta", "s5"));
      await screen.findByText("Agent is driving");
      expect((screen.getByRole("combobox", { name: "Project" }) as HTMLSelectElement).value).toBe("beta");
      expect(screen.getByRole("button", { name: "Take control" }).getAttribute("data-highlight")).toBe("true");
      await waitFor(() => expect(last(calls("browser_tabs_show"))?.tabId).toBe("b-1"));
    });

    it("opens an agent tab in my tabs as an owner tab", async () => {
      seedAgent("alpha", "a-1", "s9");
      render(<BrowserPage />);
      await waitFor(() => expect(mocks.listeners.size).toBe(6));
      emit("tab-state", state("a-1", { url: "https://deep.example/page" }));
      fireEvent.click(screen.getByRole("button", { name: "Take control" }));
      fireEvent.click(await screen.findByRole("button", { name: "Open in my tabs" }));
      await waitFor(() => expect(calls("browser_tab_create")).toHaveLength(1));
      expect(calls("browser_tab_create")[0]).toMatchObject({
        project: "alpha",
        role: "owner",
        url: "https://deep.example/page",
      });
      const id = calls("browser_tab_create")[0].tabId as string;
      await waitFor(() => expect(last(calls("browser_tabs_show"))?.tabId).toBe(id));
    });

    it("disables the omnibox and navigation while the agent drives, enables them after Take control", async () => {
      seedAgent("alpha", "a-1", "s-ctl");
      render(<BrowserPage />);
      await waitFor(() => expect(mocks.listeners.size).toBe(6));
      emit("tab-state", state("a-1", { canGoBack: true, canGoForward: true }));
      const disabled = (name: string) => (screen.getByRole("button", { name }) as HTMLButtonElement).disabled;
      expect((screen.getByRole("textbox", { name: "Address" }) as HTMLInputElement).disabled).toBe(true);
      expect(disabled("Back")).toBe(true);
      expect(disabled("Forward")).toBe(true);
      expect(disabled("Reload")).toBe(true);
      fireEvent.click(screen.getByRole("button", { name: "Take control" }));
      await screen.findByText("You are driving");
      expect((screen.getByRole("textbox", { name: "Address" }) as HTMLInputElement).disabled).toBe(false);
      expect(disabled("Back")).toBe(false);
      expect(disabled("Forward")).toBe(false);
      expect(disabled("Reload")).toBe(false);
    });

    it("cannot revive a detached agent tab after a process failure", async () => {
      seedAgent("alpha", "a-1", "s-dead");
      render(<BrowserPage />);
      await waitFor(() => expect(mocks.listeners.size).toBe(6));
      fireEvent.click(screen.getByRole("button", { name: "Take control" }));
      await screen.findByText("You are driving");
      emit("process-failed", { tabId: "a-1", kind: "crashed" });
      await waitFor(() => expect(mocks.reportAgentTabClosed).toHaveBeenCalledWith("s-dead"));
      await screen.findByText("This tab crashed");
      // The session id is cleared, so the tab is locked again and offers no reload.
      await waitFor(() =>
        expect((screen.getByRole("button", { name: "Reload" }) as HTMLButtonElement).disabled).toBe(true),
      );
      expect(within(screen.getByRole("alert")).queryByRole("button", { name: "Reload" })).toBeNull();
      expect(calls("browser_tab_reload")).toHaveLength(0);
    });

    it("drops a new-window event from an agent tab", async () => {
      render(<BrowserPage />);
      const tabId = await openAgent();
      await waitFor(() => expect(mocks.listeners.size).toBe(6));
      emit("new-window", { fromTabId: tabId, url: "https://popup.example/" });
      expect(calls("browser_tab_create")).toHaveLength(1);
    });
  });

  it("keeps the event listeners on unmount; stopping the bridge unlistens", async () => {
    const { unmount } = render(<BrowserPage />);
    await waitFor(() => expect(mocks.listeners.size).toBe(6));
    unmount();
    expect(mocks.unlisten).not.toHaveBeenCalled();
    stopBridge();
    await waitFor(() => expect(mocks.unlisten).toHaveBeenCalledTimes(6));
  });
});
