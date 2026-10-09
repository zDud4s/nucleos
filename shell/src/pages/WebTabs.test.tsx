import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { daemonFetch, daemonState, renderApp } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();

  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  daemon.apiFetch.mockImplementation(daemonFetch(daemonState()));
});

const TABLIST = "Web views";

describe("the Web page tabs", () => {
  it("renders one Web header with Sessions first and selected", async () => {
    await renderApp({ initialPath: "/web" });

    expect(await screen.findAllByRole("heading", { level: 1, name: "Web" })).toHaveLength(1);
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    const list = screen.getByRole("tablist", { name: TABLIST });
    expect(within(list).getAllByRole("tab").map((tab) => tab.textContent)).toEqual(["Sessions", "Archive"]);
    expect(await screen.findByRole("tab", { name: "Sessions", selected: true })).toBeDefined();
  });

  it("switches tabs by moving the URL", async () => {
    const { router } = await renderApp({ initialPath: "/web" });
    await screen.findByRole("tab", { name: "Sessions", selected: true });
    expect(await screen.findByRole("heading", { level: 2, name: "Live sessions" })).toBeDefined();

    fireEvent.mouseDown(screen.getByRole("tab", { name: "Archive" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/web/archive"));

    fireEvent.mouseDown(screen.getByRole("tab", { name: "Sessions" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/web"));
  });

  it("opens the tab its URL names", async () => {
    const first = await renderApp({ initialPath: "/web/pages/42" });
    expect(await screen.findByRole("tab", { name: "Archive", selected: true })).toBeDefined();
    first.unmount();

    const second = await renderApp({ initialPath: "/web/archive" });
    expect(await screen.findByRole("tab", { name: "Archive", selected: true })).toBeDefined();
    second.unmount();

    await renderApp({ initialPath: "/web" });
    expect(await screen.findByRole("tab", { name: "Sessions", selected: true })).toBeDefined();
  });

  it("forwards /browser and the old /web/sessions to /web", async () => {
    for (const path of ["/browser", "/web/sessions"]) {
      const { router, unmount } = await renderApp({ initialPath: path });
      await waitFor(() => expect(router.state.location.pathname).toBe("/web"));
      unmount();
    }
  });

  it("lights one Web rail entry on all three URLs", async () => {
    for (const path of ["/web", "/web/pages/42", "/web/archive"]) {
      const { router, unmount } = await renderApp({ initialPath: path });
      const rail = within(await screen.findByRole("navigation", { name: "Sections" }));
      await waitFor(() => expect(router.state.location.pathname).toBe(path));
      expect(rail.getAllByRole("link", { name: "Web" })).toHaveLength(1);
      expect(rail.queryByRole("link", { name: "Browser" })).toBeNull();
      expect(rail.getByRole("link", { name: "Web" }).getAttribute("aria-current")).toBe("page");
      unmount();
    }
  });

  it("keeps the tablist mounted and focus on the tab when switching by keyboard", async () => {
    const { router } = await renderApp({ initialPath: "/web" });
    await screen.findByRole("tab", { name: "Sessions", selected: true });

    const list = screen.getByRole("tablist", { name: TABLIST });
    const sessions = screen.getByRole("tab", { name: "Sessions" });
    act(() => sessions.focus());
    fireEvent.keyDown(sessions, { key: "ArrowRight" });

    await waitFor(() => expect(router.state.location.pathname).toBe("/web/archive"));
    expect(await screen.findByRole("tab", { name: "Archive", selected: true })).toBeDefined();
    expect(screen.getByRole("tablist", { name: TABLIST })).toBe(list);
    expect(document.activeElement).toBe(screen.getByRole("tab", { name: "Archive" }));
  });
});
