import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, within } from "@testing-library/react";
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

import { Roster } from "./Roster";
import { createAppQueryClient } from "../app/queryClient";
import type { Agent } from "../data/agents";
import type { TeamItem, TeamRun, TeamRunView, TeamView } from "../data/teams";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
});

/* The fixtures are `roster-graph.test.ts`', repeated rather than shared: a component test that
   imported another suite's fixtures would start failing for reasons that live in that suite. */

function team(overrides: Partial<TeamView> = {}): TeamView {
  return {
    id: "financas",
    name: "Finanças",
    mission: "keep the books straight",
    director_agent_id: "controller",
    max_rounds: 4,
    max_parallel: 2,
    budget_usd: 5,
    max_open_actions: 5,
    max_live_runs: 1,
    created_at: "2026-08-01T09:00:00Z",
    updated_at: "2026-09-01T09:00:00Z",
    members: ["controller", "auditor", "researcher"],
    grants: [],
    ...overrides,
  };
}

function agent(id: string, name: string, overrides: Partial<Agent> = {}): Agent {
  return {
    id,
    name,
    speciality: "does the thing",
    prompt: "You do the thing.",
    engine: "claude",
    model: null,
    tool_policy: "mcp_only",
    created_at: "2026-08-01T09:00:00Z",
    updated_at: "2026-08-01T09:00:00Z",
    ...overrides,
  };
}

const CATALOGUE = [
  agent("controller", "controller", { speciality: "runs the books" }),
  agent("auditor", "Auditor Sénior", { speciality: "checks the books" }),
  agent("researcher", "researcher", { speciality: "goes and finds it" }),
];

function item(overrides: Partial<TeamItem> = {}): TeamItem {
  return {
    ordinal: 1,
    round: 1,
    agent_id: "auditor",
    description: "pull the bank export",
    state: "done",
    run_id: 11,
    output_path: null,
    ...overrides,
  };
}

function run(overrides: Partial<TeamRunView> = {}): TeamRunView {
  return {
    id: "run-live-1",
    team_id: "financas",
    request: "Chase the invoices",
    workspace: "teams/financas/run-live-1",
    state: "working",
    director_node: "replanning",
    director_run_id: 10,
    round: 2,
    next_ordinal: 5,
    dry_rounds: 0,
    plan_retries: 0,
    replanned: "",
    outcome: null,
    why: null,
    created_at: "2026-09-04T09:00:00Z",
    updated_at: "2026-09-04T09:20:00Z",
    finished_at: null,
    trigger_id: null,
    parent_id: null,
    root_id: "run-live-1",
    depth: 0,
    cost_usd: 1.24,
    items: [
      item(),
      item({
        ordinal: 3,
        round: 2,
        agent_id: "controller",
        description: "match them line by line",
        state: "working",
        run_id: 13,
      }),
    ],
    ...overrides,
  };
}

/** The list row the bench passes in — `TeamRun`, without the items and without the cost. */
function listRow(overrides: Partial<TeamRun> = {}): TeamRun {
  const { items: _items, cost_usd: _cost, ...row } = run();
  return { ...row, ...overrides };
}

/**
 * The tab over a fake daemon whose live run can be swapped mid-test, which is what the last test
 * needs: the layout must survive a state changing under it without moving a box.
 */
async function renderRoster(view: TeamView, runs: TeamRun[], seed: TeamRunView = run()) {
  const state = { detail: seed };

  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (path === "/agents") return CATALOGUE;
    if (path === "/teams") return [view];
    if (path.startsWith("/team-runs/")) return state.detail;
    return undefined;
  });

  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({
      getParentRoute: () => rootRoute,
      path: "/team-runs/$runId",
      component: () => null,
    }),
    createRoute({
      getParentRoute: () => rootRoute,
      path: "/teams/$teamId",
      component: () => <Roster team={view} runs={runs} />,
    }),
  ];
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: ["/teams/financas"] }),
    defaultPreload: false,
  });

  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { ...result, queryClient, state };
}

describe("Roster", () => {
  /**
   * The structure is the subject. These two are what the first version of the tab could not do:
   * an idle department drew three boxes and "nothing in flight", and said nothing about how it is
   * put together or what it runs under.
   */
  it("draws the department itself, with the limits it runs under", async () => {
    await renderRoster(team(), [listRow()]);

    expect(await screen.findByLabelText("Finanças — 2 on the roster")).toBeTruthy();
    expect(screen.getByText("4 rounds · 2 at a time")).toBeTruthy();
    expect(screen.getByText("$5.00 ceiling · 5 open actions")).toBeTruthy();
  });

  it("draws the whole structure when nothing at all is running", async () => {
    await renderRoster(team(), []);

    expect(await screen.findByLabelText("Finanças — 2 on the roster")).toBeTruthy();
    expect(screen.getByLabelText("controller — directs")).toBeTruthy();
    expect(screen.getByLabelText("Auditor Sénior — checks the books")).toBeTruthy();
    expect(screen.getByText("nothing in flight")).toBeTruthy();
  });

  it("says nobody is in charge rather than drawing a nameless box", async () => {
    await renderRoster(team({ director_agent_id: "", members: ["auditor"] }), []);

    expect(await screen.findByText(/nobody is in charge of this department yet/)).toBeTruthy();
    expect(screen.getByLabelText("Auditor Sénior — checks the books")).toBeTruthy();
  });

  it("draws the director, the roster and the work in flight", async () => {
    await renderRoster(team(), [listRow()]);

    expect(await screen.findByLabelText("controller — directs")).toBeTruthy();
    expect(await screen.findByLabelText("Auditor Sénior — checks the books")).toBeTruthy();
    expect(await screen.findByLabelText("pull the bank export — done")).toBeTruthy();
  });

  it("links each piece of work to its task", async () => {
    await renderRoster(team(), [listRow()]);

    const work = await screen.findByLabelText("pull the bank export — done");
    expect(work.querySelector("a")?.getAttribute("href")).toBe("/team-runs/run-live-1");
  });

  it("says the state in words beside the glyph, never in colour alone", async () => {
    await renderRoster(team(), [listRow()]);

    const work = await screen.findByLabelText("pull the bank export — done");
    expect(within(work).getByText("done")).toBeTruthy();
  });

  it("says nothing is in flight when no run is alive", async () => {
    await renderRoster(team(), []);

    expect(await screen.findByText("nothing in flight")).toBeTruthy();
    expect(screen.queryByLabelText(/pull the bank export/)).toBeNull();
    expect(daemon.apiFetch).not.toHaveBeenCalledWith(expect.stringContaining("/team-runs/"));
  });

  it("marks a member whose agent was deleted", async () => {
    await renderRoster(team({ members: ["controller", "ghost"] }), []);

    expect(await screen.findByLabelText("ghost — deleted from the catalogue")).toBeTruthy();
  });

  it("routes the director's own edge around the roster rather than through it", async () => {
    const { container } = await renderRoster(team(), [listRow()]);
    await screen.findByLabelText("match them line by line — working");

    const crossing = container.querySelector(".teams-org-edge-far");
    // Orthogonal, not a curve. A bezier from rank 0 to rank 2 passes under a specialist's box and
    // reappears at its edge, which reads as that specialist holding the director's own work.
    expect(crossing?.getAttribute("d")).toMatch(/^M [\d.]+ [\d.]+ V [\d.]+ H [\d.]+ V [\d.]+$/);
  });

  it("does not move a box when only a state changes", async () => {
    const { queryClient, state } = await renderRoster(team(), [listRow()]);
    const before = (await screen.findByLabelText("pull the bank export — done")).getAttribute(
      "transform",
    );
    expect(before).not.toBeNull();

    state.detail = run({ items: [item({ state: "failed" })] });
    await act(async () => {
      await queryClient.invalidateQueries();
    });

    const after = await screen.findByLabelText("pull the bank export — failed");
    expect(after.getAttribute("transform")).toBe(before);
  });
});
