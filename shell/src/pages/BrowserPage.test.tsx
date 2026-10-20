// §spec pilar-de-browser
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { BrowserPage } from "./BrowserPage";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, (e: { payload: unknown }) => void>(),
  unlisten: vi.fn(),
  overlay: { open: false },
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
  useProjects: () => ({ data: [{ project_id: "alpha" }, { project_id: "beta" }] }),
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

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  mocks.invoke.mockReset();
  mocks.invoke.mockResolvedValue(undefined);
  mocks.listeners.clear();
  mocks.unlisten.mockClear();
  mocks.overlay.open = false;
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
    await waitFor(() => expect(mocks.listeners.size).toBe(4));
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
    await waitFor(() => expect(mocks.listeners.size).toBe(4));
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

  it("unsubscribes from the events on unmount", async () => {
    const { unmount } = render(<BrowserPage />);
    await waitFor(() => expect(mocks.listeners.size).toBe(4));
    unmount();
    await waitFor(() => expect(mocks.unlisten).toHaveBeenCalledTimes(4));
  });
});
