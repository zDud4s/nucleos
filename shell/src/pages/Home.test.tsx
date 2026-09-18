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
    expect(within(await card("Waiting on you")).getByText("2")).toBeDefined();
    expect((await card("Waiting on you")).textContent).toContain(
      "decisions held for you — not records, and not the calendar",
    );
    expect(within(await card("Window spend")).getByText("$1.42")).toBeDefined();

    // Five now, and the fifth is not a fifth reading of §6.1: "Subsystems healthy" is
    // System's own headline, on the first screen because that is where somebody finds out
    // a subsystem is down. The four above are still the four, and still say what §6.1
    // says they say.
    expect(screen.getAllByRole("article")).toHaveLength(5);
  });

  it("the headline counts the whole waiting queue", async () => {
    const answer = daemonFetch(daemonState({ proposals: [proposal({ id: 1 })] }));
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path === "/proposals/team-actions" ? [proposal({ id: 2, kind: "team-action" })] : answer(path, init),
    );

    await renderWithRouter(<Home />);

    expect(within(await card("Waiting on you")).getByText("2")).toBeDefined();
    expect((await screen.findByText(/2 waiting on you/)).textContent).toMatch(/; 2 waiting on you$/);
  });

  it("the waiting card says what the number is", async () => {
    daemon.apiFetch.mockImplementation(daemonFetch(daemonState({ proposals: [proposal({ id: 1 })] })));

    await renderWithRouter(<Home />);

    expect((await card("Waiting on you")).textContent).toContain(
      "decisions held for you — not records, and not the calendar",
    );
    const door = screen
      .getAllByRole("link", { name: "Waiting" })
      .find((link) => link.closest("p") !== null)
      ?.closest("p");
    expect(door?.textContent).toContain("every decision that stopped to ask you something, in one queue");
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

  it("the window spend shows its share of the ceiling, and shows no bar when there is no ceiling", async () => {
    daemon.apiFetch.mockImplementation(
      daemonFetch(daemonState({ budget: { ...daemonState().budget, window_spend_usd: 4.1, limit_usd: 5 } })),
    );
    const first = await renderWithRouter(<Home />);
    const spend = await card("Window spend");
    const fill = spend.querySelector(".ui-gauge-fill") as HTMLElement;
    expect(fill.style.width).toBe("82%");
    expect(spend.querySelector(".ui-gauge")?.className).toContain("ui-gauge-quantity");
    expect(spend.querySelector(".ui-gauge-head")).toBeNull();
    first.unmount();

    daemon.apiFetch.mockImplementation(
      daemonFetch(daemonState({ budget: { ...daemonState().budget, limit_usd: null } })),
    );
    await renderWithRouter(<Home />);
    expect((await card("Window spend")).querySelector(".ui-gauge")).toBeNull();
  });

  it("shows an em dash rather than a zero while nothing has been read", async () => {
    // A count nobody has managed to fetch and a count that is genuinely zero are
    // different pieces of news, and only one of them means you can go to lunch.
    daemon.apiFetch.mockReturnValue(new Promise(() => {}));

    await renderWithRouter(<Home />);

    for (const label of ["Projects", "Shadow decisions pending", "Waiting on you", "Window spend"]) {
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

  it("the headline names the worst live fact and links to system", async () => {
    // The harness's `daemonFetch` has no `/health/readout` case and falls through to
    // `undefined`, so the readout is wrapped AROUND it rather than passed through it.
    const answer = daemonFetch(daemonState({ proposals: [] }));
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path === "/health/readout"
        ? {
            status: "degraded",
            subsystems: [
              { name: "sqlite_pool", status: "ok" },
              { name: "worktree_disk", status: "degraded", reason: "low-disk-space" },
              { name: "browser_sidecar", status: "down", reason: "not-running" },
            ],
          }
        : answer(path, init),
    );

    await renderWithRouter(<Home />);

    // Named, and a door. "1 subsystem down" would send somebody to /system to find out
    // which one, and the answer is three words long — so the sentence says it, and the
    // sentence is the way there.
    const door = await screen.findByRole("link", {
      name: /1 subsystem down \(browser_sidecar\), 1 degraded/,
    });
    expect(door.getAttribute("href")).toBe("/system");

    // The waiting clause is still appended to it: the worst fact leads the sentence, it
    // does not replace it.
    expect(door.closest("p")?.textContent).toBe(
      "1 subsystem down (browser_sidecar), 1 degraded; nothing waiting on you",
    );

    /*
      And it is the wrong colour for a wrong fact.

      A bare `<Link>` here rendered Signal Cyan — the wordmark's colour, which the design
      spends on identity and navigation and on nothing that is a state. The worst live fact
      about the machine was being reported in the one colour that means "nothing is
      happening". `.ui-wrong` carries the tone and `.ui-wrong-door` takes it from the clause
      around it, because `base.css` styles `a` unlayered and a Tailwind utility cannot beat
      that. Both are classes rather than paint, because jsdom applies no stylesheet.
    */
    expect(door.className).toContain("ui-wrong-door");
    expect(door.parentElement?.className).toContain("ui-wrong");

    // And the card that counts the same fact wears the same tone. The figure reads
    // "2/3" whether or not anything is wrong; the tone is what says which it is.
    const subsystemsCard = screen.getByRole("article", { name: "Subsystems healthy" });
    expect(subsystemsCard.className).toBe("ui-stat");
    expect(subsystemsCard.querySelector(".ui-stat-value")?.className).toBe("ui-stat-value");
    // ui-wrong lands on the span StatCard's detail slot wraps, not on the slot's own
    // p.ui-stat-detail — Boundary.tsx's rule is that the whole sentence carries the tone.
    expect(
      screen
        .getByRole("article", { name: "Subsystems healthy" })
        .querySelector(".ui-stat-detail .ui-wrong"),
    ).not.toBeNull();
  });

  it("a subsystem down tones the whole sentence, not just the numeral", async () => {
    // The same fixture the neighbouring case uses — since round 11 the detail wears `.ui-wrong`
    // and `StatCard` has no tone of its own.
    const answer = daemonFetch(daemonState({ proposals: [] }));
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path === "/health/readout"
        ? {
            status: "degraded",
            subsystems: [
              { name: "sqlite_pool", status: "ok" },
              { name: "worktree_disk", status: "degraded", reason: "low-disk-space" },
              { name: "browser_sidecar", status: "down", reason: "not-running" },
            ],
          }
        : answer(path, init),
    );

    await renderWithRouter(<Home />);

    const card_ = await screen.findByRole("article", { name: "Subsystems healthy" });
    expect(card_.className).toBe("ui-stat");
    // The detail carries it too: one reading, one treatment — Boundary.tsx's rule.
    expect(card_.querySelector(".ui-stat-detail .ui-wrong")).not.toBeNull();
  });

  it("an all-clear keeps the mode sentence and all five cards", async () => {
    const answer = daemonFetch(
      daemonState({
        projects: [
          project({ project_id: "alpha", mode: "active" }),
          project({ project_id: "beta", mode: "shadow" }),
          project({ project_id: "gamma", mode: "shadow" }),
        ],
        proposals: [],
      }),
    );
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path === "/health/readout"
        ? {
            status: "ok",
            subsystems: [
              { name: "sqlite_pool", status: "ok" },
              { name: "worktree_disk", status: "ok" },
              { name: "browser_sidecar", status: "ok" },
            ],
          }
        : answer(path, init),
    );

    await renderWithRouter(<Home />);

    expect(await screen.findByText("1 acting, 2 in shadow; nothing waiting on you")).toBeDefined();
    expect(screen.queryByRole("link", { name: /subsystem/ })).toBeNull();

    // And the cards do not recede. A card that appeared only when something was wrong
    // would teach the reader that an absent card is an absent fact.
    expect(screen.getAllByRole("article")).toHaveLength(5);

    // Nor does the healthy card wear the tone. A figure that is always red says nothing
    // when something actually goes wrong.
    expect(screen.getByRole("article", { name: "Subsystems healthy" }).className).toBe("ui-stat");
    expect(
      screen
        .getByRole("article", { name: "Subsystems healthy" })
        .querySelector(".ui-stat-detail .ui-wrong"),
    ).toBeNull();
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
