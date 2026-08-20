import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
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

import { TeamRunDetail } from "./TeamRunDetail";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import { POLL } from "../data/poll";
import type { TeamAction, TeamItem, TeamRunView } from "../data/teams";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/* ------------------------------------------------------------ fixtures -- */

function teamRunView(overrides: Partial<TeamRunView> = {}): TeamRunView {
  return {
    id: "run-1",
    team_id: "atendimento",
    request: "clear the backlog",
    workspace: "teams/atendimento/run-1",
    state: "working",
    director_node: "planning",
    director_run_id: null,
    round: 1,
    next_ordinal: 1,
    dry_rounds: 0,
    plan_retries: 0,
    replanned: "no",
    outcome: null,
    why: null,
    created_at: "2026-08-18T09:00:00Z",
    updated_at: "2026-08-18T09:00:00Z",
    finished_at: null,
    trigger_id: null,
    parent_id: null,
    root_id: "run-1",
    depth: 0,
    items: [],
    cost_usd: 0,
    ...overrides,
  };
}

function teamItem(overrides: Partial<TeamItem> = {}): TeamItem {
  return {
    ordinal: 1,
    round: 1,
    agent_id: "ana",
    description: "read the mailbox",
    state: "done",
    run_id: null,
    output_path: null,
    ...overrides,
  };
}

function teamAction(overrides: Partial<TeamAction> = {}): TeamAction {
  return {
    id: 1,
    team_run_id: "run-1",
    ordinal: null,
    kind: "send_email",
    payload: JSON.stringify({ to: "cliente@example.com" }),
    why: "answer the open ticket",
    proposal_id: null,
    state: "pending",
    error: null,
    created_at: "2026-08-18T09:00:00Z",
    executed_at: null,
    ...overrides,
  };
}

/** The team-run routes, over mutable state — the `Council.test.tsx` shape. */
function teamRunFetch(
  views: Record<string, TeamRunView>,
  actions: Record<string, TeamAction[]> = {},
  opts: { onCancel?: (id: string) => unknown } = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    const cancelMatch = /^\/team-runs\/([^/]+)\/cancel$/.exec(path);
    if (cancelMatch !== null && init?.method === "POST") {
      if (opts.onCancel !== undefined) return opts.onCancel(cancelMatch[1]);
      return undefined;
    }
    const actionsMatch = /^\/team-runs\/([^/]+)\/actions$/.exec(path);
    if (actionsMatch !== null) return actions[actionsMatch[1]] ?? [];
    if (init?.method === "DELETE") return undefined;
    const detailMatch = /^\/team-runs\/([^/]+)$/.exec(path);
    if (detailMatch !== null) {
      const view = views[detailMatch[1]];
      if (view === undefined) throw new ApiRefusal(404, "not_found", "no such run");
      return view;
    }
    return undefined;
  };
}

/**
 * A local two-route router only — `/team-runs/$runId` plus `/teams` as the
 * navigation target. No `renderApp` anywhere in this file: it mounts the
 * gate, the rail and its own live queries around every assertion, and this
 * machine cannot pay for that more than once per file.
 */
async function renderTeamRunDetail(initialPath: string) {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({ getParentRoute: () => rootRoute, path: "/team-runs/$runId", component: TeamRunDetail }),
    createRoute({ getParentRoute: () => rootRoute, path: "/teams", component: () => <p>Teams index</p> }),
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

/* ------------------------------------------------------------------ rounds -- */

describe("TeamRunDetail - rounds and actions", () => {
  it("draws each round with its items and the actions the department asked for", async () => {
    const run = teamRunView({
      id: "run-1",
      items: [
        teamItem({ ordinal: 2, round: 1, agent_id: "bruno", description: "second in round one" }),
        teamItem({ ordinal: 1, round: 1, agent_id: "ana", description: "first in round one" }),
        teamItem({ ordinal: 1, round: 2, agent_id: "carla", description: "first in round two" }),
      ],
    });
    const action = teamAction({ id: 9, team_run_id: "run-1", kind: "send_email" });
    daemon.apiFetch.mockImplementation(teamRunFetch({ "run-1": run }, { "run-1": [action] }));

    await renderTeamRunDetail("/team-runs/run-1");

    const round1 = await screen.findByRole("list", { name: "Round 1" });
    const round1Items = within(round1).getAllByRole("listitem");
    expect(round1Items.map((item) => item.textContent?.includes("first in round one"))).toEqual([true, false]);

    const round2 = screen.getByRole("list", { name: "Round 2" });
    expect(within(round2).getByText("first in round two")).toBeDefined();

    // Actions are drawn below the items.
    const roundsHeading = screen.getByRole("heading", { level: 2, name: "Rounds" });
    const actionsHeading = await screen.findByRole("heading", { level: 2, name: "Actions" });
    expect(roundsHeading.compareDocumentPosition(actionsHeading) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(within(screen.getByRole("list", { name: "Actions" })).getByText("send_email")).toBeDefined();
  });
});

/* --------------------------------------------------------------- stopped -- */

describe("TeamRunDetail - a run stopped at a ceiling", () => {
  it("shows a stopped run with the núcleo's reason and never the look of a failure", async () => {
    const run = teamRunView({
      id: "run-1",
      state: "stopped",
      outcome: "stopped",
      why: "the team's ceiling of $5.00 is spent ($5.12)",
    });
    daemon.apiFetch.mockImplementation(teamRunFetch({ "run-1": run }));

    await renderTeamRunDetail("/team-runs/run-1");

    expect(await screen.findByText("the team's ceiling of $5.00 is spent ($5.12)")).toBeDefined();
    expect(document.querySelector(".ui-badge-danger")).toBeNull();
    expect(screen.queryByText(/fail/i)).toBeNull();
  });
});

/* --------------------------------------------------------------- actions -- */

describe("TeamRunDetail - a refused action vs a failed one", () => {
  it("separates an action the owner refused from one that failed", async () => {
    const run = teamRunView({ id: "run-1" });
    const refused = teamAction({ id: 1, kind: "send_email", state: "failed", error: "rejected" });
    const failed = teamAction({ id: 2, kind: "file_document", state: "failed", error: "smtp refused the message" });
    daemon.apiFetch.mockImplementation(teamRunFetch({ "run-1": run }, { "run-1": [refused, failed] }));

    await renderTeamRunDetail("/team-runs/run-1");

    const actionsList = await screen.findByRole("list", { name: "Actions" });
    // Direct children only: each action card nests its own `<ul>` of payload
    // fields, and `getAllByRole("listitem")` would recurse into those too.
    const cards = Array.from(actionsList.children) as HTMLElement[];
    expect(cards).toHaveLength(2);

    const refusedCard = cards.find((card) => card.textContent?.includes("send_email"));
    const failedCard = cards.find((card) => card.textContent?.includes("file_document"));
    if (refusedCard === undefined || failedCard === undefined) throw new Error("missing an action card");

    expect(within(refusedCard).getByText("refused by you")).toBeDefined();
    expect(within(refusedCard).queryByText("failed")).toBeNull();
    expect(within(failedCard).getByText("failed")).toBeDefined();
  });
});

/* ---------------------------------------------------------------- origin -- */

describe("TeamRunDetail - where a run came from", () => {
  it("names who started the run and links to the run it came from", async () => {
    const run = teamRunView({ id: "run-2", parent_id: "run-1", root_id: "run-1" });
    daemon.apiFetch.mockImplementation(teamRunFetch({ "run-2": run }));

    await renderTeamRunDetail("/team-runs/run-2");

    const link = await screen.findByRole("link", { name: "the run it came from" });
    expect(link.getAttribute("href")).toBe("/team-runs/run-1");
  });
});

/* ------------------------------------------------------------------- cost -- */

describe("TeamRunDetail - cost", () => {
  it("shows this run's own cost and does not invent the chain's", async () => {
    const run = teamRunView({ id: "run-2", root_id: "run-1", cost_usd: 1.25 });
    daemon.apiFetch.mockImplementation(teamRunFetch({ "run-2": run }));

    await renderTeamRunDetail("/team-runs/run-2");

    const costFigures = await screen.findAllByText((_, element) => element?.className === "teams-cost");
    expect(costFigures).toHaveLength(1);
    expect(costFigures[0].textContent).toContain("1.2500");
  });
});

/* --------------------------------------------------------------- cadence -- */

describe("TeamRunDetail - the run's cadence", () => {
  it("stops polling once the run has ended", async () => {
    const liveRun = teamRunView({ id: "run-1", state: "working" });
    daemon.apiFetch.mockImplementation(teamRunFetch({ "run-1": liveRun }));

    const { queryClient } = await renderTeamRunDetail("/team-runs/run-1");
    await screen.findByRole("heading", { level: 1, name: "Team run" });

    function computedInterval(): number | false {
      const query = queryClient.getQueryCache().find({ queryKey: keys.teams.run("run-1"), exact: true });
      if (query === undefined) throw new Error("no cached query for run-1");
      // `refetchInterval` lives on `QueryObserverOptions`, not on the narrower
      // `QueryOptions` type `Query.options` is typed as.
      const option = (
        query.options as {
          refetchInterval?: number | false | ((q: typeof query) => number | false | undefined);
        }
      ).refetchInterval;
      if (typeof option !== "function") throw new Error("refetchInterval is not a function here");
      return option(query) ?? false;
    }

    expect(computedInterval()).toBe(POLL.queue);

    const doneRun = teamRunView({ id: "run-1", state: "done", outcome: "done" });
    daemon.apiFetch.mockImplementation(teamRunFetch({ "run-1": doneRun }));
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.teams.run("run-1") });
    });
    expect(await screen.findByText("delivered")).toBeDefined();

    expect(computedInterval()).toBe(false);
  });
});

/* -------------------------------------------------------------------- 404 -- */

describe("TeamRunDetail - cancelling an already-ended run", () => {
  it("reads a cancel that answers 404 as a run that had already ended", async () => {
    const run = teamRunView({ id: "run-1", state: "working" });
    daemon.apiFetch.mockImplementation(
      teamRunFetch({ "run-1": run }, {}, {
        onCancel: () => {
          throw new ApiRefusal(404, "not_found", "team not found");
        },
      }),
    );

    await renderTeamRunDetail("/team-runs/run-1");

    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));

    expect(await screen.findByText(/already ended/i)).toBeDefined();
    expect(screen.queryByText("team not found")).toBeNull();
  });
});
