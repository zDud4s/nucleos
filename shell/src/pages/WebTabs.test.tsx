import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { clearWebDraft } from "./Web";
import { daemonFetch, daemonState, renderApp } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  clearWebDraft();
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();

  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  daemon.apiFetch.mockImplementation(daemonFetch(daemonState()));
});

vi.mock("./BrowserPage", () => ({ BrowserPage: () => <div data-testid="browser-page" /> }));

const archiveToggle = () => screen.getByRole("button", { name: "Archive" });

describe("the Web page split", () => {
  it("renders one Web header with the browser as the main surface and no panel", async () => {
    await renderApp({ initialPath: "/web" });

    expect(await screen.findAllByRole("heading", { level: 1, name: "Web" })).toHaveLength(1);
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    expect(await screen.findByTestId("browser-page")).toBeDefined();
    expect(archiveToggle().getAttribute("aria-pressed")).toBe("false");
    expect(screen.queryByRole("complementary", { name: "Side panel" })).toBeNull();
  });

  it("toggles the Archive panel by moving the URL, keeping the browser mounted", async () => {
    const { router } = await renderApp({ initialPath: "/web" });
    const browser = await screen.findByTestId("browser-page");

    fireEvent.click(archiveToggle());
    await waitFor(() => expect(router.state.location.pathname).toBe("/web/archive"));
    await waitFor(() => expect(archiveToggle().getAttribute("aria-pressed")).toBe("true"));
    expect(screen.getByTestId("browser-page")).toBe(browser);

    fireEvent.click(archiveToggle());
    await waitFor(() => expect(router.state.location.pathname).toBe("/web"));
    expect(screen.getByTestId("browser-page")).toBe(browser);
  });

  it("shows the Archive panel on the archive URLs", async () => {
    for (const path of ["/web/pages/42", "/web/archive"]) {
      const { unmount } = await renderApp({ initialPath: path });
      await waitFor(() => expect(archiveToggle().getAttribute("aria-pressed")).toBe("true"));
      expect(screen.getByTestId("browser-page")).toBeDefined();
      unmount();
    }
  });

  it("opens the Sessions panel beside the browser and closes it when Archive opens", async () => {
    const { router } = await renderApp({ initialPath: "/web" });
    const sessions = await screen.findByRole("button", { name: /^Sessions/ });

    fireEvent.click(sessions);
    expect(await screen.findByRole("heading", { level: 2, name: "Live sessions" })).toBeDefined();
    expect(sessions.getAttribute("aria-pressed")).toBe("true");

    fireEvent.click(archiveToggle());
    await waitFor(() => expect(router.state.location.pathname).toBe("/web/archive"));
    expect(sessions.getAttribute("aria-pressed")).toBe("false");
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
});
