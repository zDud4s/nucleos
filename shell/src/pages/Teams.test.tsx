import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
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
import type { TeamAction, TeamRun, TeamRunView, TeamTrigger, TeamView, TriggerNext } from "../data/teams";
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
    id: "financas",
    name: "Finanças",
    mission: "keep the books straight",
    director_agent_id: "controller",
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

function teamRun(overrides: Partial<TeamRun> = {}): TeamRun {
  return {
    id: "run-1",
    team_id: "financas",
    request: "reconcile October",
    workspace: "teams/financas/run-1",
    state: "working",
    director_node: "none",
    director_run_id: null,
    round: 2,
    next_ordinal: 3,
    dry_rounds: 0,
    plan_retries: 0,
    replanned: "no",
    outcome: null,
    why: null,
    created_at: "2026-08-24T09:00:00Z",
    updated_at: "2026-08-24T09:05:00Z",
    finished_at: null,
    trigger_id: null,
    parent_id: null,
    root_id: "run-1",
    depth: 0,
    ...overrides,
  };
}

function teamTrigger(overrides: Partial<TeamTrigger> = {}): TeamTrigger {
  return {
    id: 1,
    team_id: "financas",
    name: "monthly reconciliation",
    enabled: 0,
    source: "cron",
    cron: "0 7 1 * *",
    timezone: "Europe/Lisbon",
    from_team: null,
    email_class: null,
    request: "reconcile last month",
    created_at: "2026-08-18T09:00:00Z",
    updated_at: "2026-08-18T09:00:00Z",
    ...overrides,
  };
}

interface Fake {
  teams?: TeamView[];
  runs?: TeamRun[];
  triggers?: TeamTrigger[];
  actions?: TeamAction[];
  /** `GET /team-runs/{id}` — only live tasks are ever asked for. */
  runViews?: Record<string, TeamRunView>;
  next?: Record<number, TriggerNext>;
}

/**
 * The console's routes, over mutable state — the `Council.test.tsx`
 * `councilFetch` shape: the page polls the run list, so a queue of one-shot
 * answers runs out halfway through the second tick.
 */
function teamsFetch(fake: Fake): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path) => {
    if (path === "/agents") return [];
    if (path === "/teams") return fake.teams ?? [];
    if (path === "/team-runs") return fake.runs ?? [];
    if (path === "/team-triggers") return fake.triggers ?? [];
    if (path === "/team-actions") return fake.actions ?? [];

    const nextMatch = /^\/team-triggers\/(\d+)\/next$/.exec(path);
    if (nextMatch !== null) return fake.next?.[Number(nextMatch[1])] ?? { next: null, error: null };

    const runMatch = /^\/team-runs\/([^/]+)$/.exec(path);
    if (runMatch !== null) return fake.runViews?.[runMatch[1]];

    return undefined;
  };
}

/**
 * The console inside a one-route router, exactly like `Council.test.tsx`'s
 * `renderCouncil`: `renderApp` mounts the gate, the rail and its own live
 * queries around every assertion, which this machine cannot pay for more than
 * once. Only the case that proves the real tree registers the route uses it, at
 * the end of this file.
 */
async function renderTeams() {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({ getParentRoute: () => rootRoute, path: "/teams", component: Teams }),
    // Registered so the cards' links resolve; the bench itself is `Bench.test.tsx`.
    createRoute({ getParentRoute: () => rootRoute, path: "/teams/$teamId", component: () => null }),
    createRoute({ getParentRoute: () => rootRoute, path: "/team-runs/$runId", component: () => null }),
  ];
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: ["/teams"] }),
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

/* -------------------------------------------------------------- the matrix -- */

describe("Teams - who works where", () => {
  it("draws one cell for every (specialist, department) pair GET /teams returns", async () => {
    // Three specialists across two departments, one of whom serves both — the
    // fact the old page could not show at all, because a roster was only
    // visible once you had opened the department it belonged to.
    const financas = teamView({
      id: "financas",
      name: "Finanças",
      director_agent_id: "controller",
      members: ["controller", "auditor"],
    });
    const marketing = teamView({
      id: "marketing",
      name: "Marketing",
      director_agent_id: "writer",
      members: ["writer", "auditor"],
    });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [financas, marketing] }));

    await renderTeams();

    const matrix = await screen.findByRole("table");
    // Three specialists (controller, auditor, writer) × two departments.
    const cells = matrix.querySelectorAll(".teams-matrix-cell");
    expect(cells).toHaveLength(6);

    // And each cell says which of the three standings it is, in words.
    expect(within(matrix).getAllByText("directs")).toHaveLength(2);
    expect(within(matrix).getAllByText("on staff")).toHaveLength(2);
    expect(within(matrix).getAllByText("not on staff")).toHaveLength(2);

    // The headcount row counts the director whether or not the roster does.
    expect(within(matrix).getAllByText("2")).toHaveLength(2);
  });

  it("keeps a column for a department with nobody in it rather than dropping it", async () => {
    // The state a department is in between being created and being staffed —
    // and the state that stops every task it is asked to run. It must not be
    // the one column that disappears.
    const staffed = teamView({ id: "financas", name: "Finanças", director_agent_id: "controller", members: ["controller"] });
    const empty = teamView({
      id: "vendas",
      name: "Vendas",
      director_agent_id: "",
      members: [],
      budget_usd: null,
    });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [staffed, empty] }));

    await renderTeams();

    const matrix = await screen.findByRole("table");
    expect(within(matrix).getByRole("columnheader", { name: "Vendas" })).toBeDefined();
    // One specialist × two departments: the empty column is a column of
    // "not on staff", not a missing column.
    expect(matrix.querySelectorAll(".teams-matrix-cell")).toHaveLength(2);
    expect(within(matrix).getByText("not on staff")).toBeDefined();
    // Its headcount is zero, and zero is drawn.
    expect(within(matrix).getByText("0")).toBeDefined();
  });
});

/* -------------------------------------------------------------- the cards -- */

describe("Teams - the department card", () => {
  it("draws occupancy as a bar and a per-task rule as a chip with no bar", async () => {
    const team = teamView({
      id: "financas",
      name: "Finanças",
      max_live_runs: 2,
      max_open_actions: 5,
      max_rounds: 4,
      max_parallel: 2,
      budget_usd: 5,
      members: ["controller"],
    });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [team] }));

    await renderTeams();

    const card = await screen.findByRole("article", { name: "Finanças" });

    // Occupancy: something can be in it, so it gets a bar.
    expect(within(card).getByRole("img", { name: "at work: 0 of 2" })).toBeDefined();
    expect(within(card).getByRole("img", { name: "waiting on you: 0 of 5" })).toBeDefined();

    // Per task: nothing is consuming these, so no bar exists for them at all.
    expect(within(card).getByText("rounds")).toBeDefined();
    expect(within(card).getByText("≤ 4")).toBeDefined();
    expect(within(card).getByText("≤ $5.00")).toBeDefined();
    expect(within(card).queryByRole("img", { name: /rounds/ })).toBeNull();
  });

  it("never writes an absent budget ceiling as zero", async () => {
    const team = teamView({ id: "financas", name: "Finanças", budget_usd: null });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [team] }));

    await renderTeams();

    const card = await screen.findByRole("article", { name: "Finanças" });
    expect(within(card).getByText("no ceiling")).toBeDefined();
    expect(within(card).queryByText("≤ $0.00")).toBeNull();
  });

  it("shows a live task with what it has spent against this department's per-task ceiling", async () => {
    const team = teamView({ id: "financas", name: "Finanças", budget_usd: 5, max_live_runs: 1 });
    const live = teamRun({ id: "run-1", team_id: "financas", state: "working", request: "reconcile October" });
    daemon.apiFetch.mockImplementation(
      teamsFetch({
        teams: [team],
        runs: [live],
        runViews: { "run-1": { ...live, items: [], cost_usd: 1.2 } },
      }),
    );

    await renderTeams();

    const card = await screen.findByRole("article", { name: "Finanças" });
    // "at work" is both the card's derived state badge and the occupancy
    // meter's label — the badge is the one under test here.
    expect(within(card).getByText("at work", { selector: ".ui-badge" })).toBeDefined();
    expect(within(card).getByText("reconcile October")).toBeDefined();
    // Money written as money — the one place in this pillar where a spend and
    // the ceiling it runs against both exist.
    expect(await within(card).findByRole("img", { name: "spent on this task: $1.20 of $5.00" })).toBeDefined();
  });

  it("names the armed routine it is reporting on rather than claiming to be the soonest", async () => {
    const team = teamView({ id: "financas", name: "Finanças" });
    const armed = teamTrigger({ id: 3, team_id: "financas", name: "monthly reconciliation", enabled: 1 });
    const alsoArmed = teamTrigger({ id: 4, team_id: "financas", name: "weekly sweep", enabled: 1 });
    daemon.apiFetch.mockImplementation(
      teamsFetch({
        teams: [team],
        triggers: [armed, alsoArmed],
        next: { 3: { next: "2026-09-01T07:00:00Z", error: null } },
      }),
    );

    await renderTeams();

    const card = await screen.findByRole("article", { name: "Finanças" });
    expect(within(card).getByText("monthly reconciliation")).toBeDefined();
    expect(within(card).getByText("+1")).toBeDefined();
  });

  it("renders a rule that does not fire on a clock as a sentence rather than an error", async () => {
    const team = teamView({ id: "financas", name: "Finanças" });
    const rule = teamTrigger({ id: 7, team_id: "financas", name: "on demand", source: "team_finished", enabled: 1, cron: null });
    daemon.apiFetch.mockImplementation(
      teamsFetch({
        teams: [team],
        triggers: [rule],
        next: { 7: { next: null, error: "this rule does not fire on a clock" } },
      }),
    );

    await renderTeams();

    expect(await screen.findByText("this rule does not fire on a clock")).toBeDefined();
    expect(document.querySelector(".ui-badge-danger")).toBeNull();
  });
});

/* ------------------------------------------------------------ the header -- */

describe("Teams - the headline", () => {
  it("counts departments and specialists, and never a spend the daemon does not report", async () => {
    const financas = teamView({ id: "financas", name: "Finanças", director_agent_id: "controller", members: ["controller", "auditor"] });
    const marketing = teamView({ id: "marketing", name: "Marketing", director_agent_id: "writer", members: ["writer"] });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [financas, marketing] }));

    await renderTeams();

    const headline = await screen.findByText(/2 departments · 3 specialists · none at work/);
    // There is no per-department spend anywhere in the núcleo, so the console
    // aggregates none — the only money on this page is a per-task ceiling on a
    // card, and even that is a rule rather than a total. See the module header.
    expect(headline.textContent).not.toMatch(/\$/);
    expect(screen.queryByText(/spent/i)).toBeNull();
  });

  it("keeps the create form closed until it is asked for", async () => {
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [teamView()] }));

    await renderTeams();

    // The old page opened an eleven-field editor above a list nobody had read.
    expect(await screen.findByRole("button", { name: "New department" })).toBeDefined();
    expect(screen.queryByRole("heading", { level: 2, name: "New department" })).toBeNull();
  });
});

/* ------------------------------------------------------------- the route -- */

describe("Teams - the real route", () => {
  it("reaches the real /teams route with no placeholder left", async () => {
    const shared = daemonFetch(daemonState());
    const teams = teamsFetch({});
    daemon.apiFetch.mockImplementation(async (path, init) => {
      if (
        path === "/teams" ||
        path.startsWith("/teams/") ||
        path === "/team-runs" ||
        path.startsWith("/team-runs/") ||
        path === "/team-actions" ||
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
