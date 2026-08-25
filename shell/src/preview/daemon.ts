import type { BudgetView } from "../data/system";
import type { TeamAction, TeamRun, TeamRunView, TeamTrigger, TeamView } from "../data/teams";

/**
 * A núcleo made of fixtures, for looking at pages with.
 *
 * **Not part of the app and not a test.** Nothing under `src/` imports this;
 * the only thing that reaches it is `preview.html`, which only
 * `preview.vite.config.mjs` builds. It exists because a suite that passes and a
 * screen that reads well are different claims, and jsdom can only make the
 * first one — spacing, contrast, what a long name does to a table, whether the
 * eye lands where it should. Those need pixels.
 *
 * The fixtures are deliberately awkward rather than tidy: a department with
 * nobody in it, one with no ceiling at all, a specialist serving four
 * departments, a mission long enough to wrap, a task mid-flight. A preview
 * built out of a happy path shows a layout that has never been asked a
 * question.
 */

const DAY = 86_400_000;
/** Fixed, because a screenshot taken twice should be the same screenshot. */
const NOW = Date.parse("2026-08-24T09:41:00Z");

function ago(ms: number): string {
  return new Date(NOW - ms).toISOString();
}

/* -------------------------------------------------------- the departments -- */

function team(overrides: Partial<TeamView>): TeamView {
  return {
    id: "x",
    name: "X",
    mission: "",
    director_agent_id: "",
    max_rounds: 3,
    max_parallel: 2,
    budget_usd: 5,
    max_open_actions: 5,
    max_live_runs: 1,
    created_at: ago(90 * DAY),
    updated_at: ago(2 * DAY),
    members: [],
    grants: [],
    ...overrides,
  };
}

export const TEAMS: TeamView[] = [
  team({
    id: "financas",
    name: "Finanças",
    mission:
      "Reconcile what the bank says with what the ledger says, chase the invoices nobody has paid, and be the one who notices before the accountant does.",
    director_agent_id: "controller",
    members: ["controller", "auditor", "researcher", "reviewer"],
    grants: [
      { kind: "send_email", mode: "propose" },
      { kind: "file_document", mode: "allow" },
    ],
    max_rounds: 4,
    max_parallel: 2,
    budget_usd: 5,
    max_open_actions: 5,
    max_live_runs: 1,
  }),
  team({
    id: "marketing",
    name: "Marketing",
    mission: "Write what goes out, and keep it sounding like one company rather than eleven.",
    director_agent_id: "editor",
    members: ["editor", "writer", "researcher", "reviewer"],
    grants: [
      { kind: "send_email", mode: "propose" },
      { kind: "calendar_event", mode: "propose" },
    ],
    max_rounds: 3,
    max_parallel: 3,
    budget_usd: 8,
    max_open_actions: 6,
    max_live_runs: 2,
  }),
  team({
    id: "vendas",
    name: "Vendas",
    mission: "Answer the people who wrote in, and remember what was promised to whom.",
    director_agent_id: "closer",
    members: ["closer", "researcher", "writer"],
    grants: [{ kind: "send_email", mode: "allow" }],
    max_rounds: 3,
    max_parallel: 2,
    budget_usd: 3,
    max_open_actions: 8,
    max_live_runs: 2,
  }),
  team({
    id: "seguranca",
    name: "Segurança",
    /* No ceiling of its own — the one department where an armed routine can spend
       without bound, which is the case the interlock exists for. */
    budget_usd: null,
    mission: "Read what the scanners say, and decide which of it is actually true.",
    director_agent_id: "analyst",
    members: ["analyst", "reviewer"],
    grants: [],
    max_rounds: 6,
    max_parallel: 1,
    max_open_actions: 3,
    max_live_runs: 1,
  }),
  team({
    id: "informatica",
    name: "Informática",
    mission: "Keep the machines patched and the backups restorable, and prove the second one.",
    director_agent_id: "sysadmin",
    members: ["sysadmin", "reviewer", "researcher"],
    grants: [{ kind: "file_document", mode: "allow" }],
    max_rounds: 3,
    max_parallel: 4,
    budget_usd: 12,
    max_open_actions: 5,
    max_live_runs: 3,
  }),
  team({
    id: "operacoes",
    name: "Operações",
    /* Created and not yet staffed: a column of empty cells, and the state that
       stops every task it is asked to run. */
    mission: "Just created — nobody on it yet.",
    director_agent_id: "",
    members: [],
    grants: [],
    budget_usd: null,
    max_rounds: 3,
    max_parallel: 2,
    max_open_actions: 5,
    max_live_runs: 1,
  }),
];

/* --------------------------------------------------------------- the work -- */

function run(overrides: Partial<TeamRun>): TeamRun {
  return {
    id: "r",
    team_id: "financas",
    request: "",
    workspace: "",
    state: "done",
    director_node: "none",
    director_run_id: null,
    round: 1,
    next_ordinal: 2,
    dry_rounds: 0,
    plan_retries: 0,
    replanned: "no",
    outcome: "done",
    why: null,
    created_at: ago(DAY),
    updated_at: ago(DAY),
    finished_at: ago(DAY),
    trigger_id: null,
    parent_id: null,
    root_id: "r",
    depth: 0,
    ...overrides,
  };
}

export const RUNS: TeamRun[] = [
  run({
    id: "run-live-1",
    team_id: "financas",
    request: "Reconcile the October invoices against the bank statement",
    state: "working",
    round: 2,
    outcome: null,
    finished_at: null,
    created_at: ago(23 * 60 * 1000),
  }),
  run({
    id: "run-live-2",
    team_id: "marketing",
    request: "Draft the release note for the September changes",
    state: "planning",
    round: 1,
    outcome: null,
    finished_at: null,
    created_at: ago(4 * 60 * 1000),
  }),
  run({ id: "r1", team_id: "financas", request: "Chase the three invoices past sixty days", created_at: ago(DAY) }),
  run({ id: "r2", team_id: "financas", request: "Close August", created_at: ago(2 * DAY) }),
  run({ id: "r3", team_id: "financas", request: "Close July", created_at: ago(3 * DAY) }),
  run({
    id: "r4",
    team_id: "financas",
    request: "Reconcile September",
    state: "failed",
    outcome: "failed",
    why: "the bank export was truncated at 500 rows and nobody noticed until round three",
    created_at: ago(4 * DAY),
  }),
  run({ id: "r5", team_id: "financas", request: "Chase the Ferreira invoice", created_at: ago(5 * DAY) }),
  run({ id: "r6", team_id: "marketing", request: "Rewrite the pricing page", created_at: ago(DAY) }),
  run({ id: "r7", team_id: "marketing", request: "September newsletter", created_at: ago(3 * DAY) }),
  run({ id: "r8", team_id: "vendas", request: "Answer the four that came in overnight", created_at: ago(DAY) }),
  run({ id: "r9", team_id: "seguranca", request: "Triage the weekly scan", created_at: ago(2 * DAY) }),
  run({ id: "r10", team_id: "informatica", request: "Prove last week's backup restores", created_at: ago(DAY) }),
];

export const RUN_VIEWS: Record<string, TeamRunView> = {
  "run-live-1": {
    ...RUNS[0],
    cost_usd: 1.24,
    items: [
      { ordinal: 1, round: 1, agent_id: "auditor", description: "pull the bank export", state: "done", run_id: 11, output_path: null },
      { ordinal: 2, round: 1, agent_id: "researcher", description: "pull the ledger", state: "done", run_id: 12, output_path: null },
      { ordinal: 3, round: 2, agent_id: "controller", description: "match them line by line", state: "working", run_id: 13, output_path: null },
      { ordinal: 4, round: 2, agent_id: "reviewer", description: "check the exceptions", state: "planned", run_id: null, output_path: null },
    ],
  },
  "run-live-2": {
    ...RUNS[1],
    cost_usd: 0.08,
    items: [
      { ordinal: 1, round: 1, agent_id: "writer", description: "draft it", state: "working", run_id: 21, output_path: null },
    ],
  },
};

/* ----------------------------------------------------------- the routines -- */

function trigger(overrides: Partial<TeamTrigger>): TeamTrigger {
  return {
    id: 0,
    team_id: "financas",
    name: "",
    enabled: 1,
    source: "cron",
    cron: null,
    timezone: null,
    from_team: null,
    email_class: null,
    request: "",
    created_at: ago(30 * DAY),
    updated_at: ago(30 * DAY),
    ...overrides,
  };
}

export const TRIGGERS: TeamTrigger[] = [
  trigger({
    id: 1,
    team_id: "financas",
    name: "monthly reconciliation",
    cron: "0 7 1 * *",
    timezone: "Europe/Lisbon",
    request: "Reconcile last month against the bank statement and file the exceptions.",
  }),
  trigger({
    id: 2,
    team_id: "financas",
    name: "chase the overdue",
    enabled: 0,
    cron: "0 9 * * 1",
    timezone: "Europe/Lisbon",
    request: "List every invoice past sixty days and draft one chasing email each.",
  }),
  trigger({
    id: 3,
    team_id: "marketing",
    name: "after finance closes",
    source: "team_finished",
    cron: null,
    from_team: "financas",
    request: "Write the month's numbers up for the newsletter.",
  }),
  trigger({
    id: 4,
    team_id: "seguranca",
    name: "weekly scan triage",
    enabled: 0,
    cron: "0 6 * * 1",
    timezone: "Europe/Lisbon",
    request: "Read the scanner output and separate the real findings from the noise.",
  }),
  trigger({
    id: 5,
    team_id: "vendas",
    name: "on a new enquiry",
    source: "email_triaged",
    cron: null,
    email_class: "enquiry",
    request: "Draft an answer and check what was promised to this contact before.",
  }),
];

export const TRIGGER_NEXT: Record<number, { next: string | null; error: string | null }> = {
  1: { next: new Date(NOW + 7 * DAY + 5 * 3600_000).toISOString(), error: null },
  2: { next: new Date(NOW + 3 * DAY).toISOString(), error: null },
  3: { next: null, error: "this rule does not fire on a clock" },
  4: { next: new Date(NOW + 4 * DAY).toISOString(), error: null },
  5: { next: null, error: "this rule does not fire on a clock" },
};

/* ---------------------------------------------------------- the decisions -- */

export const ACTIONS: TeamAction[] = [
  {
    id: 1,
    team_run_id: "run-live-1",
    ordinal: 3,
    kind: "send_email",
    payload: JSON.stringify({
      to: "contas@fornecedor.pt",
      subject: "October invoices — three we cannot match",
      body: "Three lines on your October statement have no invoice on our side…",
    }),
    why: "Three lines on their statement have no invoice here, and the amounts are round numbers — either they billed twice or we never received them. Asking is cheaper than guessing.",
    proposal_id: 91,
    state: "pending",
    error: null,
    created_at: ago(11 * 60 * 1000),
    executed_at: null,
  },
  {
    id: 2,
    team_run_id: "run-live-1",
    ordinal: null,
    kind: "file_document",
    payload: JSON.stringify({ path: "financas/2026-10/exceptions.md", title: "October exceptions" }),
    why: "The exception list is what the accountant will ask for first.",
    /* Granted `allow`: nobody decides, and it happens on the next tick. */
    proposal_id: null,
    state: "working",
    error: null,
    created_at: ago(6 * 60 * 1000),
    executed_at: null,
  },
];

export const RECRUITS = [
  {
    id: 21,
    kind: "agent-recruit",
    status: "pending",
    run_id: null,
    session_id: null,
    project_id: null,
    errand_id: null,
    errand_name: null,
    tool_name: "tax-analyst",
    reasoning:
      "Nobody on this department can read a VAT return, and three of October's exceptions are VAT reclassifications rather than mismatches.",
    tool_input: JSON.stringify({
      name: "tax analyst",
      speciality: "reads VAT returns and tells a reclassification from a mistake",
      prompt: "You read Portuguese VAT returns. You never file anything; you explain what you found.",
      engine: "claude",
      model: null,
      tool_policy: "read_only",
      team_id: "financas",
      team_run_id: "run-live-1",
      slug: "tax-analyst",
    }),
    read_from: null,
    created_at: ago(31 * 60 * 1000),
    decided_at: null,
  },
];

/* ------------------------------------------------------------- the agents -- */

const NAMES = [
  "controller",
  "auditor",
  "researcher",
  "reviewer",
  "editor",
  "writer",
  "closer",
  "analyst",
  "sysadmin",
];

export const AGENTS = NAMES.map((name) => ({
  id: name,
  name,
  speciality: "",
  prompt: "",
  engine: "claude",
  model: null,
  tool_policy: "read_only",
  created_at: ago(90 * DAY),
  updated_at: ago(90 * DAY),
}));

/* ------------------------------------------------------------- the router -- */

/**
 * What this fake daemon answers, by path.
 *
 * Anything not named here answers `[]` — the shell reads forty routes and this
 * preview is about two pages. An empty list is the honest default: it is a real
 * answer the app is built to render, unlike a 500, which would put the whole
 * window into a connection state and hide the thing being looked at.
 */
export function answer(path: string, init?: RequestInit): unknown {
  if (path === "/teams") return TEAMS;
  if (path === "/team-runs") return RUNS;
  if (path === "/team-triggers") return TRIGGERS;
  if (path === "/team-actions") return ACTIONS;
  if (path === "/proposals/recruits") return RECRUITS;
  if (path === "/agents") return AGENTS;
  if (path === "/autopilot/budget") {
    /*
      `satisfies` and not a bare literal, because this is the one fixture that
      was ever wrong. Guessed field names cost a white screen: `BudgetLine`
      calls `.toFixed` on `window_spend_usd`, and an invented `spent_usd` is
      `undefined` by the time it gets there — a defect with no stack anyone
      sees, since the boundary catches it and renders an apology instead.

      Every other fixture here goes through a builder returning the app's own
      type, so a field renamed in the núcleo breaks `tsc -b`. This one returned
      an object literal into an `unknown`, which is precisely how it drifted.
      Pinned, the same gate now covers it.
    */
    return {
      limit_usd: 5,
      period: "daily",
      hourly_limit_usd: null,
      per_run_reserve_usd: 0.25,
      time_cost_per_hour_usd: 0,
      window_spend_usd: 4.1,
      hourly_spend_usd: 0.42,
      paused: false,
      reason: null,
    } satisfies BudgetView;
  }

  const next = /^\/team-triggers\/(\d+)\/next$/.exec(path);
  if (next !== null) return TRIGGER_NEXT[Number(next[1])] ?? { next: null, error: null };

  const runView = /^\/team-runs\/([^/]+)$/.exec(path);
  if (runView !== null && init?.method === undefined) return RUN_VIEWS[runView[1]] ?? null;

  const team = /^\/teams\/([^/]+)$/.exec(path);
  if (team !== null && init?.method === undefined) {
    return TEAMS.find((row) => row.id === team[1]) ?? null;
  }

  return [];
}
