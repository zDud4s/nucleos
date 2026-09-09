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

/** One open action of `run-1`. Its team comes back through the run list, never off the action. */
function teamAction(overrides: Partial<TeamAction> = {}): TeamAction {
  return {
    id: 1,
    team_run_id: "run-1",
    ordinal: 0,
    kind: "send_email",
    payload: "{}",
    why: "the customer asked for the invoice",
    proposal_id: 9,
    state: "pending",
    error: null,
    created_at: "2026-08-24T09:00:00Z",
    executed_at: null,
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

    const matrix = await screen.findByRole("table", { name: /specialists? across/ });
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

    const matrix = await screen.findByRole("table", { name: /specialists? across/ });
    expect(within(matrix).getByRole("columnheader", { name: "Vendas" })).toBeDefined();
    // One specialist × two departments: the empty column is a column of
    // "not on staff", not a missing column.
    expect(matrix.querySelectorAll(".teams-matrix-cell")).toHaveLength(2);
    expect(within(matrix).getByText("not on staff")).toBeDefined();
    // Its headcount is zero, and zero is drawn.
    expect(within(matrix).getByText("0")).toBeDefined();
  });
});

/* -------------------------------------------------------------- the table -- */

/** The teams table, by its caption — the matrix is the other one. */
function departments() {
  return screen.findByRole("table", { name: /Every team/ });
}

describe("Teams - the table", () => {
  it("gives every team the same row, whatever it happens to be doing", async () => {
    // The defect this shape exists to make impossible. As cards, a department
    // with a task running carried a block the others did not, so everything
    // below it sat at a different height in every card and six cards became six
    // documents. Rows cannot do that: the columns are the same or the table is
    // malformed.
    const busy = teamView({ id: "financas", name: "Finanças", max_live_runs: 1 });
    const idle = teamView({ id: "vendas", name: "Vendas", max_live_runs: 2 });
    const live = teamRun({ id: "run-1", team_id: "financas", state: "working" });
    daemon.apiFetch.mockImplementation(
      teamsFetch({
        teams: [busy, idle],
        runs: [live],
        runViews: { "run-1": { ...live, items: [], cost_usd: 1.2 } },
      }),
    );

    await renderTeams();

    const table = await departments();
    expect(within(table).getAllByRole("columnheader")).toHaveLength(7);

    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(2);
    for (const row of rows) {
      // One row header plus six cells, on the busy department and the idle one
      // alike.
      expect(within(row).getAllByRole("rowheader")).toHaveLength(1);
      expect(within(row).getAllByRole("cell")).toHaveLength(6);
    }
  });

  it("lifts the running work out of the rows and into the strip above them", async () => {
    const team = teamView({ id: "financas", name: "Finanças" });
    const live = teamRun({
      id: "run-1",
      team_id: "financas",
      state: "working",
      request: "reconcile October",
    });
    daemon.apiFetch.mockImplementation(
      teamsFetch({
        teams: [team],
        runs: [live],
        runViews: { "run-1": { ...live, items: [], cost_usd: 1.2 } },
      }),
    );

    await renderTeams();

    const strip = await screen.findByRole("region", { name: "In flight" });
    expect(within(strip).getByText("reconcile October")).toBeDefined();

    // And nowhere in the table, which is what keeps the rows level.
    const table = await departments();
    expect(within(table).queryByText("reconcile October")).toBeNull();
  });

  it("marks a reading that has reached its ceiling", async () => {
    // `1 / 1` and `0 / 1` are one glyph apart and are not the same news.
    const team = teamView({ id: "financas", name: "Finanças", max_live_runs: 1 });
    const live = teamRun({ id: "run-1", team_id: "financas", state: "working" });
    daemon.apiFetch.mockImplementation(
      teamsFetch({
        teams: [team],
        runs: [live],
        runViews: { "run-1": { ...live, items: [], cost_usd: 0 } },
      }),
    );

    await renderTeams();

    const table = await departments();
    const full = table.querySelector(".teams-figure-full");
    expect(full).not.toBeNull();
    expect(full?.textContent).toContain("1 / 1");
    // Not colour alone: the sentence is there for anything that does not render.
    expect(within(table).getByText(/at the ceiling/)).toBeDefined();
  });

  /**
   * A pulse with no key is a shape whose unit a reader has to guess, and the guess is
   * free to be wrong: these are days, and a run stacked on a day, and nothing on the
   * page said either. The header now carries the unit and each bar carries its own day —
   * there is no axis to read it off, because there is no fixed span to draw one for.
   */
  it("says what the pulse counts, in the header and on every bar", async () => {
    const team = teamView({ id: "financas", name: "Finanças" });
    daemon.apiFetch.mockImplementation(
      teamsFetch({
        teams: [team],
        runs: [
          teamRun({ id: "run-1", state: "done", created_at: "2026-09-05T08:00:00Z" }),
          teamRun({ id: "run-2", state: "done", created_at: "2026-09-05T17:00:00Z" }),
          teamRun({ id: "run-3", state: "done", created_at: "2026-09-06T09:00:00Z" }),
        ],
      }),
    );

    await renderTeams();

    const table = await departments();
    const pulse = within(table).getByRole("columnheader", { name: /Pulse/ });
    expect(pulse.querySelector(".teams-col-key")?.textContent).toBe("per day");

    // Oldest first, plural where the count earns it, and the day spelled out.
    const said = [...table.querySelectorAll("svg title")].map((node) => node.textContent);
    expect(said).toEqual(["2 runs on 2026-09-05", "1 run on 2026-09-06"]);
  });

  it("nobody yet is a mark, not a fault", async () => {
    const empty = teamView({
      id: "operacoes",
      name: "Operações",
      director_agent_id: "",
      members: [],
    });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [empty] }));

    await renderTeams();

    const table = await departments();
    expect(table.querySelector(".teams-figure-none")?.textContent).toContain("0");
    expect(within(table).getByText(/nobody yet, so no task can start/)).toBeDefined();
  });

  it("says what a team does on its own in words, not by colour alone", async () => {
    const team = teamView({
      id: "financas",
      name: "Finanças",
      grants: [
        { kind: "send_email", mode: "allow" },
        { kind: "file_document", mode: "propose" },
      ],
    });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [team] }));

    await renderTeams();

    const table = await departments();
    expect(within(table).getByText("send_email: does it")).toBeDefined();
    expect(within(table).getByText("file_document: asks first")).toBeDefined();
    // No grant row at all IS the denial — there is no `deny` mode in the núcleo.
    expect(within(table).getByText("calendar_event: asks you")).toBeDefined();
  });

  it("the glyph column says what its glyphs mean", async () => {
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [teamView()] }));

    await renderTeams();

    const header = within(await departments()).getByRole("columnheader", { name: /On its own/ });
    expect(header.textContent).toContain("does it");
    expect(header.textContent).toContain("asks first");
    expect(header.textContent).toContain("asks you");
    expect(header.textContent).toContain("routines armed");
  });

  it("reads a grant mode this shell does not know as its own gap, not as a decision", async () => {
    // `TeamGrant.mode` is a bare string on the wire. Folding an unknown one into
    // "asks you" would report a decision the daemon never made.
    const team = teamView({
      id: "financas",
      name: "Finanças",
      grants: [{ kind: "send_email", mode: "whenever-it-likes" }],
    });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [team] }));

    await renderTeams();

    const table = await departments();
    expect(within(table).getByText(/no reading for that mode/)).toBeDefined();
    expect(within(table).queryByText("send_email: asks you")).toBeNull();
  });

  it("counts the armed routines and never claims which fires first", async () => {
    const team = teamView({ id: "financas", name: "Finanças" });
    const armed = teamTrigger({
      id: 3,
      team_id: "financas",
      name: "monthly reconciliation",
      enabled: 1,
    });
    const alsoArmed = teamTrigger({ id: 4, team_id: "financas", name: "weekly sweep", enabled: 1 });
    daemon.apiFetch.mockImplementation(
      teamsFetch({ teams: [team], triggers: [armed, alsoArmed] }),
    );

    await renderTeams();

    const table = await departments();
    expect(within(table).getByText("2 armed routines")).toBeDefined();
    // The soonest across a department would be one query per rule per
    // department; the bench answers it per rule, where there is room.
    expect(within(table).queryByText(/next/i)).toBeNull();
  });
});

/* ----------------------------------------------------------- in flight -- */

describe("Teams - in flight", () => {
  it("shows what a task has spent against its department's per-task ceiling", async () => {
    const team = teamView({ id: "financas", name: "Finanças", budget_usd: 5, max_live_runs: 1 });
    const live = teamRun({
      id: "run-1",
      team_id: "financas",
      state: "working",
      request: "reconcile October",
    });
    daemon.apiFetch.mockImplementation(
      teamsFetch({
        teams: [team],
        runs: [live],
        runViews: { "run-1": { ...live, items: [], cost_usd: 1.2 } },
      }),
    );

    await renderTeams();

    const strip = await screen.findByRole("region", { name: "In flight" });
    // Which department is running it — the card said so by containing it, and
    // nothing contains it now.
    expect(within(strip).getByRole("link", { name: "Finanças" })).toBeDefined();
    // Money written as money — the one place in this pillar where a spend and
    // the ceiling it runs against both exist.
    expect(
      await within(strip).findByRole("img", { name: "spent on this task: $1.20 of $5.00" }),
    ).toBeDefined();
  });

  it("never writes an absent budget ceiling as zero", async () => {
    const team = teamView({ id: "financas", name: "Finanças", budget_usd: null });
    const live = teamRun({ id: "run-1", team_id: "financas", state: "working" });
    daemon.apiFetch.mockImplementation(
      teamsFetch({
        teams: [team],
        runs: [live],
        runViews: { "run-1": { ...live, items: [], cost_usd: 1.2 } },
      }),
    );

    await renderTeams();

    const strip = await screen.findByRole("region", { name: "In flight" });
    expect(await within(strip).findByText("no ceiling")).toBeDefined();
    expect(within(strip).queryByText(/\$0\.00/)).toBeNull();
  });

  it("draws nothing at all when nothing is running", async () => {
    // The headline already says "none at work"; an empty strip would be a second
    // way of saying it and a permanent hole in the page.
    const team = teamView({ id: "financas", name: "Finanças" });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [team], runs: [] }));

    await renderTeams();

    await departments();
    expect(screen.queryByRole("region", { name: "In flight" })).toBeNull();
  });
});

/* ------------------------------------------------------------ the header -- */

describe("Teams - the headline", () => {
  it("counts teams and specialists, and never a spend the daemon does not report", async () => {
    const financas = teamView({ id: "financas", name: "Finanças", director_agent_id: "controller", members: ["controller", "auditor"] });
    const marketing = teamView({ id: "marketing", name: "Marketing", director_agent_id: "writer", members: ["writer"] });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [financas, marketing] }));

    await renderTeams();

    const headline = await screen.findByText(/2 teams · 3 specialists · none at work/);
    // There is no per-department spend anywhere in the núcleo, so the console
    // aggregates none — the only money on this page is a per-task ceiling on a
    // card, and even that is a rule rather than a total. See the module header.
    expect(headline.textContent).not.toMatch(/\$/);
    expect(screen.queryByText(/spent/i)).toBeNull();
  });

  /**
   * The headline and the badge name team actions.
   *
   * Both said `waiting on you`, which is the one queue's phrase for the six decision lists at
   * `/waiting`. What this page has is open team actions, so the sentence says so and the badge
   * — already inside a column headed `Waiting`, on a row that names the team — says only the
   * one word it still needs to add.
   */
  it("the headline and the badge name team actions", async () => {
    const financas = teamView({
      id: "financas",
      name: "Finan\u00e7as",
      director_agent_id: "controller",
      members: ["controller", "auditor"],
    });
    // Finished, so the row is not `at work`: working beats waiting, and this is about waiting.
    const finished = teamRun({ id: "run-1", team_id: "financas", state: "done" });
    daemon.apiFetch.mockImplementation(
      teamsFetch({
        teams: [financas],
        runs: [finished],
        actions: [teamAction({ id: 1 }), teamAction({ id: 2 })],
      }),
    );

    await renderTeams();

    const headline = await screen.findByText(/team actions? waiting/);
    expect(headline.textContent).toContain("2 team actions waiting");
    expect(headline.textContent).not.toContain("waiting on you");

    const table = await departments();
    const row = within(table).getAllByRole("row")[1];
    expect(within(row).getByText("waiting").textContent).toBe("waiting");
    expect(within(row).queryByText("waiting on you")).toBeNull();
  });

  it("keeps the create form closed until it is asked for", async () => {
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [teamView()] }));

    await renderTeams();

    // The old page opened an eleven-field editor above a list nobody had read.
    expect(await screen.findByRole("button", { name: "New team" })).toBeDefined();
    expect(screen.queryByRole("heading", { level: 2, name: "New team" })).toBeNull();
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

describe("Teams - map-authored readings", () => {
  it("a department's state is one word from the map", async () => {
    const team = teamView({ id: "financas", name: "FinanÃ§as" });
    const done = teamRun({ id: "run-1", team_id: "financas", state: "done" });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [team], runs: [done], actions: [teamAction()] }));
    await renderTeams();
    const row = within(await departments()).getAllByRole("row")[1];
    const badge = within(row).getByText("waiting");
    expect(badge.textContent).toBe("waiting");
    expect(badge.className).toContain("ui-badge-pending");
    expect(within(row).queryByText("waiting on you")).toBeNull();
  });

  it("the spend meter does not ask anything of you", async () => {
    const team = teamView({ budget_usd: 5, max_live_runs: 1 });
    const live = teamRun({ id: "run-1", state: "working" });
    daemon.apiFetch.mockImplementation(teamsFetch({ teams: [team], runs: [live], runViews: { "run-1": { ...live, items: [], cost_usd: 1.2 } } }));
    await renderTeams();
    const meter = await screen.findByRole("img", { name: "spent on this task: $1.20 of $5.00" });
    const gauge = meter.closest(".ui-gauge");
    expect(gauge?.className).toContain("ui-gauge-active");
    expect(gauge?.className).not.toContain("ui-gauge-pending");
  });
});
