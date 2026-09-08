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

import { Decisions, recruitTeam } from "./Decisions";
import { createAppQueryClient } from "../app/queryClient";
import type { Proposal } from "../data/system";
import type { TeamAction, TeamRun, TeamView } from "../data/teams";

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
    max_rounds: 4,
    max_parallel: 2,
    budget_usd: 5,
    max_open_actions: 5,
    max_live_runs: 1,
    created_at: "2026-08-18T09:00:00Z",
    updated_at: "2026-08-18T09:00:00Z",
    members: ["controller"],
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
    round: 1,
    next_ordinal: 2,
    dry_rounds: 0,
    plan_retries: 0,
    replanned: "no",
    outcome: null,
    why: null,
    created_at: "2026-08-24T09:00:00Z",
    updated_at: "2026-08-24T09:00:00Z",
    finished_at: null,
    trigger_id: null,
    parent_id: null,
    root_id: "run-1",
    depth: 0,
    ...overrides,
  };
}

function teamAction(overrides: Partial<TeamAction> = {}): TeamAction {
  return {
    id: 1,
    team_run_id: "run-1",
    ordinal: null,
    kind: "send_email",
    payload: JSON.stringify({ to: "fornecedor@exemplo.pt", subject: "October invoices", body: "…" }),
    why: "the supplier has asked twice and the ledger agrees with them",
    proposal_id: 9,
    state: "pending",
    error: null,
    created_at: "2026-08-24T09:10:00Z",
    executed_at: null,
    ...overrides,
  };
}

/**
 * A recruitment as `create_agent_recruit` writes it: the six `AgentRequest`
 * fields, plus `team_id`, `team_run_id` and `slug` merged in
 * (`core/src/proposals.rs:464`).
 */
function recruit(teamId: string, overrides: Partial<Proposal> = {}): Proposal {
  return {
    id: 21,
    kind: "agent-recruit",
    status: "pending",
    run_id: null,
    session_id: null,
    project_id: null,
    errand_id: null,
    errand_name: null,
    tool_name: "tax-analyst",
    reasoning: "nobody here can read a VAT return",
    tool_input: JSON.stringify({
      name: "tax analyst",
      speciality: "reads VAT returns",
      prompt: "you read VAT returns",
      engine: "claude",
      model: null,
      tool_policy: "read_only",
      team_id: teamId,
      team_run_id: "run-1",
      slug: "tax-analyst",
    }),
    read_from: null,
    created_at: "2026-08-24T09:20:00Z",
    decided_at: null,
    ...overrides,
  } as Proposal;
}

interface Fake {
  actions?: TeamAction[];
  recruits?: Proposal[];
}

async function renderDecisions(team: TeamView, runs: TeamRun[], fake: Fake) {
  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (path === "/team-actions") return fake.actions ?? [];
    if (path === "/proposals/recruits") return fake.recruits ?? [];
    return undefined;
  });

  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({
      getParentRoute: () => rootRoute,
      path: "/teams/$teamId",
      component: () => <Decisions team={team} runs={runs} />,
    }),
  ];
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: [`/teams/${team.id}`] }),
    defaultPreload: false,
  });

  await router.load();
  return render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

/** The `<section>` a panel's own heading belongs to, so an assertion is scoped to one. */
async function panelFor(heading: string): Promise<HTMLElement> {
  const found = await screen.findByRole("heading", { level: 2, name: heading });
  const panel = found.closest("section");
  if (panel === null) throw new Error(`no panel for "${heading}"`);
  return panel;
}

/* ------------------------------------------------------------ recruits -- */

describe("Decisions - recruitment", () => {
  it("shows only the recruitments this team asked for, by team_id inside tool_input", async () => {
    // `GET /proposals/recruits` answers every pending recruitment across the
    // house; the department is inside the JSON payload and there is no query
    // parameter for it, so the filter is the shell's.
    const mine = recruit("financas", { id: 21, tool_name: "tax-analyst" });
    const theirs = recruit("marketing", { id: 22, tool_name: "copywriter" });

    await renderDecisions(teamView(), [], { recruits: [mine, theirs] });

    const panel = await panelFor("Specialists it asked for");
    expect(await within(panel).findByText("recruit #21")).toBeDefined();
    expect(within(panel).queryByText("recruit #22")).toBeNull();
    expect(within(panel).getByText("nobody here can read a VAT return")).toBeDefined();
  });

  it("says Hire rather than Approve, and sends whatever was edited", async () => {
    await renderDecisions(teamView(), [], { recruits: [recruit("financas")] });

    const panel = await panelFor("Specialists it asked for");

    // The one editable approval in the house — a director gets the engine
    // wrong more often than anything else, and it is what costs money.
    fireEvent.change(await within(panel).findByLabelText("engine"), { target: { value: "codex" } });

    // "Hire", not "Approve": approving an action means do that once; hiring
    // means keep this person, and it lasts forever.
    expect(within(panel).queryByRole("button", { name: /^Approve/ })).toBeNull();
    fireEvent.click(within(panel).getByRole("button", { name: "Hire #21" }));
    const confirm = await within(panel).findByRole("button", {
      name: "Write the specialist and add them to the roster",
    });
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(confirm);

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/proposals/21/approve",
        expect.objectContaining({ method: "POST" }),
      );
    });
    const call = (daemon.apiFetch.mock.calls as unknown[][]).find(
      (one) => one[0] === "/proposals/21/approve",
    ) as [string, RequestInit];
    const body = JSON.parse(call[1].body as string) as { hire: { engine: string; name: string } };
    expect(body.hire.engine).toBe("codex");
    expect(body.hire.name).toBe("tax analyst");
  });

  it("reads no team out of a payload that has none, rather than guessing one", () => {
    expect(recruitTeam(recruit("financas"))).toBe("financas");
    expect(recruitTeam(recruit("financas", { tool_input: null }))).toBeNull();
    expect(recruitTeam(recruit("financas", { tool_input: "not json" }))).toBeNull();
    expect(recruitTeam(recruit("financas", { tool_input: JSON.stringify({ name: "x" }) }))).toBeNull();
  });
});

/* ------------------------------------------------------------- actions -- */

describe("Decisions - actions", () => {
  it("shows only the actions belonging to this team's runs", async () => {
    // `GET /team-actions` carries `team_run_id` and no department, so the link
    // back is through the run list — and an action whose run has fallen off
    // the newest-hundred window is shown nowhere rather than shown wrongly.
    const mine = teamAction({ id: 1, team_run_id: "run-1" });
    const theirs = teamAction({ id: 2, team_run_id: "run-9" });
    const orphan = teamAction({ id: 3, team_run_id: "run-off-the-window" });

    await renderDecisions(
      teamView(),
      [teamRun({ id: "run-1", team_id: "financas" }), teamRun({ id: "run-9", team_id: "marketing" })],
      { actions: [mine, theirs, orphan] },
    );

    const panel = await panelFor("Actions it wants to take");
    const list = await within(panel).findByRole("list", { name: "Actions" });
    expect(within(list).getAllByText("send_email")).toHaveLength(1);
  });

  it("renders the payload by kind rather than as a blob, with the why as a quotation", async () => {
    await renderDecisions(teamView(), [teamRun({ id: "run-1" })], { actions: [teamAction()] });

    const panel = await panelFor("Actions it wants to take");
    // A JSON blob is not a decision anybody can make.
    expect(await within(panel).findByText("fornecedor@exemplo.pt")).toBeDefined();
    expect(within(panel).getByText("October invoices")).toBeDefined();
    expect(within(panel).getByText(/the supplier has asked twice/)).toBeDefined();
    expect(panel.querySelector(".teams-act-raw")).toBeNull();
  });

  it("offers no decision on an action nobody decides", async () => {
    // A null `proposal_id` means the grant was `allow`: the núcleo carries it
    // out on the next tick and there is nothing to answer.
    await renderDecisions(teamView(), [teamRun({ id: "run-1" })], {
      actions: [teamAction({ proposal_id: null })],
    });

    const panel = await panelFor("Actions it wants to take");
    expect(await within(panel).findByText("granted — nobody decides")).toBeDefined();
    expect(within(panel).queryByRole("button", { name: "Approve" })).toBeNull();
  });

  it("shows a payload it cannot parse verbatim rather than dropping it", async () => {
    await renderDecisions(teamView(), [teamRun({ id: "run-1" })], {
      actions: [teamAction({ payload: "{not json" })],
    });

    const panel = await panelFor("Actions it wants to take");
    expect(await within(panel).findByText("{not json")).toBeDefined();
  });
});
