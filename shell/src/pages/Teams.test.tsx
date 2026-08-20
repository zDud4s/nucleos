import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import {
  Outlet,
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Teams } from "./Teams";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import type { TeamRun, TeamTrigger, TeamView, TriggerNext } from "../data/teams";
import { daemonFetch, daemonState, renderApp } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/* ------------------------------------------------------------ fixtures -- */

function teamView(overrides: Partial<TeamView> = {}): TeamView {
  return {
    id: "atendimento",
    name: "Atendimento",
    mission: "answer the customers who write in",
    director_agent_id: "ana",
    max_rounds: 3,
    max_parallel: 2,
    budget_usd: 10,
    max_open_actions: 5,
    max_live_runs: 1,
    created_at: "2026-08-18T09:00:00Z",
    updated_at: "2026-08-18T09:00:00Z",
    members: [],
    grants: [],
    ...overrides,
  };
}

function teamTrigger(overrides: Partial<TeamTrigger> = {}): TeamTrigger {
  return {
    id: 1,
    team_id: "atendimento",
    name: "morning digest",
    enabled: 0,
    source: "cron",
    cron: "0 9 * * *",
    timezone: "Europe/Lisbon",
    from_team: null,
    email_class: null,
    request: "summarise the queue",
    created_at: "2026-08-18T09:00:00Z",
    updated_at: "2026-08-18T09:00:00Z",
    ...overrides,
  };
}

/**
 * The team routes, over mutable state — the `Council.test.tsx` `councilFetch`
 * shape: the page polls the run list, so a queue of one-shot answers runs out
 * halfway through the second tick.
 */
function teamsFetch(
  teams: TeamView[],
  views: Record<string, TeamView>,
  runs: TeamRun[],
  triggers: TeamTrigger[],
  next: Record<number, TriggerNext>,
  opts: { onStartRun?: () => unknown } = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    if (path === "/agents") return [];
    if (path === "/teams") return teams;
    if (path === "/team-runs") return runs;
    if (path === "/team-triggers") return triggers;

    const nextMatch = /^\/team-triggers\/(\d+)\/next$/.exec(path);
    if (nextMatch !== null) {
      const id = Number(nextMatch[1]);
      return next[id] ?? { next: null, error: null };
    }

    const runsMatch = /^\/teams\/([^/]+)\/runs$/.exec(path);
    if (runsMatch !== null && init?.method === "POST") {
      if (opts.onStartRun !== undefined) return opts.onStartRun();
      return { id: "new-run-1" };
    }

    const detailMatch = /^\/teams\/([^/]+)$/.exec(path);
    if (detailMatch !== null) {
      const view = views[detailMatch[1]];
      if (view === undefined) throw new ApiRefusal(404, "not_found", "no such team");
      return view;
    }

    return undefined;
  };
}

/**
 * The page inside a two-route router, exactly like `Council.test.tsx`'s
 * `renderCouncil`: `renderApp` mounts the gate, the rail and its own live
 * queries around every assertion, which this machine cannot pay for more than
 * once. Only the case that proves the real tree registers both routes uses it,
 * at the end of this file.
 */
async function renderTeams(initialPath: string) {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({ getParentRoute: () => rootRoute, path: "/teams", component: Teams }),
    createRoute({ getParentRoute: () => rootRoute, path: "/teams/$teamId", component: Teams }),
  ];
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
    defaultPreload: false,
  });

  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { ...result, router, queryClient };
}

/** The `<section>` a panel's own heading belongs to, so an assertion can be scoped to one card. */
async function panelFor(headingText: string): Promise<HTMLElement> {
  const heading = await screen.findByRole("heading", { level: 2, name: headingText });
  const panel = heading.closest("section");
  if (panel === null) throw new Error(`no panel section found for heading "${headingText}"`);
  return panel as HTMLElement;
}

/**
 * Real time, past `ConfirmButton`'s 300ms dwell — the `Chats.test.tsx`
 * `afterDwell` idiom. A click landing inside the dwell reads as the tail of a
 * double-click and is swallowed rather than confirming.
 */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

/* -------------------------------------------------------------- the list -- */

describe("Teams - the list", () => {
  it("lists every department with its director and its ceilings", async () => {
    const teamA = teamView({ id: "atendimento", name: "Atendimento", director_agent_id: "ana", budget_usd: null });
    const teamB = teamView({ id: "vendas", name: "Vendas", director_agent_id: "bruno", budget_usd: 25 });
    daemon.apiFetch.mockImplementation(teamsFetch([teamA, teamB], {}, [], [], {}));

    await renderTeams("/teams");

    expect(await screen.findByText("Atendimento")).toBeDefined();
    expect(screen.getByText("Vendas")).toBeDefined();
    expect(screen.getByText(/director: ana/)).toBeDefined();
    expect(screen.getByText(/director: bruno/)).toBeDefined();
    // Absent is not zero: a null ceiling never reads as $0.
    expect(screen.getByText("no ceiling of its own")).toBeDefined();
    expect(screen.queryByText("$0.00")).toBeNull();
  });
});

/* ------------------------------------------------------------ the detail -- */

describe("Teams - the detail", () => {
  it("shows a team's roster, its grants and its rules when one is opened", async () => {
    const team = teamView({
      id: "atendimento",
      name: "Atendimento",
      members: ["ana", "bruno"],
      grants: [{ kind: "send_email", mode: "propose" }],
    });
    const rule = teamTrigger({ id: 1, team_id: "atendimento", name: "morning digest" });
    daemon.apiFetch.mockImplementation(teamsFetch([team], { atendimento: team }, [], [rule], {}));

    await renderTeams("/teams/atendimento");

    // The list row and the detail panel render the same team name at once —
    // a bare `findByText` would throw "multiple elements".
    const list = await screen.findByRole("list", { name: "Departments" });
    expect(within(list).getByText("Atendimento")).toBeDefined();
    const detail = await panelFor("Atendimento");
    expect(within(detail).getByText("ana")).toBeDefined();
    expect(within(detail).getByText("bruno")).toBeDefined();

    // The same grant selector appears in the "New department" form too — scope
    // to the edit panel, or a bare `findByLabelText` throws "multiple elements".
    const editPanel = await panelFor("Edit department");
    const grantSelect = within(editPanel).getByLabelText("send_email grant") as HTMLSelectElement;
    expect(grantSelect.value).toBe("propose");

    expect(await screen.findByText("morning digest")).toBeDefined();
  });

  it("renders a rule that does not fire on a clock as a sentence rather than an error", async () => {
    const team = teamView({ id: "atendimento", name: "Atendimento" });
    const rule = teamTrigger({
      id: 7,
      team_id: "atendimento",
      name: "on demand",
      source: "team_finished",
      cron: null,
      timezone: null,
    });
    daemon.apiFetch.mockImplementation(
      teamsFetch([team], { atendimento: team }, [], [rule], {
        7: { next: null, error: "this rule does not fire on a clock" },
      }),
    );

    await renderTeams("/teams/atendimento");

    expect(await screen.findByText("this rule does not fire on a clock")).toBeDefined();
    expect(screen.queryByText(/fail/i)).toBeNull();
    expect(document.querySelector(".ui-badge-danger")).toBeNull();
  });
});

/* ---------------------------------------------------------- trigger rules -- */

describe("Teams - arming and disarming a rule", () => {
  it("asks before arming a rule for a team with no ceiling, and never before disarming", async () => {
    const team = teamView({ id: "atendimento", name: "Atendimento", budget_usd: null });
    const disarmedRule = teamTrigger({ id: 1, team_id: "atendimento", name: "disarmed rule", enabled: 0 });
    const armedRule = teamTrigger({ id: 2, team_id: "atendimento", name: "armed rule", enabled: 1 });
    daemon.apiFetch.mockImplementation(
      teamsFetch([team], { atendimento: team }, [], [disarmedRule, armedRule], {}),
    );

    await renderTeams("/teams/atendimento");

    // Arming a no-ceiling team's rule is a ConfirmButton, and the dwell must
    // pass for real before the confirm click is honoured.
    const armButton = await screen.findByRole("button", { name: "Arm with no ceiling" });
    fireEvent.click(armButton);
    const confirmArm = await screen.findByRole("button", { name: "Arm it anyway" });
    await afterDwell();
    fireEvent.click(confirmArm);
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/team-triggers/1/enable", {
        method: "POST",
        body: JSON.stringify({ enabled: true }),
      });
    });

    // Disarming is always a plain button and fires on one click, with no
    // interlock at all.
    const disarmButton = await screen.findByRole("button", { name: "Disarm" });
    fireEvent.click(disarmButton);
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/team-triggers/2/enable", {
        method: "POST",
        body: JSON.stringify({ enabled: false }),
      });
    });
  });
});

/* -------------------------------------------------------------- start run -- */

describe("Teams - starting a run", () => {
  it("names the specialist the núcleo says is missing when a run will not start", async () => {
    const team = teamView({ id: "atendimento", name: "Atendimento" });
    daemon.apiFetch.mockImplementation(
      teamsFetch([team], { atendimento: team }, [], [], {}, {
        onStartRun: () => {
          throw new ApiRefusal(400, "bad_request", "`pesquisa` is on this team and not in the catalogue");
        },
      }),
    );

    await renderTeams("/teams/atendimento");

    const startPanel = await panelFor("Start a run");
    fireEvent.change(within(startPanel).getByLabelText("Request"), { target: { value: "find leads" } });
    fireEvent.click(within(startPanel).getByRole("button", { name: "Start" }));

    // The daemon's own sentence, verbatim — no toast and no modal anywhere.
    expect(await within(startPanel).findByText(/pesquisa.*not in the catalogue/)).toBeDefined();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("says the run list is the newest hundred and not the whole history", async () => {
    const team = teamView({ id: "atendimento", name: "Atendimento" });
    daemon.apiFetch.mockImplementation(teamsFetch([team], { atendimento: team }, [], [], {}));

    await renderTeams("/teams/atendimento");

    expect(await screen.findByText(/newest hundred/i)).toBeDefined();
  });
});

/* ------------------------------------------------------------- the route -- */

describe("Teams - the real route", () => {
  it("reaches the real /teams route with no placeholder left", async () => {
    const shared = daemonFetch(daemonState());
    const teams = teamsFetch([], {}, [], [], {});
    daemon.apiFetch.mockImplementation(async (path, init) => {
      if (
        path === "/teams" ||
        path.startsWith("/teams/") ||
        path === "/team-runs" ||
        path === "/team-triggers" ||
        path.startsWith("/team-triggers/") ||
        path === "/agents"
      ) {
        return teams(path, init);
      }
      return await shared(path, init);
    });

    await renderApp({ initialPath: "/teams" });

    expect(await screen.findByRole("heading", { level: 1, name: "Teams" })).toBeDefined();
    expect(screen.queryByText(/not yet wired/i)).toBeNull();
  });
});
