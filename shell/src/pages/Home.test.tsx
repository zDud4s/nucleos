import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";
import { Home } from "./Home";
import { daemonFetch, daemonState, project, proposal, renderWithRouter } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

/** The card with this label, once its query has landed. */
async function card(label: string): Promise<HTMLElement> {
  return await screen.findByRole("article", { name: label });
}

describe("Home", () => {
  it("shows the four readings of §6.1 from the daemon's answers", async () => {
    daemon.apiFetch.mockImplementation(
      daemonFetch(
        daemonState({
          projects: [
            project({ project_id: "alpha", mode: "active", pending: 0 }),
            project({ project_id: "beta", mode: "shadow", pending: 4 }),
            project({ project_id: "gamma", mode: "off", pending: 3 }),
          ],
          proposals: [proposal({ id: 1 }), proposal({ id: 2 })],
        }),
      ),
    );

    await renderWithRouter(<Home />);

    const projects = await card("Projects");
    expect(within(projects).getByText("3")).toBeDefined();
    expect(within(projects).getByText("1 active · 1 shadow")).toBeDefined();

    // Summed, not counted: one project holding seven decisions is not the same
    // news as seven projects holding one.
    expect(within(await card("Shadow decisions pending")).getByText("7")).toBeDefined();
    expect(within(await card("Approval queue")).getByText("2")).toBeDefined();
    expect(within(await card("Window spend")).getByText("$1.42")).toBeDefined();

    // Five now, and the fifth is not a fifth reading of §6.1: "Subsystems healthy" is
    // System's own headline, on the first screen because that is where somebody finds out
    // a subsystem is down. The four above are still the four, and still say what §6.1
    // says they say.
    expect(screen.getAllByRole("article")).toHaveLength(5);
  });

  it("reads an absent ceiling as no ceiling, never as zero", async () => {
    daemon.apiFetch.mockImplementation(
      daemonFetch(daemonState({ budget: { ...daemonState().budget, limit_usd: null } })),
    );

    await renderWithRouter(<Home />);

    const spend = await card("Window spend");
    // `null` means nothing will ever stop the spend; `0.00` means nothing will
    // ever run. Rendering the first as the second invents a policy nobody set.
    expect(within(spend).getByText(/no ceiling/)).toBeDefined();
    expect(spend.textContent).not.toMatch(/0\.00/);
  });

  it("shows an em dash rather than a zero while nothing has been read", async () => {
    // A count nobody has managed to fetch and a count that is genuinely zero are
    // different pieces of news, and only one of them means you can go to lunch.
    daemon.apiFetch.mockReturnValue(new Promise(() => {}));

    await renderWithRouter(<Home />);

    for (const label of ["Projects", "Shadow decisions pending", "Approval queue", "Window spend"]) {
      expect(within(await card(label)).getByText("—")).toBeDefined();
    }
  });

  it("says out loud when a ceiling is holding autonomous work", async () => {
    daemon.apiFetch.mockImplementation(
      daemonFetch(
        daemonState({
          budget: { ...daemonState().budget, paused: true, reason: "the daily ceiling is spent" },
        }),
      ),
    );

    await renderWithRouter(<Home />);

    expect(await screen.findByText(/the daily ceiling is spent/)).toBeDefined();
  });

  it("mutates nothing — the first screen is a reading, not a console", async () => {
    daemon.apiFetch.mockImplementation(daemonFetch(daemonState({ projects: [project()] })));

    await renderWithRouter(<Home />);
    await card("Projects");

    // Every action lives one click away, on the page that also shows what you
    // would be acting on. A control here would be pressed by somebody looking at
    // the state of five seconds ago.
    expect(screen.queryAllByRole("button")).toHaveLength(0);
    expect(screen.getByRole("link", { name: /Autopilot/ })).toBeDefined();
    expect(screen.getByRole("link", { name: /Waiting/ })).toBeDefined();
  });
});
