import type { Agent } from "../data/agents";
import type { ClassTally } from "../data/autopilot";
import type { Concurrency, Job, JobDetail, JobItem, RunSearchResult } from "../data/fleet";
import type { MapImport, MapModule, ProjectMap } from "../data/project-map";
import type {
  InspectEntry,
  InspectMatch,
  ProjectRules,
} from "../data/projects";
import type { CalendarConfigView, EventOccurrence } from "../data/calendar";
import type { FeedEntry, FeedSeen, FeedTimeline, PendingNotification } from "../data/feed";
import type { Branches } from "../data/project-git";
import type { ProjectReadings } from "../data/project-readings";
import type { BudgetView, HealthReadout, KillSwitchState, ProjectSummary, Proposal, SidecarState } from "../data/system";
import type { VoiceConfigView } from "../data/voice";
import type { TeamAction, TeamRun, TeamRunView, TeamTrigger, TeamView } from "../data/teams";
import type { RunDetail, RunStop, RunTailChunk } from "../data/runs";
import type { EmailDetail } from "../data/mail";
import type { VcsRequestSummary } from "../data/waiting";

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

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
/** Fixed, because a screenshot taken twice should be the same screenshot. */
export const NOW = Date.parse("2026-08-24T09:41:00Z");

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
    /* `closer` directs Vendas and serves here: until this line no director in
       the fixture served a second department, so two things were never drawn.
       `Who`'s accent ring and dashed outline together — leading one place and
       shared with another — and the catalogue's employment column showing both
       of its marks at once, which is the only case where `◉ 1 ● 1` means two
       departments rather than one counted twice. */
    members: ["sysadmin", "reviewer", "researcher", "closer"],
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

/* --------------------------------------------------------------------- jobs -- */

/**
 * One job, mid-flight, with a queue worth drawing.
 *
 * Directed by a team, because `depends_on` and `agent_name` are null for every item of every
 * job that is not -- and a queue with no dependencies is a straight line, which is the one
 * shape that says nothing. This one has two roots that can run at once, a join that waits on
 * both, a second round, and three of the readings that are easy to get wrong:
 * `gate_failed` (which the wire cannot tell from "going round again"), `cancelled` (withdrawn
 * work, never the failure tone) and `conflicted` (owed a resolution run, not waiting on a person).
 */
function jobItem(overrides: Partial<JobItem>): JobItem {
  return {
    ordinal: 0,
    description: "",
    status: "pending",
    round: 0,
    run_id: null,
    gate_status: null,
    agent_id: null,
    agent_name: null,
    depends_on: [],
    files: [],
    ...overrides,
  };
}

export const JOB: Job = {
  id: 24,
  project_id: "alpha",
  rule_name: "nightly reconciliation",
  status: "implementing",
  wait_reason: null,
  max_items: 8,
  created_at: ago(DAY / 12),
  completed_at: null,
  slot: 0,
  round: 1,
  max_rounds: 3,
  team_id: "financas",
  team_name: "Finanças",
  team_max_parallel: 2,
};

export const JOB_VIEW: JobDetail = {
  ...JOB,
  branch: "job/24-reconciliation",
  items: [
    jobItem({
      ordinal: 0, round: 0, description: "read the bank export", status: "passed",
      gate_status: "passed", agent_id: "auditor", agent_name: "Ana", run_id: 101,
      files: ["core/src/storage.rs"],
    }),
    jobItem({
      ordinal: 1, round: 0, description: "read the ledger", status: "passed",
      gate_status: "passed", agent_id: "researcher", agent_name: "Rui", run_id: 102,
      files: ["core/src/files.rs"],
    }),
    jobItem({
      ordinal: 2, round: 0, description: "match them line by line", status: "running",
      agent_id: "controller", agent_name: "Ana", run_id: 103, depends_on: [0, 1],
      files: ["core/src/triage.rs"],
    }),
    jobItem({
      ordinal: 3, round: 0, description: "check the exceptions", status: "pending",
      agent_id: "reviewer", agent_name: "Rui", depends_on: [2],
    }),
    jobItem({
      ordinal: 4, round: 1, description: "widen the gate", status: "gate_failed",
      gate_status: "failed", agent_id: "auditor", agent_name: "Ana", run_id: 104,
      files: ["scripts/gates.sh"],
    }),
    jobItem({
      ordinal: 5, round: 1, description: "drop the old import path", status: "cancelled",
      agent_id: "researcher", agent_name: "Rui",
    }),
    jobItem({
      ordinal: 6, round: 1, description: "rewrite the importer", status: "conflicted",
      agent_id: "controller", agent_name: "Ana", run_id: 105, depends_on: [4],
      files: ["core/src/storage.rs"],
    }),
  ],
};

export const RUN_VIEWS: Record<string, TeamRunView> = {
  "run-live-1": {
    ...RUNS[0],
    cost_usd: 1.24,
    items: [
      { ordinal: 1, round: 1, agent_id: "auditor", description: "pull the bank export", state: "done", run_id: 11, output_path: null },
      { ordinal: 2, round: 1, agent_id: "researcher", description: "pull the ledger", state: "done", run_id: 12, output_path: null },
      { ordinal: 3, round: 2, agent_id: "controller", description: "match them line by line", state: "running", run_id: 13, output_path: null },
      { ordinal: 4, round: 2, agent_id: "reviewer", description: "check the exceptions", state: "pending", run_id: null, output_path: null },
    ],
  },
  "run-live-2": {
    ...RUNS[1],
    cost_usd: 0.08,
    items: [
      { ordinal: 1, round: 1, agent_id: "writer", description: "draft it", state: "running", run_id: 21, output_path: null },
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

export const PROPOSALS: Proposal[] = [
  ...["alpha", "alpha", "alpha", "bravo", "bravo"].map((project_id, index) => ({
    id: 101 + index,
    kind: "action-approval",
    status: "pending",
    run_id: null,
    session_id: null,
    project_id,
    errand_id: null,
    errand_name: null,
    tool_name: "Bash",
    reasoning: "The next action needs an owner's approval.",
    tool_input: JSON.stringify({ command: "git status" }),
    read_from: null,
    created_at: ago((index + 1) * 60 * 1000),
    decided_at: null,
  })),
];

export const TEAM_ACTION_PROPOSALS: Proposal[] = ACTIONS.map((action) => ({
  id: action.proposal_id ?? action.id,
  kind: "team-action",
  status: action.state === "pending" ? "pending" : "approved",
  run_id: null,
  session_id: null,
  project_id: "alpha",
  errand_id: null,
  errand_name: null,
  tool_name: action.kind,
  reasoning: action.why,
  tool_input: action.payload,
  read_from: null,
  created_at: action.created_at,
  decided_at: action.executed_at,
}));

/* ------------------------------------------------------------- the agents -- */

/**
 * The catalogue, pinned to the app's own type.
 *
 * It was nine copies of one row built by `NAMES.map` into an untyped object,
 * and every part of it that mattered was wrong: `speciality: ""` on all nine
 * when `agent::validate` refuses an empty one, `prompt: ""` likewise, and
 * `tool_policy: "read_only"` — a value the daemon has never accepted. `tsc`
 * never saw any of it, because the array went into `answer`'s `unknown` return
 * with nothing on the way to check it. That is precisely the drift the note on
 * the budget fixture below warns about, and the fix is the same one: a builder
 * that returns `Agent`, so a field renamed in the núcleo breaks the build.
 *
 * The ids are load-bearing — `TEAMS` above points at them — so they stay as
 * they were. Awkward on purpose, as the header asks: a renamed agent whose id
 * no longer matches, four rows with no model of their own, a `local` engine, an
 * agent with no tools, two nobody employs, and a name long enough to test the
 * column.
 */
function agent(overrides: Partial<Agent>): Agent {
  return {
    id: "x",
    name: "x",
    speciality: "",
    prompt: "",
    engine: "claude",
    model: null,
    tool_policy: "mcp_only",
    created_at: ago(90 * DAY),
    updated_at: ago(30 * DAY),
    ...overrides,
  };
}

export const AGENTS: Agent[] = [
  agent({
    id: "controller",
    name: "controller",
    speciality: "Runs the books: reconciles the ledger and decides what is worth chasing.",
    prompt: "You are the controller. Reconcile first, judge second, and never guess at a figure.",
    model: "claude-opus-5",
    updated_at: ago(3 * DAY),
  }),
  /* Renamed after it was created, which is the permanent state of the id and the
     name disagreeing — and the one fact only this page can show. */
  agent({
    id: "auditor",
    name: "Auditor Sénior",
    speciality: "Checks the controller's work and says so plainly when it does not add up.",
    prompt: "You audit. Assume the number is wrong until the second source agrees with it.",
    updated_at: ago(11 * DAY),
  }),
  agent({
    id: "researcher",
    name: "researcher",
    speciality: "Goes and finds the thing nobody has bothered to look up yet.",
    prompt: "You research. Cite where each claim came from, or do not make it.",
    model: "claude-sonnet-5",
    updated_at: ago(21 * DAY),
  }),
  /* No tools at all — a policy that decides what the agent can do, and reads as
     decoration until something says so. */
  agent({
    id: "reviewer",
    name: "reviewer",
    speciality: "Reads what the others wrote and refuses it when it is not ready.",
    prompt: "You review. Be specific about what is wrong and where.",
    model: "claude-sonnet-5",
    tool_policy: "none",
    updated_at: ago(6 * DAY),
  }),
  agent({
    id: "editor",
    name: "editor",
    speciality:
      "Keeps everything that goes out sounding like one company rather than eleven, and cuts what does not earn its line.",
    prompt: "You edit. Shorter, and in the house voice.",
    model: "claude-opus-5",
    updated_at: ago(2 * DAY),
  }),
  agent({
    id: "writer",
    name: "writer",
    speciality: "Writes the first draft so somebody has something to argue with.",
    prompt: "You write drafts. Plain sentences, no throat-clearing.",
    engine: "codex",
    model: "gpt-5-codex",
    updated_at: ago(14 * DAY),
  }),
  agent({
    id: "closer",
    name: "closer",
    speciality: "Answers the people who wrote in, and remembers what was promised to whom.",
    prompt: "You close. Never promise a date the team has not agreed to.",
    updated_at: ago(9 * DAY),
  }),
  /* Runs on this machine, which is the one engine that spends nothing. */
  agent({
    id: "analyst",
    name: "analyst",
    speciality: "Reads what the scanners say and decides which of it is actually true.",
    prompt: "You triage findings. A false positive costs more than a slow answer.",
    engine: "local",
    model: "qwen3-coder:30b",
    updated_at: ago(4 * DAY),
  }),
  /* A policy this shell has no reading for. The daemon cannot send one today —
     `validate` takes `mcp_only` and `none` and nothing else — which is exactly
     why it is here: the arm that says so is otherwise unreachable, and an
     unreachable arm is one nobody has ever looked at. */
  agent({
    id: "sysadmin",
    name: "sysadmin",
    speciality: "Keeps the machines patched and the backups restorable, and proves the second one.",
    prompt: "You operate. Change one thing at a time and write down what you changed.",
    model: "claude-sonnet-5",
    tool_policy: "read_only",
    updated_at: ago(30 * DAY),
  }),
  /* Nobody employs this one, and nothing else in the app would ever say so. */
  agent({
    id: "tradutor",
    name: "tradutor",
    speciality: "Translates between pt-PT and en-GB without flattening either.",
    prompt: "You translate. Keep the register, not just the words.",
    model: "claude-haiku-4-5",
    updated_at: ago(66 * DAY),
  }),
  /* A long name, renamed, on nobody's roster: the row that tells you whether the
     column widths were chosen or merely happened. */
  agent({
    id: "revisor-de-contratos",
    name: "Revisor de contratos, cláusulas e anexos",
    speciality:
      "Reads a contract end to end and lists what changed since the last version, including the annexes nobody opens.",
    prompt: "You review contracts. Quote the clause, then say what it now means.",
    updated_at: ago(48 * DAY),
  }),
];

/* --------------------------------------------------------- the projects -- */

/**
 * Four projects, chosen so the inspector has to answer something awkward.
 *
 * The page's whole subject is what a project does when nobody is watching, and
 * the states that matter are the ones nothing else in the app reports: a rules
 * file that will not parse, a rule that is armed and inert, a queue that
 * refuses every merge over a key in a gitignored file, a brake that is holding.
 * A preview with one healthy project would photograph none of them.
 *
 * `alpha` works and is busy, `bravo` is broken in the two ways that stop a
 * project silently, `charlie` has never been given a folder, and `delta` has a
 * folder and no rules at all — which is ordinary and must not read as a fault.
 *
 * **`alpha` is `promotable`, and it is the only one.** Until it was, no fixture
 * here had ever earned the third mode segment, so no shot had ever photographed
 * that segment enabled — and none could photograph what it says once armed. The
 * consequence sentence rode inside the segment as its armed label, wrapped, and
 * grew the roster row from 90.6 to 125.0 pixels under the pointer about to press
 * it again: a regression that shipped precisely because the state it broke had
 * no picture. It is `shadow` with every class clearing the bar and two of them
 * withheld, which is what the núcleo asks for, and it keeps `3 of 4` proposal
 * slots so the armed sentence in the shot is the sentence the tests pin. `delta`
 * took over the 2-of-5 classes, so the "still short of the bar" blocker is still
 * photographed somewhere — a folder with no RULES is not contradicted by having
 * shadow classes short of the bar.
 */
function project(overrides: Partial<ProjectSummary>): ProjectSummary {
  return {
    project_id: "x",
    mode: "shadow",
    project_root: null,
    pending: 0,
    classes_ready: 0,
    classes_total: 0,
    promotable: false,
    open_proposals: 0,
    wip_limit: null,
    queue_full: false,
    root_exists: null,
    last_gate: null,
    last_gate_at: null,
    ...overrides,
  };
}

export const PROJECTS: ProjectSummary[] = [
  project({
    project_id: "alpha",
    mode: "shadow",
    project_root: "C:/repos/alpha",
    root_exists: true,
    open_proposals: 3,
    wip_limit: 4,
    // Every class clears the bar and two are classes the classifier withheld — the daemon's
    // own conditions for offering promotion. `promotable` is still carried, never derived:
    // the shell prints the row's numbers and does not recompute the núcleo's arithmetic.
    classes_ready: 5,
    classes_total: 5,
    withheld_classes_ready: 2,
    promotable: true,
    last_gate: "passed",
    last_gate_at: ago(3 * 3600_000),
  }),
  project({
    project_id: "bravo",
    mode: "active",
    project_root: "C:/repos/bravo-servicos-partilhados",
    root_exists: true,
    // At the ceiling: the brake is holding, which is one of the five findings.
    open_proposals: 2,
    wip_limit: 2,
    queue_full: true,
    last_gate: "failed",
    last_gate_at: ago(2 * DAY),
  }),
  project({ project_id: "charlie", mode: "off" }),
  project({
    project_id: "delta",
    project_root: "C:/repos/delta",
    root_exists: true,
    wip_limit: null,
    // alpha's old numbers, so the "3 of 5 action classes are still short of the bar" blocker
    // keeps a row to be photographed in. delta's story is a folder with no rules file, which
    // shadow classes short of the bar do not contradict.
    classes_ready: 2,
    classes_total: 5,
  }),
];

/* ------------------------------------------------------- the queue of pushes -- */

/**
 * Three rows through the queue that serialises every push, merge and rebase.
 *
 * There was no branch for this route at all, so every GET fell through to `[]` and the
 * queue photographed as "nothing has been through" on every surface that reads it. That is
 * the worst kind of missing fixture: an empty list is a legitimate state, so the picture
 * looked fine and simply showed a panel nobody had ever seen do anything.
 *
 * Two of the three want a person and one does not, which is the whole shape of the panel:
 * `escalated` means somebody owns a conflict now (the queue working, not breaking),
 * `blocked` is terminal without being a failure, and `succeeded` is the history kept behind
 * them. Nothing prunes the table, so a listing is a record and not a backlog.
 */
export const VCS_REQUESTS: VcsRequestSummary[] = [
  {
    id: 412,
    op: "merge",
    project_id: "alpha",
    repo_key: "C:/repos/alpha",
    origin: "run",
    status: "escalated",
    created_at: ago(40 * 60 * 1000),
  },
  {
    id: 409,
    op: "push",
    project_id: "bravo",
    repo_key: "C:/repos/bravo-servicos-partilhados",
    origin: "run",
    status: "blocked",
    created_at: ago(5 * 3600_000),
  },
  {
    id: 404,
    op: "rebase",
    project_id: "alpha",
    repo_key: "C:/repos/alpha",
    origin: "owner",
    status: "succeeded",
    created_at: ago(1 * DAY),
  },
];

/**
 * What the classifier has recorded for each project, class by class.
 *
 * The route had no branch, so the daemon fell through to `[]` and `10-project-state` said
 * "Nothing recorded in shadow yet" beside a roster row claiming 5 of 5 classes clearing the
 * bar — the contradiction reached a screen for the first time when round 8 made alpha
 * promotable. The bar is `READINESS_MIN_REVIEWED` reviews at `READINESS_MIN_AGREE_PERCENT`
 * agreement, per class; `ClassTally` carries no "withheld" field, so a class the classifier
 * held back is one with `would_allow: 0` and the total sitting in `would_pend`/`would_deny`.
 */
export const SCOREBOARD: Record<string, ClassTally[]> = {
  alpha: [
    { mode: "shadow", action_class: "read-local", total: 46, would_allow: 46, would_pend: 0, would_deny: 0, reviewed: 18, agree: 18, disagree: 0 },
    { mode: "shadow", action_class: "confined-to-workspace", total: 31, would_allow: 31, would_pend: 0, would_deny: 0, reviewed: 14, agree: 14, disagree: 0 },
    { mode: "shadow", action_class: "vcs-local", total: 12, would_allow: 12, would_pend: 0, would_deny: 0, reviewed: 11, agree: 11, disagree: 0 },
    // The two the classifier withheld — the other half of the bar, and the reason alpha's
    // row carries `withheld_classes_ready: 2`.
    { mode: "shadow", action_class: "unrecognized", total: 22, would_allow: 0, would_pend: 22, would_deny: 0, reviewed: 20, agree: 19, disagree: 1 },
    { mode: "shadow", action_class: "push-merge-deploy", total: 15, would_allow: 0, would_pend: 15, would_deny: 0, reviewed: 12, agree: 12, disagree: 0 },
  ],
  delta: [
    { mode: "shadow", action_class: "read-local", total: 30, would_allow: 30, would_pend: 0, would_deny: 0, reviewed: 12, agree: 12, disagree: 0 },
    { mode: "shadow", action_class: "confined-to-workspace", total: 18, would_allow: 18, would_pend: 0, would_deny: 0, reviewed: 10, agree: 10, disagree: 0 },
    // Short on evidence, not on agreement — the panel has to be able to show both ways of
    // failing the bar, or "still short" reads as one thing.
    { mode: "shadow", action_class: "vcs-local", total: 9, would_allow: 9, would_pend: 0, would_deny: 0, reviewed: 4, agree: 4, disagree: 0 },
    { mode: "shadow", action_class: "unrecognized", total: 11, would_allow: 0, would_pend: 11, would_deny: 0, reviewed: 10, agree: 8, disagree: 2 },
    { mode: "shadow", action_class: "destructive", total: 6, would_allow: 0, would_pend: 0, would_deny: 6, reviewed: 3, agree: 3, disagree: 0 },
  ],
};

/**
 * A moment relative to the REAL clock, not the frozen one.
 *
 * Everything else here is pinned to `NOW` so a screenshot taken twice is the
 * same screenshot. These two cannot be: `RelativeTime` reads against the
 * machine's own clock, so a next fire pinned to a fixed date photographs as
 * "6d ago" — a next fire in the past, which is a lie in the picture rather
 * than a stable one. The strings drift by an hour between runs; the direction
 * of time does not.
 */
function fromNow(ms: number): string {
  return new Date(Date.now() + ms).toISOString();
}

function rules(overrides: Partial<ProjectRules>): ProjectRules {
  return {
    project_id: "x",
    project_root: null,
    rules_file: "absent",
    rules_error: null,
    gate_command: null,
    gate_before_publish: false,
    judge: { state: "default" },
    schedules: [],
    repo_triggers: [],
    wip_limit: null,
    open_proposals: 0,
    queue_full: false,
    ...overrides,
  };
}

const ALPHA_RULES: ProjectRules = rules({
  project_id: "alpha",
  project_root: "C:/repos/alpha",
  rules_file: "present",
  gate_command: "cargo test -p nucleos-core --all-features",
  gate_before_publish: true,
  wip_limit: 4,
  open_proposals: 3,
  schedules: [
    {
      name: "nightly-tidy",
      cron: "0 3 * * *",
      prompt: "Tidy the imports and run the formatter over anything that moved today.",
      cwd: null,
      timezone: "Europe/Lisbon",
      next_fire_at: fromNow(5 * 3600_000),
      problem: null,
      last_fired_at: fromNow(-21 * 3600_000),
      fires_today: 1,
      daily_cap: 4,
    },
    {
      /* Armed and inert. The daemon skips this rule 2,880 times a day and logs
         at debug, so without this row the rule silently never runs. */
      name: "weekly-audit",
      cron: "0 7 * * MON",
      prompt: "Read what changed this week and write down anything that looks like a decision.",
      cwd: "core",
      timezone: "Europe/Lisboa",
      next_fire_at: null,
      problem: "unknown timezone: Europe/Lisboa",
      last_fired_at: null,
      fires_today: 0,
      daily_cap: 1,
    },
    {
      /* Today's allowance spent — a rule that is fine and will not run again
         until midnight, which is a different fact from both of the above. */
      name: "hourly-sweep",
      cron: "0 * * * *",
      prompt:
        "Sweep the queue for proposals nobody has answered and summarise them in one line each, so that the morning does not start with forty unread rows and no idea which of them matters.",
      cwd: null,
      timezone: null,
      next_fire_at: fromNow(40 * 60_000),
      problem: null,
      last_fired_at: fromNow(-40 * 60_000),
      fires_today: 6,
      daily_cap: 6,
    },
  ],
  repo_triggers: [
    {
      name: "on-main",
      branch: "main",
      prompt: "Run the gate on whatever just landed.",
      last_sha: "9f2c1ab7d4e08b3c5a6f7e8d9c0b1a2f3e4d5c6b",
    },
    {
      /* Armed with nothing to compare against fires nothing, by design. */
      name: "on-release",
      branch: "release/2026-09",
      prompt: "Build the installer and attach it to the draft release.",
      last_sha: null,
    },
  ],
});

const BRAVO_RULES: ProjectRules = rules({
  project_id: "bravo",
  project_root: "C:/repos/bravo-servicos-partilhados",
  /* The two silent stoppers at once: a file that will not parse, so every rule
     below is absent because none could be loaded — and a queue set to wait for
     a gate that does not exist, which refuses every merge. */
  rules_file: "unreadable",
  rules_error:
    "unknown field `schedule`, expected one of `schedules`, `repo_triggers`, `gate`, `gate_before_publish`, `wip_limit` at line 3 column 1",
  gate_command: null,
  gate_before_publish: true,
  wip_limit: 2,
  open_proposals: 2,
  queue_full: true,
});

const CHARLIE_RULES: ProjectRules = rules({ project_id: "charlie" });

/* A folder, a readable file, and nothing in it. Ordinary, and the page must not
   dress it as a fault: the file is gitignored, so a fresh clone has none. */
const DELTA_RULES: ProjectRules = rules({
  project_id: "delta",
  project_root: "C:/repos/delta",
  rules_file: "present",
  gate_command: "npm run gate",
});

const RULES: Record<string, ProjectRules> = {
  alpha: ALPHA_RULES,
  bravo: BRAVO_RULES,
  charlie: CHARLIE_RULES,
  delta: DELTA_RULES,
};

/**
 * One folder of `alpha`, by path.
 *
 * Deep enough to need a breadcrumb trail, and with one name long enough to ask
 * the listing whether its column width was chosen or merely happened.
 */
const TREE: Record<string, InspectEntry[]> = {
  "": [
    { name: ".ai", is_dir: true },
    { name: "core", is_dir: true },
    { name: "shell", is_dir: true },
    { name: "sidecars", is_dir: true },
    { name: ".gitignore", is_dir: false },
    { name: "AGENTS.md", is_dir: false },
    { name: "Cargo.lock", is_dir: false },
    { name: "Cargo.toml", is_dir: false },
    { name: "README.md", is_dir: false },
  ],
  core: [
    { name: "src", is_dir: true },
    { name: "tests", is_dir: true },
    { name: "Cargo.toml", is_dir: false },
  ],
  "core/src": [
    { name: "autopilot", is_dir: true },
    { name: "config.rs", is_dir: false },
    { name: "gate.rs", is_dir: false },
    { name: "inspect.rs", is_dir: false },
    { name: "job.rs", is_dir: false },
    { name: "main.rs", is_dir: false },
    { name: "ownership.rs", is_dir: false },
    { name: "the_scheduler_and_everything_it_reads_from_disk.rs", is_dir: false },
  ],
};

/** A file worth opening: the very document this page reports on. */
const FILE = `# Read on open, never polled. The núcleo parses this strictly, so an
# unknown key is an error rather than a silently empty ruleset.

schedules:
  - name: nightly-tidy
    cron: "0 3 * * *"
    timezone: Europe/Lisbon
    daily_cap: 4
    prompt: >-
      Tidy the imports and run the formatter over anything that moved today.

  # Switched off since the timezone stopped parsing. Leave this comment here —
  # a form would re-serialise the file and delete it.
  - name: weekly-audit
    cron: "0 7 * * MON"
    timezone: Europe/Lisboa
    daily_cap: 1
    cwd: core
    prompt: Read what changed this week.

gate: cargo test -p nucleos-core --all-features
gate_before_publish: true
wip_limit: 4
`;

/** An uncommitted diff, with the four line kinds a reader has to tell apart. */
const DIFF = `diff --git a/core/src/gate.rs b/core/src/gate.rs
index 3a1f9c2..b7e4d81 100644
--- a/core/src/gate.rs
+++ b/core/src/gate.rs
@@ -41,9 +41,14 @@ impl Gate {
     pub fn command(&self) -> Option<&str> {
-        self.command.as_deref()
+        // An empty string is not a command. It reached here as one, and the
+        // queue then measured every merge against a shell that does nothing.
+        match self.command.as_deref() {
+            Some(text) if text.trim().is_empty() => None,
+            other => other,
+        }
     }

     pub fn before_publish(&self) -> bool {
         self.before_publish
     }
diff --git a/core/src/config.rs b/core/src/config.rs
index 8c2b0d4..1e9a3f7 100644
--- a/core/src/config.rs
+++ b/core/src/config.rs
@@ -12,6 +12,7 @@ pub struct Rules {
     pub schedules: Vec<Schedule>,
     pub repo_triggers: Vec<RepoTrigger>,
+    pub gate_before_publish: bool,
 }
`;

/** Matches for `gate_before_publish`, spread over the files that mention it. */
const MATCHES: InspectMatch[] = [
  { path: ".ai/autopilot.yaml", line: 21, text: "gate_before_publish: true" },
  { path: "core/src/config.rs", line: 15, text: "    pub gate_before_publish: bool," },
  {
    path: "core/src/config.rs",
    line: 88,
    text: "            gate_before_publish: raw.gate_before_publish.unwrap_or(false),",
  },
  { path: "core/src/gate.rs", line: 47, text: "    pub fn before_publish(&self) -> bool {" },
  {
    path: "core/src/vcs.rs",
    line: 612,
    text: "        if rules.gate_before_publish && rules.gate_command.is_none() {",
  },
  {
    path: "shell/src/pages/Projects.tsx",
    line: 528,
    text: "      <GatePanel command={rules.data.gate_command} beforePublish={rules.data.gate_before_publish} />",
  },
];

/* --------------------------------------------------------------- calendar -- */

/**
 * A month with something to look at in it.
 *
 * **The dates are absolute and pinned to {@link NOW}'s week**, and the harness
 * freezes the clock to match (`preview/main.tsx`). A calendar drawn against a
 * real clock photographs a different month every day, which makes two shots
 * impossible to compare and makes "is this right?" unanswerable.
 *
 * Deliberately awkward, like the rest of this file. There is a day carrying
 * more than three occurrences, so the `+n more` count is in the picture; a
 * morning with three genuinely overlapping meetings, which is the only thing
 * that exercises the lane arithmetic; a proposal, which has a tone of its own;
 * an occurrence an exception has MOVED, whose `occurrence_local` is therefore
 * a different day from where it draws; and a Saturday with something on it, so
 * a non-working day is not photographed empty and therefore unstyled.
 */
function occurrence(
  eventId: number,
  title: string,
  local: string,
  minutes: number,
  extra: { source?: string; movedTo?: string } = {},
): EventOccurrence {
  /*
    The fixture's zone is UTC+1 — August in Lisbon, which is the machine this
    app runs on. Written as an explicit offset rather than through a `Date`
    built from the local string, so the fixture states what the daemon would
    send instead of re-deriving it with the same arithmetic the page uses.
  */
  const at = `${extra.movedTo ?? local}+01:00`;
  const starts = new Date(at);
  return {
    event_id: eventId,
    title,
    source: extra.source ?? "human",
    // The ORIGINAL local start, which SURVIVES a move and is the identity.
    occurrence_local: local,
    starts_at: starts.toISOString(),
    ends_at: new Date(starts.getTime() + minutes * 60_000).toISOString(),
  };
}

export const CALENDAR: EventOccurrence[] = [
  /* Monday: an ordinary day. */
  occurrence(1, "Standup", "2026-08-24T09:30:00", 15),
  occurrence(2, "Reconcile the ledger", "2026-08-24T11:00:00", 90),
  occurrence(3, "Call with the accountant", "2026-08-24T16:00:00", 45),

  /* Tuesday: three at once — the case the month cannot draw and the week can. */
  occurrence(4, "Design review", "2026-08-25T10:00:00", 60),
  occurrence(5, "Interview — backend", "2026-08-25T10:15:00", 45),
  occurrence(6, "Vendor call", "2026-08-25T10:30:00", 30),
  occurrence(7, "Retro", "2026-08-25T15:00:00", 60),

  /* Wednesday: more than three, so the month has to count the rest. */
  occurrence(8, "Standup", "2026-08-26T09:30:00", 15),
  occurrence(9, "Pairing on the migration", "2026-08-26T10:00:00", 120),
  occurrence(10, "Lunch with the Vendas team", "2026-08-26T13:00:00", 60),
  occurrence(11, "Security review", "2026-08-26T15:00:00", 60),
  occurrence(12, "Write up the quarter", "2026-08-26T17:00:00", 45),

  /* Thursday: a proposal nobody has approved, and a title long enough to be cut. */
  occurrence(13, "Draft the reply to the auditor's third question", "2026-08-27T09:00:00", 30),
  occurrence(14, "Quarterly planning", "2026-08-27T14:00:00", 120, { source: "proposal" }),

  /* Friday: an occurrence an exception moved here from Thursday. */
  occurrence(15, "Moved: budget sign-off", "2026-08-27T11:00:00", 60, {
    movedTo: "2026-08-28T11:00:00",
  }),

  /* Saturday, so a non-working day is not photographed empty. */
  occurrence(16, "Football", "2026-08-29T10:00:00", 90),

  /* The week before and the week after, so the month is not one busy row. */
  occurrence(17, "Standup", "2026-08-17T09:30:00", 15),
  occurrence(18, "One-to-one", "2026-08-18T14:00:00", 30),
  occurrence(19, "Board pack due", "2026-08-31T09:00:00", 60),
  occurrence(20, "Standup", "2026-09-01T09:30:00", 15),
];

/**
 * The week of a clock change, so the 23-hour column can be photographed.
 *
 * Lisbon jumps 01:00 WET to 02:00 WEST on 29 March 2026. The offsets here are
 * therefore `+00:00` on the Saturday and `+01:00` on the Sunday — written out
 * rather than computed, because the whole point of the shot is to check that
 * the page reaches the same conclusion by itself.
 */
export const CALENDAR_DST: EventOccurrence[] = [
  {
    event_id: 30,
    title: "Late one",
    source: "human",
    occurrence_local: "2026-03-28T23:00:00",
    starts_at: "2026-03-28T23:00:00Z",
    ends_at: "2026-03-29T00:00:00Z",
  },
  {
    event_id: 31,
    title: "Morning after the clocks",
    source: "human",
    occurrence_local: "2026-03-29T10:00:00",
    starts_at: "2026-03-29T09:00:00Z",
    ends_at: "2026-03-29T10:00:00Z",
  },
  {
    event_id: 32,
    title: "Brunch",
    source: "human",
    occurrence_local: "2026-03-29T11:30:00",
    starts_at: "2026-03-29T10:30:00Z",
    ends_at: "2026-03-29T12:00:00Z",
  },
];

/** Working hours and weekdays, which the grid draws as a wash and a dimming. */
export const CALENDAR_CONFIG: CalendarConfigView = {
  default_tz: "Europe/Lisbon",
  working_hours_start: "09:00",
  working_hours_end: "18:00",
  working_weekdays: ["mon", "tue", "wed", "thu", "fri"],
};

/**
 * What the calendar held back, and what it later let through.
 *
 * Both lists non-empty, because the panel keeps them apart and a preview with
 * only one of them photographs half a component.
 */
/**
 * Representative feed rows, including the unmapped-device state.
 *
 * Every summary is in the shape the núcleo's own writer builds, and that is the point rather than
 * decoration. Two of the row's readings are PARSED out of the summary — `waitReasonFromSummary`
 * and `readEfficiencySignal` — and the round-10 fixture's generic sentences ("job 40 is waiting",
 * "efficiency observation") parsed to `null`, so neither device had ever appeared in a shot. The
 * generic summaries also made the badge look redundant: six of twelve badge/summary pairs on
 * `06-feed.png` were the same string, which is a fact about this fixture and not about the page.
 */
export const FEED: FeedEntry[] = [
  { id: 14, project_id: "alpha", kind: "job_finished", summary: "job 41 finished `completed` after 6 item(s)", run_id: 41, errand_id: null, subject: "job:41", created_at: ago(3 * MINUTE) },
  { id: 13, project_id: "alpha", kind: "job_started", summary: "job 42 started on job/42-tighten-the-gate", run_id: 42, errand_id: null, subject: "job:42", created_at: ago(9 * MINUTE) },
  { id: 12, project_id: null, kind: "team_run_finished", summary: "a team run done: the department delivered", run_id: null, errand_id: null, subject: "team_run:30", created_at: ago(14 * MINUTE) },
  { id: 11, project_id: "bravo", kind: "job_failed", summary: "job 39 could not start its implement node: the runner exited before the first turn", run_id: 39, errand_id: null, subject: "job:39", created_at: ago(31 * MINUTE) },
  { id: 10, project_id: "bravo", kind: "job_waiting", summary: "job 40 is waiting: another run holds the project's worktree slot", run_id: 40, errand_id: null, subject: "job:40", created_at: ago(48 * MINUTE) },
  { id: 9, project_id: "alpha", kind: "vcs_request_finished", summary: "vcs request 21 escalated — the merge would revert two files nobody asked about", run_id: null, errand_id: null, subject: "vcs:21", created_at: ago(HOUR) },
  { id: 8, project_id: null, kind: "team_trigger_armed", summary: "`morning digest` is armed for support", run_id: null, errand_id: null, subject: null, created_at: ago(95 * MINUTE) },
  { id: 7, project_id: null, kind: "email_urgent", summary: "the accountant is blocked on the Q3 reconciliation and has asked twice", run_id: null, errand_id: null, subject: null, created_at: ago(2 * HOUR) },
  { id: 6, project_id: "alpha", kind: "token_efficiency", summary: "token efficiency (project alpha): 4 runs in a row sent a prompt of 38412 tokens and neither read nor wrote a single cached token", run_id: 38, errand_id: null, subject: null, created_at: ago(3 * HOUR) },
  { id: 5, project_id: null, kind: "web.read", summary: "read https://docs.rs/sqlx/latest/sqlx/ (raw)", run_id: null, errand_id: 2, subject: "errand:2", created_at: ago(4 * HOUR) },
  { id: 4, project_id: null, kind: "errand_rule_fired", summary: "the rule \"weekday sweep\" of the errand \"inbox\" started a turn", run_id: null, errand_id: 2, subject: "errand:2", created_at: ago(5 * HOUR) },
  { id: 3, project_id: "delta", kind: "council_finished", summary: "council done", run_id: null, errand_id: null, subject: "council:11", created_at: ago(7 * HOUR) },
  { id: 2, project_id: "bravo", kind: "worktree_released", summary: "released worktree C:/Projects/bravo/.nucleos/worktrees/run-318 + branch run/318-retry-the-gate", run_id: null, errand_id: null, subject: "run:318", created_at: ago(DAY) },
  { id: 1, project_id: "alpha", kind: "map_stamp_recorded", summary: "module map stamp for core/src/feed.rs", run_id: null, errand_id: null, subject: null, created_at: ago(2 * DAY) },
];

/**
 * A week of the feed as the time axis reads it — `GET /feed/timeline`.
 *
 * Its own list and not `FEED` above, because the two routes answer different questions: `FEED`
 * is the listing's newest fifty and what a search finds, and those rows are pinned by
 * `fixtures.test.ts` for the drawer and the embed. The trace needs TIME — a busy night with a
 * silence in it, and a week of ordinary work behind it dense enough that a lane has to fold its
 * routine sequences into one row — and fourteen rows cannot photograph either.
 *
 * The night is the one the owner approved the direction on: two lines that went wrong (a gate, a
 * run given up on after three attempts), two held (an item that did not merge, a run the núcleo
 * restarted under), two that ask for you (an urgent e-mail, a project ready for active mode), a
 * four-hour quiet from 02:41, and the seen marker at 21:10 the evening before. Two sequences are
 * still open at now — a job parked behind a slot and a run between attempts — so the trace has a
 * dashed ghost to draw. Minutes are UTC offsets from `NOW`.
 *
 * Every line carries the subject the núcleo writes (`job:57`, `run:900598`, `council:12`), which
 * is what folds a job's start, plan, failed gate and unmerged item into one row.
 */
const NIGHT: Omit<FeedEntry, "id">[] = [
  { project_id: null, kind: "command_finished", summary: "project command `gates` on alpha exited 0 after 4m12s", run_id: null, errand_id: null, subject: null, created_at: ago(889 * MINUTE) },
  { project_id: "alpha", kind: "map_stamp_recorded", summary: "module map stamp for core/src/feed.rs at 4d2c1e2", run_id: null, errand_id: null, subject: null, created_at: ago(851 * MINUTE) },
  { project_id: "alpha", kind: "config_written", summary: "wrote .ai/autopilot.yaml: gate command set to scripts/gates.sh shell", run_id: null, errand_id: null, subject: null, created_at: ago(759 * MINUTE) },
  { project_id: "bravo", kind: "run_interrupted", summary: "run 900585 interrupted: the núcleo restarted mid-turn", run_id: 900585, errand_id: null, subject: "run:900585", created_at: ago(686 * MINUTE) },
  { project_id: "charlie", kind: "job_started", summary: "job 54 started on job/54-flaky-hunt from the rule flaky hunt", run_id: null, errand_id: null, subject: "job:54", created_at: ago(637 * MINUTE) },
  { project_id: "charlie", kind: "job_finished", summary: "job 54 finished `completed` after 3 item(s)", run_id: null, errand_id: null, subject: "job:54", created_at: ago(593 * MINUTE) },
  { project_id: "charlie", kind: "shadow_run_completed", summary: "shadow run 900590 completed: would have opened 2 pull requests", run_id: 900590, errand_id: null, subject: "run:900590", created_at: ago(561 * MINUTE) },
  { project_id: "charlie", kind: "vcs_request_finished", summary: "vcs request 44 landed job/54-flaky-hunt into main", run_id: null, errand_id: null, subject: "vcs:44", created_at: ago(511 * MINUTE) },
  { project_id: null, kind: "email_digest", summary: "digest: 23 e-mails triaged, 1 urgent held for the morning", run_id: null, errand_id: null, subject: null, created_at: ago(466 * MINUTE) },
  { project_id: null, kind: "errand_rule_fired", summary: "the rule \"invoice follow-up\" of the errand \"inbox\" started a turn", run_id: null, errand_id: 2, subject: "errand:2", created_at: ago(449 * MINUTE) },
  { project_id: null, kind: "team_run_started", summary: "team run 31 started: Finanças on the weekly close", run_id: null, errand_id: null, subject: "team_run:31", created_at: ago(458 * MINUTE) },
  { project_id: null, kind: "team_action", summary: "Finanças's `ledger_summary` carried out: the Q3 ledger summary is drafted", run_id: null, errand_id: null, subject: "team_run:31", created_at: ago(431 * MINUTE) },
  { project_id: null, kind: "team_run_finished", summary: "team run 31 done: Finanças delivered the weekly close with 3 action(s)", run_id: null, errand_id: null, subject: "team_run:31", created_at: ago(420 * MINUTE) },
  { project_id: "charlie", kind: "worktree_removed", summary: "removed worktree C:/repos/charlie/.nucleos/worktrees/run-900577 after its branch merged", run_id: null, errand_id: null, subject: "run:900577", created_at: ago(175 * MINUTE) },
  { project_id: null, kind: "errand_investigation_done", summary: "errand 2 investigation done: 4 invoice threads matched", run_id: null, errand_id: 2, subject: "errand:2", created_at: ago(151 * MINUTE) },
  { project_id: "delta", kind: "run_retry", summary: "run 900598 attempt 1 failed, retrying: the sidecar handshake timed out", run_id: 900598, errand_id: null, subject: "run:900598", created_at: ago(70 * MINUTE) },
  { project_id: "delta", kind: "run_retry", summary: "run 900598 attempt 2 failed, retrying: the sidecar handshake timed out", run_id: 900598, errand_id: null, subject: "run:900598", created_at: ago(62 * MINUTE) },
  { project_id: "delta", kind: "run_failed_final", summary: "run 900598 failed after 3 attempts: the sidecar handshake timed out", run_id: 900598, errand_id: null, subject: "run:900598", created_at: ago(54 * MINUTE) },
  { project_id: null, kind: "web.read", summary: "read https://docs.rs/git2/latest/git2/struct.Repository.html (raw)", run_id: null, errand_id: 2, subject: "errand:2", created_at: ago(51 * MINUTE) },
  { project_id: "alpha", kind: "job_started", summary: "job 57 started on job/57-importer from the rule nightly reconciliation", run_id: null, errand_id: null, subject: "job:57", created_at: ago(46 * MINUTE) },
  { project_id: "alpha", kind: "job_planned", summary: "job 57 planned 4 item(s) on job/57-importer", run_id: null, errand_id: null, subject: "job:57", created_at: ago(43 * MINUTE) },
  { project_id: "alpha", kind: "job_gate_failed", summary: "job 57 gate failed on round 1: 2 tests in core/src/storage.rs", run_id: 900609, errand_id: null, subject: "job:57", created_at: ago(39 * MINUTE) },
  { project_id: null, kind: "council_started", summary: "council 12 convened on 6 open proposals", run_id: null, errand_id: null, subject: "council:12", created_at: ago(36 * MINUTE) },
  { project_id: "alpha", kind: "worktree_run_completed", summary: "worktree run 900604 completed on run/900604-flaky-gate", run_id: 900604, errand_id: null, subject: "run:900604", created_at: ago(33 * MINUTE) },
  { project_id: null, kind: "council_stage", summary: "council 12 phase 2 done: 4 seats ranked", run_id: null, errand_id: null, subject: "council:12", created_at: ago(27 * MINUTE) },
  { project_id: null, kind: "council_finished", summary: "council 12 done: the week's proposals are ranked", run_id: null, errand_id: null, subject: "council:12", created_at: ago(23 * MINUTE) },
  { project_id: "charlie", kind: "promotion_ready", summary: "charlie has 5 of 5 action classes ready for active mode", run_id: null, errand_id: null, subject: null, created_at: ago(18 * MINUTE) },
  { project_id: null, kind: "email_urgent", summary: "the accountant is blocked on the Q3 reconciliation and has asked twice", run_id: null, errand_id: null, subject: null, created_at: ago(14 * MINUTE) },
  { project_id: "alpha", kind: "worktree_released", summary: "released worktree C:/repos/alpha/.nucleos/worktrees/run-900604 + branch run/900604-flaky-gate", run_id: null, errand_id: null, subject: "run:900604", created_at: ago(11 * MINUTE) },
  { project_id: "bravo", kind: "job_waiting", summary: "job 58 is waiting: another run holds the project's worktree slot", run_id: null, errand_id: null, subject: "job:58", created_at: ago(6 * MINUTE) },
  { project_id: "bravo", kind: "run_retry", summary: "run 900612 attempt 1 failed to launch, retrying: the runner exited before the first turn", run_id: 900612, errand_id: null, subject: "run:900612", created_at: ago(4 * MINUTE) },
  { project_id: "alpha", kind: "job_item_conflicted", summary: "job 57 item 3 did not merge: core/src/storage.rs changed under it on job/57-importer", run_id: null, errand_id: null, subject: "job:57", created_at: ago(2 * MINUTE) },
];

/**
 * The week behind the night: ordinary daytime work, generated and seeded.
 *
 * Working hours only (07:00–20:00 UTC), because a machine that works around the clock would leave
 * the trace no silences to name. The work comes as whole sequences, the way the núcleo writes it —
 * a job's start, plan and finish under one subject, a worktree run and its release under another —
 * so a week of it is dozens of jobs in one lane, and the lane folds its routine ones. A handful of
 * exceptions are placed by hand — a job failing on Wednesday, a worktree the núcleo could not clean
 * up — so the seven-day window has something to find besides density.
 */
function weekBehind(): Omit<FeedEntry, "id">[] {
  const random = seeded(24);
  const rows: Omit<FeedEntry, "id">[] = [];
  const projects = ["alpha", "bravo", "charlie", "delta"];
  type Step = [kind: string, say: (n: number, project: string) => string, afterMinutes: number];
  const stories: { subject: (n: number) => string | null; run: boolean; steps: Step[] }[] = [
    {
      subject: (n) => `job:${n}`,
      run: false,
      steps: [
        ["job_started", (n) => `job ${n} started on job/${n}-maintenance`, 0],
        ["job_planned", (n) => `job ${n} planned 3 item(s) on job/${n}-maintenance`, 3],
        ["job_finished", (n) => `job ${n} finished \`completed\` after 3 item(s)`, 38],
      ],
    },
    {
      subject: (n) => `run:${900000 + n}`,
      run: true,
      steps: [
        ["worktree_run_completed", (n) => `worktree run ${900000 + n} completed on run/${900000 + n}-tidy`, 0],
        ["worktree_released", (n, project) => `released worktree C:/repos/${project}/.nucleos/worktrees/run-${900000 + n} + branch run/${900000 + n}-tidy`, 2],
      ],
    },
    { subject: (n) => `vcs:${n}`, run: false, steps: [["vcs_request_finished", (n) => `vcs request ${n} landed job/${n}-maintenance into main`, 0]] },
    { subject: () => "errand:2", run: false, steps: [["errand_rule_fired", () => 'the rule "weekday sweep" of the errand "inbox" started a turn', 0]] },
    { subject: () => null, run: false, steps: [["team_trigger_armed", () => "`morning digest` is armed for support", 0]] },
  ];
  let n = 100;
  for (let day = 7; day >= 1; day -= 1) {
    const midnight = Date.parse(new Date(NOW - day * DAY).toISOString().slice(0, 10) + "T00:00:00Z");
    rows.push({ project_id: null, kind: "email_digest", summary: "digest: 31 e-mails triaged, nothing urgent", run_id: null, errand_id: null, subject: null, created_at: new Date(midnight + 7 * HOUR + 2 * MINUTE).toISOString() });
    for (let hour = 7; hour < 19; hour += 1) {
      const count = 1 + Math.floor(random() * 3);
      for (let i = 0; i < count; i += 1) {
        const begin = midnight + hour * HOUR + Math.floor(random() * 50) * MINUTE;
        const story = stories[Math.floor(random() * stories.length)];
        const project = projects[Math.floor(random() * projects.length)];
        n += 1;
        const errand = story.subject(n) === "errand:2" ? 2 : null;
        const owner = story.subject(n) === null || errand !== null ? null : project;
        for (const [kind, say, after] of story.steps) {
          const at = begin + after * MINUTE;
          if (at > NOW - 900 * MINUTE) break;
          rows.push({ project_id: owner, kind, summary: say(n, project), run_id: story.run ? 900000 + n : null, errand_id: errand, subject: story.subject(n), created_at: new Date(at).toISOString() });
        }
      }
    }
  }
  const at = (days: number, hour: number, minute: number) => {
    const midnight = Date.parse(new Date(NOW - days * DAY).toISOString().slice(0, 10) + "T00:00:00Z");
    return new Date(midnight + hour * HOUR + minute * MINUTE).toISOString();
  };
  rows.push(
    { project_id: "bravo", kind: "job_started", summary: "job 71 started on job/71-importer", run_id: null, errand_id: null, subject: "job:71", created_at: at(5, 14, 2) },
    { project_id: "bravo", kind: "job_failed", summary: "job 71 could not start its implement node: the runner exited before the first turn", run_id: null, errand_id: null, subject: "job:71", created_at: at(5, 14, 12) },
    { project_id: "delta", kind: "worktree_gc_failed", summary: "could not remove worktree C:/repos/delta/.nucleos/worktrees/run-900431: a file is in use", run_id: null, errand_id: null, subject: "run:900431", created_at: at(3, 10, 40) },
    { project_id: null, kind: "email_triage_stalled", summary: "triage stalled: 4 messages could not be read after 3 attempts", run_id: null, errand_id: null, subject: null, created_at: at(2, 16, 5) },
    { project_id: null, kind: "errand_rule_failed", summary: 'the rule "weekday sweep" of the errand "inbox" failed: the mailbox refused the login', run_id: null, errand_id: 2, subject: "errand:2", created_at: at(6, 9, 30) },
  );
  return rows;
}

/** Oldest first, numbered in that order, as the núcleo's row ids are. */
export const FEED_TIMELINE: FeedEntry[] = [...weekBehind(), ...NIGHT]
  .sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at))
  .map((row, index) => ({ ...row, id: 5000 + index }));

/**
 * Where the reader left off: 21:10 the evening before, eight minutes after the last line they saw.
 *
 * `seen_at` later than `through_created_at` on purpose — the marker is when somebody looked, and
 * the line before it is merely the newest one there was to see.
 */
const SEEN_LINE = FEED_TIMELINE.find((row) => row.kind === "config_written" && row.created_at === ago(759 * MINUTE));
export const FEED_SEEN: FeedSeen = {
  through: SEEN_LINE?.id ?? null,
  through_created_at: SEEN_LINE?.created_at ?? null,
  seen_at: ago(751 * MINUTE),
};

export const HELD: PendingNotification[] = [
  {
    id: 1,
    kind: "email_arrived",
    summary: "The accountant replied about the Q3 reconciliation.",
    queued_at: ago(22 * 60_000),
    delivered_at: null,
  },
  {
    id: 2,
    kind: "run_finished",
    summary: "nucleos/job-24 finished — the gate passed on the third attempt.",
    queued_at: ago(41 * 60_000),
    delivered_at: null,
  },
  {
    id: 3,
    kind: "proposal_raised",
    summary: "Marketing wants to put a quarterly planning block in the calendar.",
    queued_at: ago(3 * 3_600_000),
    delivered_at: ago(2 * 3_600_000),
  },
];

/* ---------------------------------------------------------------- the map -- */

/**
 * A project the size of this one, so the map can be looked at under load.
 *
 * **Generated rather than written out, and the numbers are measured and not
 * invented.** `GET /projects/nucleos/map` answers with 276 modules and 1067
 * imports -- 3.9 a file -- across communities whose sizes run from forty-odd
 * files down to three. Those are the proportions reproduced here. A tidy
 * fixture of eight boxes would photograph a picture that reads beautifully and
 * that nobody has ever seen, and the whole complaint this exists to reproduce
 * is what the drawing does when it is asked to hold a real project.
 *
 * The file names are plausible rather than real. What decides whether this
 * screen can be read is the geometry -- how many boxes, how wide a label, how
 * many communities down the side of a matrix -- and none of that changes with
 * the words in them.
 *
 * Deterministic, like `NOW` above and for the same reason: a screenshot taken
 * twice should be the same screenshot, so the pseudo-random walk that wires the
 * imports is seeded and never `Math.random`.
 */
function seeded(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** One directory's worth of files, and how many of them there are. */
const AREAS: { dir: string; ext: string; reader: MapModule["reader"]; count: number }[] = [
  { dir: "core/src", ext: "rs", reader: "rust", count: 118 },
  { dir: "shell/src/data", ext: "ts", reader: "typescript", count: 34 },
  { dir: "shell/src/pages", ext: "tsx", reader: "typescript", count: 21 },
  { dir: "shell/src/project", ext: "tsx", reader: "typescript", count: 26 },
  { dir: "shell/src/canvas", ext: "ts", reader: "typescript", count: 15 },
  { dir: "shell/src/ui", ext: "tsx", reader: "typescript", count: 19 },
  { dir: "shell/src/app", ext: "tsx", reader: "typescript", count: 12 },
];

/**
 * Stems for the generated names, long and short both.
 *
 * The long ones are the point: a community titled `instrumentation` sits
 * sideways down a matrix column and decides how tall its header has to be, and
 * a fixture of four-letter names would never ask that question.
 */
const STEMS = [
  "job", "runs", "gate", "land", "vcs", "proposals", "scheduler", "recurrence",
  "instrumentation", "classifier", "concurrency", "credentials", "resolver",
  "council", "triage", "sessions", "worktree", "workflow_graph", "map_anchor",
  "map_join", "map_store", "map_recency", "notifications", "transcription",
  "attribution", "budget", "collision", "exclusion", "errands", "detect",
];

function generatedMap(): ProjectMap {
  const random = seeded(20260831);
  const modules: MapModule[] = [];
  const byArea: string[][] = [];

  for (const area of AREAS) {
    const here: string[] = [];
    for (let i = 0; i < area.count; i += 1) {
      const stem = STEMS[(i * 7 + area.dir.length) % STEMS.length];
      const round = Math.floor(i / STEMS.length);
      const path = `${area.dir}/${stem}${round === 0 ? "" : `_${round + 1}`}.${area.ext}`;
      here.push(path);
      modules.push({
        path,
        reader: area.reader,
        // A little over half declare a section, which is roughly what the real
        // walk reports and is the number the header above the picture reads.
        declares: random() < 0.56,
        cites: [],
        spec: null,
        tested: random() < 0.62,
      });
    }
    byArea.push(here);
  }

  /*
    Imports: dense inside a directory, thin across. That is what makes a
    community a community -- the detector finds them from the edges and nothing
    else -- so wiring them uniformly would produce one undifferentiated blob and
    photograph a matrix this app would never draw.
  */
  const imports: MapImport[] = [];
  const seen = new Set<string>();
  const add = (from: string, to: string) => {
    if (from === to) return;
    /*
      Separated by a character no module path can contain, and written as the
      ESCAPE rather than as the byte. A raw NUL anywhere in a source file makes
      git, grep and `file` classify the whole file as binary: the byte that used
      to be here is why `refactor/nomes-em-ingles` missed a mention in this file
      and needed a commit of its own to find it, and why a merge of this file
      reported as one conflict from line 1 to the end. The string this builds is
      identical either way.
    */
    const key = `${from}\0${to}`;
    if (seen.has(key)) return;
    seen.add(key);
    imports.push({ from, to });
  };

  for (const here of byArea) {
    for (let i = 0; i < here.length; i += 1) {
      // Communities form where a group leans on a few files. Biasing the target
      // towards the front of the list gives each area two or three of those,
      // which is the shape the real graph has.
      const many = 2 + Math.floor(random() * 4);
      for (let n = 0; n < many; n += 1) {
        const to = Math.floor(random() ** 2 * here.length);
        add(here[i], here[to]);
      }
    }
  }
  for (let n = 0; n < 70; n += 1) {
    const from = byArea[Math.floor(random() * byArea.length)];
    const to = byArea[Math.floor(random() * byArea.length)];
    add(from[Math.floor(random() * from.length)], to[Math.floor(random() * to.length)]);
  }

  /*
    What no reader here understands. Real code, most of it -- the Go sidecars,
    the migrations, the stylesheets -- and the map's own measure of what it
    cannot see. Drawn as a count and never as boxes, which is why a list of
    plain paths is the whole of what this needs.
  */
  const unread: string[] = [];
  for (let i = 0; i < 190; i += 1) unread.push(`sidecars/${["echo", "email", "telegram", "web"][i % 4]}/${STEMS[i % STEMS.length]}_${i}.go`);
  for (let i = 0; i < 140; i += 1) unread.push(`core/migrations/${String(i).padStart(4, "0")}_${STEMS[i % STEMS.length]}.sql`);

  return {
    modules,
    imports,
    unread,
    foreign: [],
    seam: {
      served: [],
      calls: 0,
      matched: 0,
      computed: [],
      unmatched: [],
      opaque: [],
      uncalled: [],
    },
    junction: {
      decisions: [],
      unclaimed: [],
      unmatched: [],
      counts: {
        decisions: 0,
        declared: 0,
        ambiguous: 0,
        silent: 0,
        unnumbered: 0,
        unclaimed: modules.length - 112,
        unmatched: 112,
      },
    },
    standings: {},
    stamps: {
      settled: 0,
      partial: 0,
      never: 0,
      lapsed: 0,
      withdrawn: 0,
      guessed: 0,
      no_anchor: 0,
      untracked: 0,
      no_repository: 0,
      unwatched: 0,
      decisions: 0,
    },
    triage: {},
    triage_counts: {
      flagged: 0,
      silenced: 0,
      untriaged: 0,
      unseen: 0,
      waiting: 0,
      unchecked: 0,
    },
    git_would_not_answer: false,
    recency: { window: 200, ages: {} },
    last_triaged_at: null,
  };
}

/** Built once: the walk is deterministic, and the shell asks for it on every open. */
const MAP: ProjectMap = generatedMap();

/* -------------------------------------------------------------- the specs -- */

/**
 * What the map's picker offers to read.
 *
 * Enough of them to overflow the box -- the picker caps at `max-h-64` and
 * scrolls, and a list of three would photograph a control that never reaches
 * the state it was built for.
 */
const SPECS: string[] = [
  "2026-07-17-agenticos-foundation-and-autopilot-design",
  "2026-07-20-telegram-channel-design",
  "2026-07-28-email-pillar-design",
  "2026-07-29-autopilot-job-graph-design",
  "2026-07-29-harness-instrumentation-and-verification-design",
  "2026-07-30-pilar-de-voz-design",
  "2026-08-02-fila-vcs-design",
  "2026-08-04-trabalho-noturno-e-jobs-paralelos-design",
  "2026-08-09-canvas-da-frota-design",
  "2026-08-11-equipas-de-agentes-design",
  "2026-08-15-pilar-de-browser-design",
  "2026-08-17-novo-frontend-design",
  "2026-08-24-mapa-do-projeto-design",
  "2026-08-27-dono-da-arvore-principal-design",
];

/* ------------------------------------------------------------- the router -- */

/**
 * What this fake daemon answers, by path.
 *
 * Anything not named here answers `[]` — the shell reads forty routes and this
 * preview is about two pages. An empty list is the honest default: it is a real
 * answer the app is built to render, unlike a 500, which would put the whole
 * window into a connection state and hide the thing being looked at.
 */
/**
 * One conversation, so the composer can be photographed.
 *
 * The composer is the densest object in this app -- a text line, a microphone, four menus and a
 * send button inside one border -- and until now it was the one surface no shot covered. It is also
 * the surface where jsdom is least use: `Chats.test.tsx` renders every one of these controls and
 * asserts on all of them, and cannot see that a wrapper lost its rule and dropped the microphone
 * out of the box. That defect shipped and was found by a person looking at the screen.
 *
 * Rooted in a project and with tools, because that is the state where every rung of the permission
 * menu is reachable -- an unrooted conversation greys three of them and would photograph as a
 * control half out of order.
 */
const CHAT_ID = "c-preview";

export const CHATS = [
  {
    chat_id: CHAT_ID,
    title: "the permission rungs",
    brain: "cloud",
    model: null,
    effort: null,
    fallback_model: null,
    extra_dirs: [],
    turn_budget_usd: null,
    agents: [],
    system_prompt: null,
    denied_tools: [],
    cleared_after_run_id: null,
    context_window: 140000,
    created_at: "2026-08-24T09:00:00Z",
    cwd: "C:/Projects/nucleos",
    ide_session_id: null,
    first_message: "o que muda entre os cinco degraus?",
    last_activity: "2026-08-24T09:05:00Z",
    waiting: 0,
  },
];

export const CHAT_TURNS = [
  {
    id: 1,
    asked: "o que muda entre os cinco degraus?",
    answer:
      "Manual pergunta por tudo o que muda alguma coisa. Edit automatically adianta as edições. " +
      "Plan responde com um plano. Auto corre o que as regras reconhecem. Bypass não pergunta.",
    error: null,
    status: "completed",
    cost_usd: 0.02,
    answered_by: "cloud",
    session_id: "s-preview",
    created_at: "2026-08-24T09:05:00Z",
    did: [],
    images: [],
    thought: [],
    thought_tokens: null,
    context_fill: null,
    context_window: 140000,
    compacted: false,
    relayed_from_chat_id: null,
    relayed_from_title: null,
    relayed_to: [],
  },
];

/*
  Two routes and not one, because two pickers read this list and the second one only exists for the
  half the first barely uses. The chat window draws the whole menu; the judge picker on the
  inspector's rules view draws the LOCAL and HOSTED rows and nothing else — a cloud judge would
  answer through the CLI, and a CLI launched to answer a hook would re-enter it. A cloud-only
  fixture photographs that control with an empty menu, which is a real state and the least
  informative one to look at.

  `gemma3` is deliberately not installed: it is the one row that is listed and not selectable, and
  a shot is the only place that distinction is visible at all.
*/
export const CHAT_MODELS = {
  choices: [
    { id: "sonnet", label: "Sonnet 5", brain: "cloud", efforts: [] },
    {
      id: "opus",
      label: "Opus 5",
      brain: "cloud",
      efforts: ["low", "medium", "high", "xhigh", "max"],
    },
    { id: "qwen3:8b", label: "Qwen 3 8B", brain: "local", efforts: [], installed: true },
    { id: "gemma3:12b", label: "Gemma 3 12B", brain: "local", efforts: [], installed: false },
    { id: "moonshotai/kimi-k2", label: "Kimi K2", brain: "openrouter", efforts: [] },
  ],
  configured: "sonnet",
  efforts: ["low", "medium", "high", "xhigh", "max"],
};

const RUN_DETAIL = {
  id: 1,
  project_id: "alpha",
  status: "completed",
  gate_status: "passed",
  gate_exit_code: 0,
  gate_output: null,
  exit_code: 0,
  stdout: "Checked the changed files.\nThe gate passed.",
  stderr: null,
  session_id: "s-preview",
  cost_usd: 0.0412,
  input_tokens: 18_400,
  output_tokens: 2_100,
  cache_read_tokens: 96_000,
  num_turns: 7,
  context_fill: 132_000,
  steerable: false,
  successor_run_id: null,
} satisfies RunDetail;

/*
 * Four costs are deliberately absent, including two finished runs: recording a
 * cost is not guaranteed. Two excerpts are long enough to wrap, because a list
 * that only sees short prompts has not been asked about its reading width.
 */
const RUN_INDEX: RunSearchResult[] = [
  { id: 1, project_id: "alpha", status: "completed", mode: "real", created_at: ago(5 * 60_000), completed_at: ago(60_000), cost_usd: 0.0412, prompt_excerpt: "Check the changed files, run the selected gate, and summarise the result for the release note." },
  { id: 2, project_id: "bravo", status: "running", mode: "real", created_at: ago(10 * 60_000), completed_at: null, cost_usd: null, prompt_excerpt: "Trace the approval queue delay and prepare a small, reversible fix." },
  { id: 3, project_id: "charlie", status: "awaiting_approval", mode: "shadow", created_at: ago(18 * 60_000), completed_at: null, cost_usd: null, prompt_excerpt: "Review the proposed dependency update before it changes the build image." },
  { id: 4, project_id: "delta", status: "superseded", mode: "worktree", created_at: ago(32 * 60_000), completed_at: ago(30 * 60_000), cost_usd: 0.0084, prompt_excerpt: "Map the incoming request to the owning team and queue the first safe step." },
  { id: 5, project_id: null, status: "failed", mode: "real", created_at: ago(3 * 3_600_000), completed_at: ago(2 * 3_600_000), cost_usd: 0.0167, prompt_excerpt: "Reproduce the sidecar handshake failure with the production-shaped configuration." },
  { id: 6, project_id: null, status: "cancelled", mode: "real", created_at: ago(7 * 3_600_000), completed_at: ago(6 * 3_600_000), cost_usd: 0.0031, prompt_excerpt: "Stop the duplicate migration review after the owner chose the newer branch." },
  { id: 7, project_id: null, status: "timed_out", mode: "real", created_at: ago(13 * 3_600_000), completed_at: ago(11 * 3_600_000), cost_usd: null, prompt_excerpt: "Investigate why the preview service keeps returning an empty list to otherwise healthy screens." },
  { id: 8, project_id: "alpha", status: "completed", mode: "real", created_at: ago(26 * 3_600_000), completed_at: ago(25 * 3_600_000), cost_usd: 0.0289, prompt_excerpt: "Add the missing status label to the activity summary." },
  { id: 9, project_id: "bravo", status: "failed", mode: "shadow", created_at: ago(2 * DAY), completed_at: ago(47 * 3_600_000), cost_usd: 0.0195, prompt_excerpt: "Compare the changed policy with the current queue limits and report conflicts." },
  { id: 10, project_id: "charlie", status: "cancelled", mode: "real", created_at: ago(3 * DAY), completed_at: ago(71 * 3_600_000), cost_usd: 0.0062, prompt_excerpt: "Prepare a recovery checklist for the paused integration." },
  { id: 11, project_id: "delta", status: "interrupted", mode: "worktree", created_at: ago(4 * DAY), completed_at: ago(95 * 3_600_000), cost_usd: null, prompt_excerpt: "Refine the dashboard hierarchy so the queue state remains legible when several teams are blocked at once and the operator needs the cause before the chronology." },
  { id: 12, project_id: null, status: "completed", mode: "real", created_at: ago(5 * DAY), completed_at: ago(119 * 3_600_000), cost_usd: 0.0528, prompt_excerpt: "Document the observed retry pattern, including the handoff signals that distinguish a delayed worker from a run that has silently stopped making progress." },
];

const RUN_STOP = {
  run_id: 1,
  status: "completed",
  kind: "completed",
  summary: "The run completed after the gate passed.",
  decisions_recorded: true,
  gate: null,
  timeout: null,
  leading_up: [],
  exit_code: 0,
  stderr_tail: null,
  successor_run_id: null,
} satisfies RunStop;

const RUN_TAIL = { text: "", next: 0, live: false } satisfies RunTailChunk;

const EMAIL_DETAIL = {
  id: 1,
  from_addr: "mira.chen@example.com",
  from_name: "Mira Chen",
  subject: "Tuesday planning notes",
  received_at: ago(18 * 60 * 1000),
  triage_class: "action",
  triage_summary: "The team needs a reply with the agreed delivery date.",
  triaged_at: ago(14 * 60 * 1000),
  model_class: "action",
  priority_rule: null,
  body_text: "Hi team,\n\nCould you confirm the delivery date from today's planning session?\n\nThanks,\nMira",
  has_attachments: 0,
  attachments: [],
} satisfies EmailDetail;

/**
 * Every supervised sidecar, and the second half of `/health/readout`'s story above.
 *
 * That readout says `browser_sidecar` is down; until this existed the Sidecars panel beneath it read
 * "no sidecar is registered", which is a daemon with no sidecars rather than a daemon with a sidecar
 * that will not start — the empty-list fall-through telling a different story from the row above it.
 *
 * The SUPERVISOR's keys (`sidecar.rs:16-20`), not the readout's row names: `/sidecars` answers about
 * processes and `/health/readout` about pillars. Telegram is absent on purpose — the readout has it
 * `disabled`, nothing supervises it, and a sidecar list that invented a row for it would contradict
 * the row above.
 */
const SIDECARS: SidecarState[] = [
  {
    name: "echo",
    state: "running",
    started_at: "2026-09-13T06:00:00Z",
    last_failure: null,
    last_failure_at: null,
    restarts: 0,
    last_line: null,
    last_line_at: null,
  },
  {
    name: "email",
    state: "running",
    started_at: "2026-09-13T06:00:00Z",
    last_failure: null,
    last_failure_at: null,
    restarts: 1,
    last_line: "email: polled INBOX, 3 new",
    last_line_at: "2026-09-13T08:55:00Z",
  },
  {
    name: "web",
    state: "running",
    started_at: "2026-09-13T06:00:00Z",
    last_failure: null,
    last_failure_at: null,
    restarts: 0,
    last_line: null,
    last_line_at: null,
  },
  {
    name: "browser",
    state: "down",
    started_at: null,
    last_failure: "could not start: The system cannot find the file specified. (os error 2)",
    last_failure_at: "2026-09-13T08:58:00Z",
    restarts: 14,
    last_line: null,
    last_line_at: null,
  },
];

export function answer(path: string, init?: RequestInit): unknown {
  /*
    The house's capacity, with nobody holding a slot. It is here so the Codigo
    mode can be photographed at all: it reads `concurrency.data?.projects` and
    then calls `.find` on it unguarded, so the empty-list default this file
    gives everything else was a crash rather than an empty screen — and the
    empty screen is exactly the one worth looking at, because the door into the
    inspector is drawn in it.
  */
  if (path === "/concurrency") {
    return {
      house: { limit: 4, held: 1 },
      projects: [
        {
          project_id: "alpha",
          limit: 4,
          slots: [
            {
              project_id: "alpha",
              slot: 0,
              owner_kind: "job",
              owner_id: JOB.id,
              claimed_at: ago(DAY / 12),
              job_id: null,
              ordinal: null,
              item_status: null,
            },
          ],
          collision: {
            declared: { state: "clean", overlaps: [] },
            observed: { state: "not_measured", overlaps: [] },
          },
        },
      ],
    } satisfies Concurrency;
  }

  if (path === "/jobs" || path.startsWith("/jobs?")) return [JOB];
  if (/^\/jobs\/\d+$/.test(path)) return JOB_VIEW;
  if (/^\/runs\/\d+\/stop$/.test(path)) return RUN_STOP;
  if (/^\/runs\/\d+\/tail/.test(path)) return RUN_TAIL;
  if ((path === "/runs" || path.startsWith("/runs?")) && init?.method === undefined) {
    const [, query] = splitQuery(path);
    let runs = RUN_INDEX;
    if (query.get("live") === "true") {
      runs = runs.filter((run) => ["running", "awaiting_approval"].includes(run.status));
    }
    for (const key of ["status", "project_id", "mode"] as const) {
      const value = query.get(key);
      if (value !== null) runs = runs.filter((run) => run[key] === value);
    }
    const q = query.get("q");
    if (q !== null) runs = runs.filter((run) => run.prompt_excerpt.toLowerCase().includes(q.toLowerCase()));
    const rawLimit = query.get("limit");
    const limit = rawLimit === null ? Number.NaN : Number(rawLimit);
    return Number.isFinite(limit) ? runs.slice(0, limit) : runs;
  }
  if (/^\/runs\/\d+$/.test(path) && init?.method === undefined) return RUN_DETAIL;
  if (/^\/email\/\d+$/.test(path) && init?.method === undefined) return EMAIL_DETAIL;

  if (path === "/projects") return PROJECTS;

  /*
    The inspector's readers, which are the only routes here that carry a query
    string — so the path is split before it is matched. `cat` and `diff` are
    NOT here: they come back from the núcleo as a bare `String` and are answered
    by `answerText`, which is the same split `Projects.test.tsx` records.
  */
  const [route, query] = splitQuery(path);
  const inspect = /^\/projects\/([^/]+)\/(rules|ls|grep)$/.exec(route);
  if (inspect !== null) {
    const [, id, reader] = inspect;
    if (reader === "rules") return RULES[id] ?? rules({ project_id: id });
    if (reader === "ls") return TREE[query.get("path") ?? ""] ?? [];
    return query.get("q") === null ? [] : MATCHES;
  }

  /*
    The calendar's three reads. `events` is the only one that carries a window,
    and it is answered by FILTERING rather than by returning the whole fixture:
    the page asks for the six weeks it draws, and a fake that ignored `from`
    and `to` would photograph a March event in the August grid — which is
    precisely the class of defect the DST shot exists to catch.

    Filtered on `starts_at`, and not on `occurrence_local`, because that is
    what `recurrence.rs`'s `expand` does — the fixture's moved occurrence is in
    the answer for the week it was moved TO, and the page is expected to draw
    it there.
  */
  if (route === "/calendar/events") {
    const from = Date.parse(query.get("from") ?? "");
    const to = Date.parse(query.get("to") ?? "");
    if (Number.isNaN(from) || Number.isNaN(to)) return [];
    return [...CALENDAR, ...CALENDAR_DST].filter((occurrence) => {
      const starts = Date.parse(occurrence.starts_at);
      return Date.parse(occurrence.ends_at) > from && starts < to;
    });
  }
  /*
    The whole daemon's health, and the reason the System page could not be
    photographed at all: `headlineFor` reads `readout.subsystems.filter(...)`
    unguarded, so the empty-list default this file gives everything else was a
    crash — `Cannot read properties of undefined (reading 'length')` — and the
    page came back as the router's apology instead of a page. Same failure as
    `/concurrency` and the State readings above, and the third of its kind.

    In the daemon's own subsystem order (`health.rs:142-183`), never sorted, and
    deliberately not a clean bill: one sidecar is down, one is degraded and two
    are switched off, because `disabled` and `down` are different sentences and
    a preview of ten green rows can photograph neither. The aggregate agrees
    with the rows — `degraded`, not `ok`, because something is.
  */
  if (path === "/health/readout") {
    return {
      status: "degraded",
      subsystems: [
        { name: "sqlite_pool", status: "ok" },
        { name: "cli_binary", status: "ok" },
        { name: "credential_manager", status: "ok" },
        { name: "worktree_disk", status: "degraded", reason: "low-disk-space" },
        { name: "echo_sidecar", status: "ok" },
        { name: "telegram_sidecar", status: "disabled", reason: "not-configured" },
        { name: "email_sidecar", status: "ok" },
        { name: "web_sidecar", status: "ok" },
        { name: "browser_sidecar", status: "down", reason: "not-running" },
        { name: "voice_transcriber", status: "disabled", reason: "not-configured" },
      ],
    } satisfies HealthReadout;
  }

  if (path === "/sidecars" && init?.method === undefined) return SIDECARS;

  /*
    Asking a supervisor to try now. Stateless, like every other write in this file, and
    deliberately: flipping `browser` to `running` here would make a shot's answer depend on which
    shots ran before it, and would take the control out of every picture after the first press —
    including the armed one, which is the picture this route exists to make possible.
  */
  const restart = /^\/sidecars\/([^/]+)\/restart$/.exec(path);
  if (restart !== null && init?.method === "POST") {
    return { name: decodeURIComponent(restart[1]), asked: true };
  }

  /*
    What the voice pillar is configured to do, and the second half of the same
    story as `/health/readout` above: the Voice page reads `data.hints.length`
    unguarded, so the empty-list default crashed it — and the crash was hidden
    behind a louder one, because `listen()` from `@tauri-apps/api/event` was
    throwing first until that module got a stub next to `tauri.ts`. Two faults
    in a row on one page is exactly how a preview goes unlooked-at.

    Armed, with a cleanup model, because the interesting picture is the page
    that CAN record: unarmed it draws one sentence and stops. The hints are the
    daemon's own vocabulary list, which is the field that made this necessary.
  */
  if (path === "/voice/config") {
    return {
      armed: true,
      hints: ["NucleOS", "núcleo", "worktree", "autopilot", "sidecar"],
      cleanup_prompt: "Tidy the transcript. Keep the words; drop the ums.",
      cleanup_model: "local/whisper-cleanup",
      retain_dictations_days: 30,
      hotkey: "Ctrl+Shift+D",
      memo_hotkey: "Ctrl+Shift+M",
      conversation_hotkey: "",
      /* Reads but does not speak: the half-configured machine is a real state
         and the one a single `armed` flag would hide. */
      speaks: false,
      max_capture_seconds: 120,
      max_body_bytes: 8 * 1024 * 1024,
    } satisfies VoiceConfigView;
  }

  /*
    Whether the stop is engaged, and the fourth fixture gap of the same shape.

    Unanswered, `kill.data` was `[]`, `[].engaged` was `undefined`, and the rail
    drew "state unread" under the kill switch on every page for ever — which is a
    real state of the app (the first poll has not landed, or that route failed)
    being shown permanently because the preview never answered. It reads as a
    defect in the footer, and the owner reasonably asked why it was there.

    `false` — not engaged — because that is the state the rest of the fixtures
    describe: a núcleo with jobs running and proposals waiting is not a stopped
    one, and a preview that said otherwise would contradict every other page.
  */
  if (path === "/autopilot/kill" && init?.method === undefined) {
    return { engaged: false } satisfies KillSwitchState;
  }

  if (path === "/calendar/busy") return { busy: true };
  if (path === "/calendar/config") return CALENDAR_CONFIG;
  if (path === "/notifications/pending") return HELD;
  if (splitQuery(path)[0] === "/feed") return FEED;
  /*
    The axis and its marker. The window and the cursor are applied as the núcleo applies them, so
    the page's incremental poll brings nothing new and a preset photographs exactly its window. A
    POST moves nothing here — a preview is photographed, not used — and answers the marker it was
    given, which is what the page reads back.
  */
  if (splitQuery(path)[0] === "/feed/timeline") {
    const [, query] = splitQuery(path);
    const since = Date.parse(query.get("since") ?? "");
    const until = query.has("until") ? Date.parse(query.get("until") ?? "") : Infinity;
    const after = query.has("after_id") ? Number(query.get("after_id")) : -Infinity;
    const entries = FEED_TIMELINE.filter((row) => {
      const at = Date.parse(row.created_at);
      return at >= since && at <= until && row.id > after;
    });
    return { entries, truncated: false } satisfies FeedTimeline;
  }
  if (path === "/feed/seen") {
    if (init?.method === "POST" && typeof init.body === "string") {
      const { through } = JSON.parse(init.body) as { through: number };
      const line = FEED_TIMELINE.find((row) => row.id === through);
      return { through, through_created_at: line?.created_at ?? null, seen_at: new Date().toISOString() } satisfies FeedSeen;
    }
    return FEED_SEEN;
  }

  /*
    The four readings the State mode leads with — and the reason that mode
    could not be photographed at all. `ModeState` reads
    `readings.data.efficiency.median_total_tokens`, so the empty-list default
    threw before a single control was drawn and the whole page came back as the
    boundary's apology. Same failure as `/concurrency` above, one route along.

    Deliberately not a happy path: some of the runs reported no usage at all,
    the median moved the right way against the window before it, and fifteen
    runs were never judged because nothing asked them to be. Every one of those
    is a sentence this page has to be able to say.
  */
  /*
    What a project would forget by leaving, and what is holding it. Varied per
    project on purpose, because the remove panel says three different things and
    only one of them is reachable from a single fixture: `alpha` has a long
    record and a job holding a slot, so it photographs the refusal; `bravo` has
    a record and nothing in flight, which is the ordinary case with the
    checkbox; `charlie` has nothing at all, and is offered no checkbox because
    there is nothing to decide about.
  */
  const record = /^\/projects\/([^/]+)\/record$/.exec(route);
  if (record !== null) {
    const nothing = { runs: 0, jobs: 0, proposals: 0, decisions: 0, stamps: 0, commands: 0, feed: 0 };
    switch (record[1]) {
      case "alpha":
        return {
          forgets: { ...nothing, runs: 312, jobs: 4, proposals: 3, decisions: 12, stamps: 40, feed: 96 },
          holds: { slots: 1, worktrees: 1 },
        };
      case "bravo":
        return {
          forgets: { ...nothing, runs: 58, proposals: 2, commands: 3 },
          holds: { slots: 0, worktrees: 0 },
        };
      default:
        return { forgets: nothing, holds: { slots: 0, worktrees: 0 } };
    }
  }

  /*
    What deleting a project's folder would take. Four projects, four different
    sentences, because the panel says a different one for each and a single
    fixture would photograph only the dullest: `alpha` has work in flight and
    real uncommitted work, `bravo` is a repository with no remote at all —
    which is the case where nothing in it exists anywhere else — `charlie` was
    never given a folder, and `delta` is a folder git knows nothing about.
  */
  const folder = /^\/projects\/([^/]+)\/folder$/.exec(route);
  if (folder !== null) {
    switch (folder[1]) {
      case "alpha":
        return {
          root: "C:/repos/alpha",
          exists: true,
          only_here: { uncommitted: 12, unpushed: 3 },
          blocked: null,
          holds: { slots: 1, worktrees: 1 },
        };
      case "bravo":
        return {
          root: "C:/repos/bravo-servicos-partilhados",
          exists: true,
          only_here: { uncommitted: 0, unpushed: null },
          blocked: null,
          holds: { slots: 0, worktrees: 0 },
        };
      case "charlie":
        return {
          root: null,
          exists: false,
          only_here: null,
          blocked: {
            refusal: "no_root",
            detail: "this project has no folder recorded, so there is nothing to delete",
          },
          holds: { slots: 0, worktrees: 0 },
        };
      default:
        return {
          root: "C:/repos/delta",
          exists: true,
          only_here: null,
          blocked: null,
          holds: { slots: 0, worktrees: 0 },
        };
    }
  }

  const readings = /^\/projects\/([^/]+)\/readings$/.exec(route);
  if (readings !== null) {
    return {
      window_days: 30,
      efficiency: {
        measured_runs: 41,
        unmeasured_runs: 6,
        median_total_tokens: 128_400,
        previous_median_total_tokens: 154_900,
      },
      cost: { usd: 13.16, runs: 41 },
      gate: { passed: 22, failed: 3, errored: 1, no_gate: 15 },
      delivered: { landed: 9, timed: 7, median_minutes: 34 },
    } satisfies ProjectReadings;
  }

  /*
    Where work lands, and what is standing beside it. An object and not a list,
    which is why the empty default could not stand in for it: the panel reads
    `.branches.length` and an array has no such field.

    One branch is `measured: false` on purpose. That is the case the type's own
    comment exists for -- nobody knows how far ahead it is -- and it is drawn
    differently from `0/0`, so a preview without one photographs a panel that
    has never been asked the question.
  */
  const branches = /^\/projects\/([^/]+)\/branches$/.exec(route);
  if (branches !== null) {
    return {
      integration: "master",
      branches: [
        {
          name: "master",
          ahead: 0,
          behind: 0,
          measured: true,
          last_commit_at: ago(2 * 3_600_000),
          last_subject: "merge feat/inspector-de-projectos into master",
        },
        {
          name: "feat/leitor-de-comandos",
          ahead: 7,
          behind: 15,
          measured: true,
          last_commit_at: ago(DAY),
          last_subject: "three limits that pace autonomy nobody asked for",
        },
        {
          name: "tmp/daemon-timeout",
          ahead: 0,
          behind: 0,
          measured: false,
          last_commit_at: ago(9 * DAY),
          last_subject: "a deadline that outlives the gate it guards",
        },
      ],
      omitted: 0,
    } satisfies Branches;
  }

  /*
    The documents the map can be asked to read. Real slugs from this repository,
    long ones included: the picker truncates, and a fixture of short invented
    names would photograph a box that never has to.
  */
  if (/^\/projects\/[^/]+\/map\/specs$/.test(route)) return SPECS;
  if (/^\/projects\/[^/]+\/map$/.test(route)) return MAP;
  /* An object with a list inside it, so the empty-list default cannot stand in. */
  if (/^\/projects\/[^/]+\/map\/silenced$/.test(route)) return { rows: [], total: 0 };

  if (path === "/assistant/chats") return CHATS;
  if (path === "/assistant/models") return CHAT_MODELS;
  if (path === "/assistant/ide-sessions") return [];
  /* The project block, and the permission rung with it. `tools: true` so the menu photographs with
     all five reachable -- see the note on `CHATS`. */
  if (path === `/assistant/chats/${CHAT_ID}/project`) {
    return {
      cwd: "C:/Projects/nucleos",
      tools: true,
      session: "s-preview",
      permission_mode: "auto",
    };
  }
  /* Before the `?before=` reader below it, and matched on the route rather than the whole path so
     the first page and a scroll-back both answer. */
  if (splitQuery(path)[0] === `/assistant/chats/${CHAT_ID}`) {
    return { turns: CHAT_TURNS, more: false };
  }

  if (path === "/teams") return TEAMS;
  if (path === "/team-runs") return RUNS;
  if (path === "/team-triggers") return TRIGGERS;
  if (path === "/team-actions") return ACTIONS;
  if (path === "/vcs/requests") return VCS_REQUESTS;
  /* Matched on the route rather than the whole path: `useScoreboard` always sends
     `?project_id=`, so a `path ===` comparison would never fire. */
  if (splitQuery(path)[0] === "/scoreboard") {
    return SCOREBOARD[splitQuery(path)[1].get("project_id") ?? ""] ?? [];
  }
  if (path === "/proposals") return PROPOSALS;
  if (path === "/proposals/team-actions") return TEAM_ACTION_PROPOSALS;
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

/** A path and its query, apart. The inspect readers are the only routes with one. */
function splitQuery(path: string): [string, URLSearchParams] {
  const cut = path.indexOf("?");
  if (cut === -1) return [path, new URLSearchParams()];
  return [path.slice(0, cut), new URLSearchParams(path.slice(cut + 1))];
}

/**
 * The routes that answer with text rather than JSON.
 *
 * `get_project_cat` and `get_project_diff` return a bare `String`, so the shell
 * reads them through `apiText` and not `apiFetch`. Without this split the
 * preview would hand both of them a JSON body and photograph a file whose
 * entire contents were `[]` — which is exactly the defect `Projects.test.tsx`
 * mocks two clients to avoid.
 *
 * `null` means *not a text route*, which is different from a text route with
 * nothing in it: an empty file and a clean tree are both real answers.
 */
export function answerText(path: string): string | null {
  const [route] = splitQuery(path);
  const reader = /^\/projects\/([^/]+)\/(cat|diff)$/.exec(route);
  if (reader === null) return null;
  return reader[2] === "cat" ? FILE : DIFF;
}

/**
 * The paths this núcleo refuses, and with what status.
 *
 * Everything else answers 200, deliberately — see the note on `answer`. The
 * exception is a project with no recorded folder: the inspect routes really do
 * answer 404 for it, and that 404 is one of the three the page is built to tell
 * apart. Left at 200 it would be the one absence nobody could ever photograph.
 */
export function refusal(path: string): number | null {
  const [route] = splitQuery(path);
  const reader = /^\/projects\/([^/]+)\/(ls|cat|grep|diff)$/.exec(route);
  if (reader === null) return null;
  const root = PROJECTS.find((row) => row.project_id === reader[1])?.project_root ?? null;
  return root === null ? 404 : null;
}
