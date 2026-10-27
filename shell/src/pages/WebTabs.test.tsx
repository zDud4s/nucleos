import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { clearWebDraft } from "./Web";
import { requestFocus, resetBrowserStore } from "../data/browser-store";
import { daemonFetch, daemonState, renderApp } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  clearWebDraft();
  resetBrowserStore();
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();

  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  daemon.apiFetch.mockImplementation(daemonFetch(daemonState()));
});

// The page hands its panel toggles to the browser, which draws them at the end of its address row.
vi.mock("./BrowserPage", () => ({
  BrowserPage: ({ actions }: { actions?: import("react").ReactNode }) => (
    <div data-testid="browser-page">{actions}</div>
  ),
}));

// The panel itself is tested in AgentPanel.test.tsx; here only where the page puts it.
vi.mock("./AgentPanel", () => ({
  AgentPanel: () => <div data-testid="agent-panel" />,
}));

const archiveToggle = () => screen.getByRole("button", { name: "Archive" });
const agentToggle = () => screen.getByRole("button", { name: "Agent" });

describe("the Web page split", () => {
  it("is the browser alone: a title for assistive technology, no header band, no panel", async () => {
    await renderApp({ initialPath: "/web" });

    const title = await screen.findByRole("heading", { level: 1, name: "Web" });
    expect(title.classList.contains("sr-only")).toBe(true);
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    expect(document.querySelector(".web-split .ui-page-header")).toBeNull();
    const browser = await screen.findByTestId("browser-page");
    expect(within(browser).getByRole("toolbar", { name: "Web panels" })).toBeDefined();
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
  it("the Agent button toggles the agent panel beside the browser", async () => {
    const { router } = await renderApp({ initialPath: "/web" });
    const browser = await screen.findByTestId("browser-page");
    const toolbar = within(browser).getByRole("toolbar", { name: "Web panels" });
    expect(within(toolbar).getByRole("button", { name: "Agent" })).toBe(agentToggle());
    expect(agentToggle().getAttribute("aria-pressed")).toBe("false");

    fireEvent.click(agentToggle());
    await waitFor(() => expect(agentToggle().getAttribute("aria-pressed")).toBe("true"));
    const side = screen.getByRole("complementary", { name: "Side panel" });
    expect(within(side).getByTestId("agent-panel")).toBeDefined();
    // Beside the browser, never over it: the browser is not inside the side panel.
    expect(side.contains(screen.getByTestId("browser-page"))).toBe(false);
    expect(screen.getByTestId("browser-page")).toBe(browser);

    fireEvent.click(agentToggle());
    await waitFor(() => expect(agentToggle().getAttribute("aria-pressed")).toBe("false"));
    expect(screen.queryByRole("complementary", { name: "Side panel" })).toBeNull();

    // Like Sessions, the Agent panel gives way to the Archive.
    fireEvent.click(agentToggle());
    await waitFor(() => expect(agentToggle().getAttribute("aria-pressed")).toBe("true"));
    fireEvent.click(archiveToggle());
    await waitFor(() => expect(router.state.location.pathname).toBe("/web/archive"));
    expect(agentToggle().getAttribute("aria-pressed")).toBe("false");
  });

  it("a focus intent opens the agent panel", async () => {
    await renderApp({ initialPath: "/web" });
    await screen.findByTestId("browser-page");
    expect(agentToggle().getAttribute("aria-pressed")).toBe("false");

    act(() => requestFocus("p1", "s1"));

    await waitFor(() => expect(agentToggle().getAttribute("aria-pressed")).toBe("true"));
    const side = screen.getByRole("complementary", { name: "Side panel" });
    expect(within(side).getByTestId("agent-panel")).toBeDefined();
  });
});
