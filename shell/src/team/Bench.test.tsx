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

import { Bench } from "./Bench";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import type { TeamAction, TeamRun, TeamRunView, TeamTrigger, TeamView, TriggerNext } from "../data/teams";

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
    members: ["controller", "auditor"],
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
  team?: TeamView | null;
  runs?: TeamRun[];
  triggers?: TeamTrigger[];
  actions?: TeamAction[];
  runViews?: Record<string, TeamRunView>;
  next?: Record<number, TriggerNext>;
}

function benchFetch(fake: Fake): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path) => {
    if (path === "/agents") return [];
    if (path === "/teams") return fake.team === undefined || fake.team === null ? [] : [fake.team];
    if (path === "/team-runs") return fake.runs ?? [];
    if (path === "/team-triggers") return fake.triggers ?? [];
    if (path === "/team-actions") return fake.actions ?? [];

    const nextMatch = /^\/team-triggers\/(\d+)\/next$/.exec(path);
    if (nextMatch !== null) return fake.next?.[Number(nextMatch[1])] ?? { next: null, error: null };

    const runMatch = /^\/team-runs\/([^/]+)$/.exec(path);
    if (runMatch !== null) return fake.runViews?.[runMatch[1]];

    const teamMatch = /^\/teams\/([^/]+)$/.exec(path);
    if (teamMatch !== null) {
      if (fake.team === undefined || fake.team === null) {
        throw new ApiRefusal(404, "not_found", "no such team");
      }
      return fake.team;
    }

    return undefined;
  };
}

/** The bench inside a two-route router, so `useParams` has a `$teamId` to read. */
async function renderBench(fake: Fake, teamId = "financas") {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({ getParentRoute: () => rootRoute, path: "/teams", component: () => null }),
    createRoute({ getParentRoute: () => rootRoute, path: "/teams/$teamId", component: Bench }),
    createRoute({ getParentRoute: () => rootRoute, path: "/team-runs/$runId", component: () => null }),
  ];
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: [`/teams/${teamId}`] }),
    defaultPreload: false,
  });

  daemon.apiFetch.mockImplementation(benchFetch(fake));

  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { ...result, router, queryClient };
}

/**
 * Move to a tab by its label, and wait for its panel to be the one on screen.
 *
 * `mouseDown`, not `click`. Radix selects a tab on pointer-down so that a drag
 * that starts on a tab still switches to it, and `fireEvent.click` dispatches
 * only a `click` — a test that clicked would stay on the tab it started on and
 * then fail looking for content that was never mounted.
 */
async function openTab(name: RegExp | string) {
  fireEvent.mouseDown(await screen.findByRole("tab", { name }));
  const tab = await screen.findByRole("tab", { name, selected: true });
  const id = tab.getAttribute("aria-controls");
  return await waitFor(() => {
    const panel = document.getElementById(id ?? "");
    if (panel === null) throw new Error(`no panel for tab ${String(name)}`);
    return panel;
  });
}

/* ------------------------------------------------------------- the shell -- */

describe("Bench - the shell", () => {
  it("puts one tab on each API surface, and marks the one that is waiting", async () => {
    const run = teamRun({ id: "run-1", team_id: "financas" });
    const action: TeamAction = {
      id: 1,
      team_run_id: "run-1",
      ordinal: null,
      kind: "send_email",
      payload: "{}",
      why: "the supplier asked twice",
      proposal_id: 9,
      state: "pending",
      error: null,
      created_at: "2026-08-24T09:10:00Z",
      executed_at: null,
    };
    await renderBench({ team: teamView(), runs: [run], actions: [action] });

    expect(await screen.findByRole("heading", { level: 1, name: "Finanças" })).toBeDefined();
    for (const name of ["Work", "Routines", "Charter"]) {
      expect(screen.getByRole("tab", { name })).toBeDefined();
    }
    // The one tab that can be behind is the one that carries a count.
    expect(screen.getByRole("tab", { name: /Decisions/ }).textContent).toContain("1");
  });

  it("mounts every tab on its own surface, with nothing left as a placeholder", async () => {
    await renderBench({ team: teamView() });

    // Decisions owns the two decision routes.
    const decisions = await openTab(/Decisions/);
    expect(within(decisions).getByRole("heading", { level: 2, name: "Actions it wants to take" })).toBeDefined();
    expect(within(decisions).getByRole("heading", { level: 2, name: "Specialists it asked for" })).toBeDefined();

    // Charter owns `PUT /teams/{id}`, which is why all eleven fields are here
    // and under one Save — sending half of a full replace wipes the other half.
    const charter = await openTab("Charter");
    expect(within(charter).getByLabelText("Mission")).toBeDefined();
    expect(within(charter).getByLabelText("Members")).toBeDefined();
    expect(within(charter).getByLabelText("send_email grant")).toBeDefined();

    // And no tab is standing in for one that was never written.
    expect(screen.queryByText(/Not written yet/)).toBeNull();
  });

  /**
   * One count treatment across the app. This tab drew its backlog as a filled
   * `Badge`, which is the shape `ui.css` reserves for a STATE — so the bench had a
   * pending pill beside "at work"/"idle" pills, three ranks of meaning in one row of
   * identical capsules. The project tab strip already said how many with a `Count`.
   */
  it("the Decisions tab wears a count and not a pill", async () => {
    const run = teamRun({ id: "run-1", team_id: "financas" });
    const action: TeamAction = {
      id: 1,
      team_run_id: "run-1",
      ordinal: null,
      kind: "send_email",
      payload: "{}",
      why: "the supplier asked twice",
      proposal_id: 9,
      state: "pending",
      error: null,
      created_at: "2026-08-24T09:10:00Z",
      executed_at: null,
    };
    await renderBench({ team: teamView(), runs: [run], actions: [action] });

    const decisions = await screen.findByRole("tab", { name: /Decisions/ });
    const count = decisions.querySelector(".ui-count");
    expect(count?.textContent).toBe("1");
    expect(decisions.querySelector(".ui-badge")).toBeNull();
  });

  it("names the team the núcleo says it has never heard of", async () => {
    await renderBench({ team: null }, "nao-existe");

    expect(await screen.findByText("there is no team with that id")).toBeDefined();
  });
});

/* ------------------------------------------------------------- routines -- */

describe("Bench - Routines", () => {
  it("offers Duplicate and no editing control at all", async () => {
    // The núcleo mounts POST, DELETE and /enable on /team-triggers and nothing
    // else — an Edit button would answer 405. Duplicate is what stands in.
    await renderBench({ team: teamView(), triggers: [teamTrigger({ id: 1, name: "monthly reconciliation" })] });

    const panel = await openTab("Routines");

    expect(within(panel).getByRole("button", { name: "Duplicate" })).toBeDefined();
    expect(within(panel).queryByRole("button", { name: /^Edit/i })).toBeNull();
    expect(within(panel).queryByRole("button", { name: /Save changes/i })).toBeNull();
    expect(within(panel).getByText(/cannot be edited/)).toBeDefined();
  });

  it("fills the write-a-rule form from the rule that was duplicated", async () => {
    await renderBench({
      team: teamView(),
      triggers: [teamTrigger({ id: 1, name: "monthly reconciliation", cron: "0 7 1 * *", request: "reconcile last month" })],
    });

    const panel = await openTab("Routines");
    fireEvent.click(within(panel).getByRole("button", { name: "Duplicate" }));

    // Marked as a copy, so two rules are never confused for one another.
    expect((within(panel).getByLabelText("Name") as HTMLInputElement).value).toBe("monthly reconciliation (copy)");
    expect((within(panel).getByLabelText("Cron") as HTMLInputElement).value).toBe("0 7 1 * *");
    expect((within(panel).getByLabelText("Request") as HTMLTextAreaElement).value).toBe("reconcile last month");
  });

  it("asks before arming a rule for a team with no ceiling, and never before disarming", async () => {
    await renderBench({
      team: teamView({ budget_usd: null }),
      triggers: [
        teamTrigger({ id: 1, name: "disarmed rule", enabled: 0 }),
        teamTrigger({ id: 2, name: "armed rule", enabled: 1 }),
      ],
    });

    const panel = await openTab("Routines");

    // Arming a no-ceiling department's rule is the one interlock on this tab,
    // and the dwell has to pass for real before the confirm click is honoured.
    fireEvent.click(within(panel).getByRole("button", { name: "Arm with no ceiling" }));
    const confirm = await within(panel).findByRole("button", { name: "Arm it anyway" });
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(confirm);
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/team-triggers/1/enable", {
        method: "POST",
        body: JSON.stringify({ enabled: true }),
      });
    });

    // Disarming is plain and fires on one click, with no interlock at all.
    fireEvent.click(within(panel).getByRole("button", { name: "Disarm" }));
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/team-triggers/2/enable", {
        method: "POST",
        body: JSON.stringify({ enabled: false }),
      });
    });
  });

  it("renders a rule that does not fire on a clock as a sentence rather than an error", async () => {
    await renderBench({
      team: teamView(),
      triggers: [teamTrigger({ id: 7, name: "on demand", source: "team_finished", cron: null, timezone: null })],
      next: { 7: { next: null, error: "this rule does not fire on a clock" } },
    });

    const panel = await openTab("Routines");

    expect(await within(panel).findByText("this rule does not fire on a clock")).toBeDefined();
    expect(panel.querySelector(".ui-badge-danger")).toBeNull();
  });
});

/* ----------------------------------------------------------------- work -- */

describe("Bench - Work", () => {
  it("shows the rounds and the cost of a live task, and says what the list is capped at", async () => {
    const live = teamRun({ id: "run-1", state: "working", round: 2, request: "reconcile October" });
    await renderBench({
      team: teamView(),
      runs: [live],
      runViews: {
        "run-1": {
          ...live,
          cost_usd: 1.2,
          items: [
            { ordinal: 1, round: 1, agent_id: "auditor", description: "pull the ledger", state: "done", run_id: 11, output_path: null },
            { ordinal: 2, round: 2, agent_id: "controller", description: "check it", state: "working", run_id: 12, output_path: null },
          ],
        },
      },
    });

    const panel = await screen.findByRole("tabpanel");

    expect(await within(panel).findByText("round 1")).toBeDefined();
    expect(within(panel).getByText("auditor")).toBeDefined();
    // The round after the current one is a fact about how the director plans,
    // not a gap in the answer.
    expect(within(panel).getByText("not planned yet")).toBeDefined();
    expect(within(panel).getByRole("img", { name: "spent on this task: $1.20 of $10.00" })).toBeDefined();

    // The honest footer: this department's tasks out of the newest hundred
    // runs across every department, with no paging past it.
    expect(within(panel).getByText(/newest 100 runs across all teams/)).toBeDefined();
  });

  it("the rounds strip says what its marks mean", async () => {
    const live = teamRun({ id: "run-1", state: "working" });
    await renderBench({
      team: teamView(),
      runs: [live],
      runViews: {
        "run-1": {
          ...live,
          cost_usd: 0,
          items: [{ ordinal: 1, round: 1, agent_id: "auditor", description: "pull the ledger", state: "done", run_id: 11, output_path: null }],
        },
      },
    });

    const panel = await openTab("Work");
    const key = panel.querySelector(".teams-rounds-key");
    expect(key?.textContent).toContain("✓ done");
    expect(key?.textContent).toContain("⋯ running");
    expect(key?.textContent).toContain("· not started");
    expect(key?.textContent).toContain("✗ failed");
    expect(within(panel).getByRole("list", { name: "Rounds" })).toBeDefined();
  });

  it("keeps the composer to one line until it is being used", async () => {
    await renderBench({ team: teamView() });

    const panel = await screen.findByRole("tabpanel");
    const box = within(panel).getByLabelText("Ask this team for something") as HTMLTextAreaElement;

    // It replaced a three-row textarea that stood permanently open.
    expect(box.rows).toBe(1);
    fireEvent.focus(box);
    expect((within(panel).getByLabelText("Ask this team for something") as HTMLTextAreaElement).rows).toBe(3);
  });

  it("names the specialist the núcleo says is missing when a task will not start", async () => {
    await renderBench({ team: teamView() });

    const panel = await screen.findByRole("tabpanel");
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/teams/financas/runs" && init?.method === "POST") {
        throw new ApiRefusal(400, "bad_request", "`pesquisa` is on this team and not in the catalogue");
      }
      return benchFetch({ team: teamView() })(path, init);
    });

    fireEvent.change(within(panel).getByLabelText("Ask this team for something"), {
      target: { value: "find leads" },
    });
    fireEvent.click(within(panel).getByRole("button", { name: "Start" }));

    // The daemon's own sentence, verbatim — no toast and no modal anywhere.
    expect(await within(panel).findByText(/pesquisa.*not in the catalogue/)).toBeDefined();
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
