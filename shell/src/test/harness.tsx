import type { ReactNode } from "react";
import { QueryClientProvider, type QueryClient } from "@tanstack/react-query";
import {
  Outlet,
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import { render, type RenderResult } from "@testing-library/react";
import { NAV_PATHS } from "../app/nav";
import { createAppQueryClient } from "../app/queryClient";
import { createAppRouter } from "../router";
import { PaletteProvider } from "../ui";
import { ApiRefusal } from "../data/client";
import type { Concurrency, HeldSlot, ProjectConcurrency } from "../data/fleet";
import type { ClassTally } from "../data/autopilot";
import type { Changed, Worktree } from "../data/project-code";
import type { ProjectCommand } from "../data/project-commands";
import type { Claim } from "../data/project-config";
import type { ProjectFolder, ProjectRecord } from "../data/projects";
import type {
  DeclarableOp,
  IntegrationBranch,
  ShellRule,
  Verdict,
} from "../data/project-policy";
import { foldPrefix } from "../data/project-policy";
import type { ListingRead, ProjectRepo, ReadOutcome } from "../data/project-github";
import type { Branches, Commit } from "../data/project-git";
import type { Bundle, Installed, WorkflowDiff } from "../data/workflows";
import type { GraphNode, WorkflowGraph } from "../data/workflow-graph";
import type { Detected } from "../data/detect";
import type { ProjectReadings } from "../data/project-readings";
import type { BudgetView, ProjectSummary, Proposal } from "../data/system";

/**
 * The one test wrapper.
 *
 * Every component in this app that is worth testing needs two things it cannot
 * make for itself — a query cache and a router — and every test that builds
 * them by hand builds them slightly differently. One wrapper means one set of
 * answers to "how long do queries retry for in a test" and "what does the
 * router think the current path is", and it means a change to either is a
 * change in one file.
 *
 * **The API seam is `data/client.ts` and nothing below it.** Tests replace
 * `apiFetch` / `apiText` / `probeHealth` and let the real hooks, the real cache
 * and the real error classes run. Stubbing `fetch` instead would put the
 * client's own refusal parsing inside the thing under test; stubbing the hooks
 * instead would test a mock's opinion of react-query. The seam is where the
 * shell stops being ours and starts being the daemon's.
 *
 * `vi.mock` is hoisted per file and cannot be moved in here, so each test file
 * declares three lines of its own:
 *
 * ```ts
 * vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
 * const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
 * vi.mock("../data/client", async (original) => ({
 *   ...(await original<typeof import("../data/client")>()),
 *   ...daemon,
 * }));
 * ```
 *
 * Spreading the original keeps `ApiRefusal` and `ApiUnavailable` as the real
 * classes, so an `instanceof` in a component matches an error a test threw.
 */

/**
 * jsdom implements no scrolling, and the router scrolls to the top of the
 * document on every navigation. Left alone, a suite that navigates prints a
 * "Not implemented: Window's scrollTo()" line per navigation and buries real
 * output. The absence is a fact about jsdom, not a fault in the router — the
 * same reason `test-setup.ts` fills in `scrollIntoView`.
 */
window.scrollTo = () => {};

/** What the fake daemon is holding. Mutable — a POST in a test changes it. */
export interface DaemonState {
  kill: { engaged: boolean };
  budget: BudgetView;
  projects: ProjectSummary[];
  proposals: Proposal[];
  /**
   * Capacity, which the workspace reads as its occupancy panel.
   *
   * Defaults to an empty house rather than being absent, because absent is not
   * a thing this route does: `concurrency::readout` unions the roster with
   * everything holding a slot precisely so that capacity can never vanish
   * quietly, and a fake that could return nothing would let a test pass against
   * a shape the daemon cannot produce.
   */
  concurrency: Concurrency;
  /**
   * A project's four readings.
   *
   * One payload for every project rather than a map: no test so far needs two projects to read
   * differently, and a map would be scaffolding for a case nobody has.
   *
   * The default is a project with nothing behind it — every count zero, every median `null` — which
   * is both the honest empty answer and the state most worth having under a test by default, since
   * it is what a new project looks like for its first month.
   */
  readings: ProjectReadings;
  /** A project's local branches and the one they are measured against. */
  branches: Branches;
  /** A project's recent commits. */
  log: Commit[];
  /**
   * What one run changed, and where its checkout is.
   *
   * `null` means the route refuses — which is a state with three different meanings on the wire and
   * has to be reachable from a test, because the panel says a different sentence for each.
   */
  changed: Changed | null;
  worktree: Worktree | null;
  /**
   * The text routes, by path prefix.
   *
   * `diff` and `cat` return a body that is not JSON, and an **empty** body is a real answer to both
   * — a clean tree, an empty file. A responder that could not distinguish "empty" from "absent"
   * would make the most common answer untestable.
   */
  text: { diff: string; cat: string | null };
  /**
   * The write boundary this project's daemon declares.
   *
   * A list rather than a flag, because the page must not know the fence by heart: an editor that
   * appeared for a hard-coded path would keep appearing after the daemon stopped granting it. A
   * test can hand back an empty list, which is a real answer — a project the app authors nothing
   * in.
   */
  ownership: Claim[];
  /** What the classifier has been judged on, class by class. */
  scoreboard: ClassTally[];
  /**
   * Every write the shell has made, in order, so a test can assert what was SENT rather than what
   * the component thinks it sent.
   */
  writes: { path: string; contents: string }[];
  /**
   * What `POST /projects/{id}/write` refuses with, or `null` to accept.
   *
   * Refusals are the interesting half of this route — five of them, each a different sentence — and
   * they are reachable only if the fake can be told to give one.
   */
  writeRefusal: { status: number; code: string; detail: string } | null;
  /** What this project can be asked to do to itself. Empty is a real answer: nothing declared. */
  commands: ProjectCommand[];
  /** Command ids the shell asked to start, in order. */
  started: number[];
  /** Every declaration the shell sent, as it sent it. */
  declared: Partial<ProjectCommand>[];
  /** What `POST .../run` refuses with, or `null` to accept. */
  runRefusal: { status: number; code: string; detail: string } | null;
  /**
   * The workflow bundles on this fake machine.
   *
   * `null` is a machine with no library at all, which is a 503 and a different fact from an empty
   * shelf: one says there is nowhere to keep bundles, the other says there are none. The page says
   * a different sentence for each, so both have to be reachable.
   */
  library: Bundle[] | null;
  /** What this project uses, already measured — the daemon does the measuring, not the shell. */
  workflows: Installed[];
  /** What the diff route answers, or `null` for a project with no copy to compare. */
  workflowDiff: WorkflowDiff | null;
  /** Every pin change the shell asked for, as it asked for it. */
  workflowChanges: { verb: string; name: string; version?: string }[];
  /** What every workflow-changing route refuses with, or `null` to accept. */
  workflowRefusal: { status: number; code: string; detail: string } | null;
  /**
   * A `.ai/workflows.yaml` the daemon cannot parse.
   *
   * Its own field rather than a variant of `workflows`, because it is a different answer with a
   * different sentence: an empty list says *this project uses no workflow*, and that is precisely
   * the claim a daemon staring at a broken pins file cannot make.
   */
  pinsError: string | null;
  /**
   * One workflow's graph, or `null` for a bundle with no sequence in it.
   *
   * `null` is a halfway state and not a fault — skills and scripts with nothing saying in what
   * order — which the page says differently from a graph that will not parse.
   */
  graph: WorkflowGraph | null;
  /** Every node override the shell sent, as it sent it. */
  overlays: { name: string; node: string; body: unknown }[];
  /**
   * What `GET /projects/detect` answers, or `null` for a folder that is not there.
   *
   * `null` rather than an empty `Detected`, because "nothing at that path" and "a folder with
   * nothing in it" are two different answers and the wizard says a different sentence for each.
   */
  detected: Detected | null;
  /** Every workflow adopted from a folder, as it was sent. */
  adopted: { name: string; path: string }[];
  /**
   * What a project would forget by leaving, and what is holding it here.
   *
   * One payload for every project, like `readings` above and for the same reason. The default is a
   * project with nothing on record and nothing in flight — which is the case the remove control has
   * to get right first, because it is the one where there is no checkbox to offer.
   */
  record: ProjectRecord;
  /** Every removal the shell asked for, as it asked for it — including whether it said to forget. */
  removed: { projectId: string; forgetHistory: boolean }[];
  /**
   * What `DELETE /projects/{id}` refuses with, or `null` to accept.
   *
   * The 409 is the half of this route worth testing: work in flight is the state a person meets
   * when they try to remove the project they have been using, and it is only reachable if the fake
   * can be told to give it.
   */
  removeRefusal: { status: number; code: string; detail: string } | null;
  /**
   * What deleting this project's folder would take, and whether it would be allowed.
   *
   * The default is the ordinary repository: it is there, git knows it, nothing is uncommitted and
   * nothing is unpushed. Every sentence the delete control can say is a departure from that, so a
   * test that needs one says which.
   */
  folder: ProjectFolder;
  /** Every folder deletion the shell asked for, as it asked for it. */
  folderDeleted: { projectId: string; forgetHistory: boolean }[];
  /** What `DELETE /projects/{id}/folder` refuses with, or `null` to accept. */
  folderRefusal: { status: number; code: string; detail: string } | null;
  /**
   * The three lists a project declares about itself, and the catalogue the second is picked from.
   *
   * Whole rows for the shell rules, because the note and the day a prefix was first declared are on
   * the row and a fake serving two lists of prefixes could not carry either — which is the shape the
   * route deliberately stopped having.
   *
   * `declarableOps` is machine-wide and takes no project id: the set is compiled into the daemon by
   * intersecting the operations it can build with two ceilings, so it is the same answer for every
   * project. The `declarable: false` entries are the half worth keeping in a default — an operation
   * outside the ceilings is not missing, it exists and nothing on any screen can turn it on, and a
   * page has to be able to draw that.
   */
  shellRules: ShellRule[];
  githubOps: string[];
  landTargets: string[];
  /**
   * Where a landing with no argument goes — the other half of `GET /projects/{id}/land-targets`.
   *
   * **Its own field and NOT derived from `branches.integration`**, which is the distinction the
   * route exists to draw: that one is the branch the main checkout is parked on, and a fake that
   * answered this from it would reproduce in the test harness the exact confusion the núcleo now
   * refuses to make. A test that wants them to disagree — a clone sitting on a feature branch while
   * the project declares `master` — sets both, and that is the case worth having.
   */
  landIntegration: IntegrationBranch;
  declarableOps: DeclarableOp[];
  /** What every declaration WRITE refuses with, or `null` to accept. */
  policyRefusal: { status: number; code: string; detail: string } | null;
  /**
   * What the three declaration READS refuse with, or `null` to answer.
   *
   * **Its own field, and its absence is why a bug shipped.** `policyRefusal` covers the writes only,
   * so nothing here could make a GET fail — and the page's three sections guarded on
   * `data === undefined`, which with `retry: false` meant a refused read showed the loading line for
   * ever. A state the fake cannot produce is a state no test can forbid.
   */
  policyReadRefusal: { status: number; code: string; detail: string } | null;
  /** Every declaration the shell sent, in order, as it sent it. */
  policyWrites: { path: string; method: string; body: Record<string, unknown> | null }[];
  /**
   * Which repository on GitHub this project is — `GET /projects/{id}/github-repo`.
   *
   * The whole union and not a string, because five of its six arms are the reasons a project has no
   * repository and each one is a different sentence on the page. `null` is the sixth answer, which
   * is the only refusal this route makes: a project the roster has never heard of.
   *
   * The default is the ordinary case, a project pointed at a GitHub repository, because every
   * assertion about the two listings below needs one before it can begin.
   */
  githubRepo: ProjectRepo | null;
  /**
   * What `gh` said, per listing operation — `POST /github/requests`.
   *
   * Keyed by the operation, because the page asks two and shows them separately, and a fake that
   * answered both with one payload could not tell a test which panel it was looking at.
   *
   * The defaults are a listing each, because that is the state the section exists for. `exit_code`
   * is on the row rather than assumed: a non-zero exit arrives as a 200 from this route — `gh` ran
   * and GitHub said no — and the page draws that differently from a refusal, so it has to be
   * reachable without one.
   */
  githubListings: Record<ListingRead, ReadOutcome>;
  /**
   * What `POST /github/requests` refuses with, or `null` to answer.
   *
   * The half of this route worth testing hardest. §5.1 asks for no token and no `gh` to be
   * *explained and not blank*, and those are a 403 and a 503 that only a fake can produce — no
   * arrangement of the other fields reaches them.
   */
  githubReadRefusal: { status: number; code: string; detail: string } | null;
  /** Every read the shell sent, in order, as the operation it named. */
  githubReads: { op: string; repo: string }[];
}

export function daemonState(overrides: Partial<DaemonState> = {}): DaemonState {
  return {
    kill: { engaged: false },
    budget: {
      limit_usd: 5,
      period: "daily",
      hourly_limit_usd: null,
      per_run_reserve_usd: 0.25,
      time_cost_per_hour_usd: 0,
      window_spend_usd: 1.42,
      hourly_spend_usd: 0.1,
      paused: false,
      reason: null,
    },
    projects: [],
    proposals: [],
    concurrency: { house: { limit: 4, held: 0 }, projects: [] },
    readings: readings(),
    branches: { integration: "master", branches: [], omitted: 0 },
    log: [],
    changed: { paths: [], tracked: 0 },
    worktree: {
      path: "C:/Projects/nucleos-run-41",
      branch: "feat/x",
      base_sha: "a".repeat(40),
      created_at: "2026-08-23T09:00:00Z",
    },
    text: { diff: "", cat: "" },
    ownership: [
      {
        path: ".ai/autopilot.yaml",
        owner: "core",
        what: "what this project does on its own, and the gate command that decides what green means",
      },
    ],
    scoreboard: [],
    writes: [],
    writeRefusal: null,
    commands: [],
    started: [],
    declared: [],
    runRefusal: null,
    library: [],
    workflows: [],
    workflowDiff: null,
    workflowChanges: [],
    workflowRefusal: null,
    pinsError: null,
    graph: null,
    overlays: [],
    detected: null,
    adopted: [],
    record: {
      forgets: { runs: 0, jobs: 0, proposals: 0, decisions: 0, stamps: 0, commands: 0, feed: 0 },
      holds: { slots: 0, worktrees: 0 },
    },
    removed: [],
    removeRefusal: null,
    folder: {
      root: "C:/Projects/nucleos",
      exists: true,
      only_here: { uncommitted: 0, unpushed: 0 },
      blocked: null,
      holds: { slots: 0, worktrees: 0 },
    },
    folderDeleted: [],
    folderRefusal: null,
    shellRules: [],
    githubOps: [],
    landTargets: [],
    // Declared and present, which is the healthy project. `branches.integration` above defaults to
    // `master` too and that agreement is a coincidence of the fixtures, never a rule — the test
    // that matters sets them apart.
    landIntegration: { state: "declared", branch: "master" },
    /*
      A stand-in and not a copy of the real catalogue: that one is derived in `github.rs` by
      intersecting the built operations with two compiled ceilings, and a second spelling of it here
      would be exactly the drift `GET /github/declarable-ops` exists to end. `api_read` is in it by
      name because it is the standing example of the `false` case.
    */
    declarableOps: [
      { kind: "pr_list", half: "read", declarable: true },
      { kind: "run_list", half: "read", declarable: true },
      { kind: "run_logs", half: "read", declarable: false },
      { kind: "pr_comment", half: "action", declarable: true },
      { kind: "api_read", half: "action", declarable: false },
    ],
    policyRefusal: null,
    policyReadRefusal: null,
    policyWrites: [],
    githubRepo: {
      state: "known",
      repo: "duarte/nucleos",
      remote: "git@github.com:duarte/nucleos.git",
    },
    githubListings: {
      pr_list: readOutcome("pr_list", "#41\tthe queue lands\tfeat/land\tabout 2 hours ago"),
      run_list: readOutcome("run_list", "completed\tsuccess\tCI\tmaster\tpush\t9812345\t1m20s"),
    },
    githubReadRefusal: null,
    githubReads: [],
    ...overrides,
  };
}

/**
 * One `gh` invocation's answer, with the fields a test does not care about filled in.
 *
 * `stdout` is a tab-separated line because that is what `gh pr list` actually prints — the typed
 * reads refuse `--json`, so the daemon hands back the CLI's own table and the page renders it. A
 * fixture shaped like JSON would be testing a wire shape this route cannot produce.
 */
export function readOutcome(
  operation: string,
  stdout: string,
  overrides: Partial<ReadOutcome> = {},
): ReadOutcome {
  return {
    status: "ran",
    operation,
    exit_code: 0,
    stdout,
    // stdout then stderr, which for a successful listing is just stdout again.
    output_tail: stdout,
    ...overrides,
  };
}

/**
 * A project holding some slots, as `/concurrency` reports it.
 *
 * Constructors rather than object literals in each test, because `HeldSlot` carries five fields no
 * test cares about — `ordinal`, `item_status`, and the collision pair — and a literal that omits
 * them typechecks nowhere while a literal that includes them is four lines of noise per slot.
 *
 * `not_measured` for both collision sources, which is the honest default: nothing computed an
 * overlap for a fixture, and `clean` is the one answer that must never be given in vain.
 */
export function heldSlots(projectId: string, limit: number, slots: HeldSlot[]): ProjectConcurrency {
  return {
    project_id: projectId,
    limit,
    slots,
    collision: {
      declared: { state: "not_measured", overlaps: [] },
      observed: { state: "not_measured", overlaps: [] },
    },
  };
}

/** One taken slot. `run` by default, which is the only kind the Code mode can open. */
export function slot(overrides: Partial<HeldSlot> = {}): HeldSlot {
  return {
    project_id: "alpha",
    slot: 1,
    owner_kind: "run",
    owner_id: 1,
    claimed_at: "2026-08-23T09:00:00Z",
    job_id: null,
    ordinal: null,
    item_status: null,
    ...overrides,
  };
}

/**
 * One declared command.
 *
 * A constructor because `ProjectCommand` carries nine fields and no test cares about more than
 * three of them at a time — and because `last: null` is the interesting default: a command nobody
 * has run has no verdict, which is not the same as one that failed.
 */
export function projectCommand(overrides: Partial<ProjectCommand> = {}): ProjectCommand {
  return {
    id: 1,
    name: "gate",
    command: "cargo test",
    cwd: null,
    is_gate: true,
    pass_exit_code: 0,
    runnable_by: "person",
    source: "project",
    last: null,
    ...overrides,
  };
}

/**
 * One bundle in the library.
 *
 * `owns` empty by default: a bundle that declares no files is the ordinary one, and a default that
 * claimed a path would make every unrelated test's ownership fence three rows long.
 */
export function bundle(overrides: Partial<Bundle> = {}): Bundle {
  return {
    name: "harness",
    version: "1.0",
    description: "the .ai harness, as a bundle",
    origin: "library:harness@1.0",
    owns: [],
    hash: "sha256:aaaa",
    path: "C:/Users/x/.nucleos/workflows/harness/1.0",
    ...overrides,
  };
}

/**
 * One workflow a project uses.
 *
 * `referenced` and matching hashes by default — the state where nothing needs attention — so a test
 * that wants drift has to say so, rather than every test starting from a page that is shouting.
 */
export function installedWorkflow(overrides: Partial<Installed> = {}): Installed {
  return {
    name: "harness",
    version: "1.0",
    origin: "library:harness@1.0",
    hash: "sha256:aaaa",
    standing: "referenced",
    ejected_at: null,
    origin_hash: "sha256:aaaa",
    local_hash: null,
    update_available: null,
    description: "the .ai harness, as a bundle",
    owns: [],
    overridden_nodes: 0,
    disabled_nodes: 0,
    ...overrides,
  };
}

/**
 * One node of a graph, already resolved by the núcleo.
 *
 * `overridden: false` and no `origin` on any field, which is the inherited state — a default that
 * stamped the project seal would make every test start from a graph that claims to have been
 * changed.
 */
export function graphNode(overrides: Partial<GraphNode> = {}): GraphNode {
  return {
    id: "plan",
    type: "agent",
    role: "plain",
    label: "Plan",
    disabled: false,
    overridden: false,
    fields: [{ name: "model", value: "opus" }],
    ...overrides,
  };
}

/**
 * What the núcleo found in a folder.
 *
 * Nothing found by default — no harness, no commands, not a repository — which is the shape of a
 * plain directory and the one a wizard must handle without pretending anything is missing.
 */
export function detected(overrides: Partial<Detected> = {}): Detected {
  return {
    root: "C:/Projects/thing",
    is_git: false,
    remote: null,
    branch: null,
    head: null,
    harnesses: [],
    commands: [],
    commands_omitted: 0,
    taken_by: null,
    ...overrides,
  };
}

/** A project's readings, empty unless a test says otherwise. */
export function readings(overrides: Partial<ProjectReadings> = {}): ProjectReadings {
  return {
    window_days: 30,
    efficiency: {
      measured_runs: 0,
      unmeasured_runs: 0,
      median_total_tokens: null,
      previous_median_total_tokens: null,
    },
    cost: { usd: 0, runs: 0 },
    gate: { passed: 0, failed: 0, errored: 0, no_gate: 0 },
    delivered: { landed: 0, timed: 0, median_minutes: null },
    ...overrides,
  };
}

export function project(overrides: Partial<ProjectSummary> = {}): ProjectSummary {
  return {
    project_id: "alpha",
    mode: "off",
    project_root: null,
    pending: 0,
    classes_ready: 0,
    classes_total: 0,
    promotable: false,
    open_proposals: 0,
    wip_limit: null,
    queue_full: false,
    ...overrides,
  };
}

export function proposal(overrides: Partial<Proposal> = {}): Proposal {
  return {
    id: 1,
    kind: "action-approval",
    status: "pending",
    run_id: null,
    session_id: null,
    project_id: null,
    errand_id: null,
    errand_name: null,
    tool_name: null,
    reasoning: "",
    tool_input: null,
    read_from: null,
    created_at: "2026-08-17T09:00:00Z",
    decided_at: null,
    ...overrides,
  };
}

/**
 * The day a shell rule this fake stores was first declared.
 *
 * A fixed day rather than "now", and one that is plainly not today, because the caption beside a
 * rule says `declared` and never `edited`: a test that could not tell the two apart could not catch
 * a page that re-dated a rule when its verdict was flipped.
 *
 * The daemon's own spelling — `datetime('now')`, UTC, space-separated, and NOT RFC 3339.
 */
export const DECLARED_ON = "2026-03-14 09:41:00";

/** `ORDER BY prefix`, which is the order the route serves its rows in. */
function byPrefix(left: ShellRule, right: ShellRule): number {
  return left.prefix < right.prefix ? -1 : left.prefix > right.prefix ? 1 : 0;
}

/** One declared shell rule, whole, with the fields a test does not care about filled in. */
export function shellRule(overrides: Partial<ShellRule> = {}): ShellRule {
  return { prefix: "npm ci", verdict: "allow", note: null, created_at: DECLARED_ON, ...overrides };
}

/**
 * A stand-in for the núcleo's JSON routes, over mutable state.
 *
 * A responder rather than a pile of `mockResolvedValueOnce`: the shell polls,
 * so every route is asked repeatedly and in an order nobody controls, and a
 * queue of one-shot answers runs out halfway through the second tick.
 */
export function daemonFetch(state: DaemonState): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    /*
      The declaration routes, all ten, in one block at the very top.

      Ahead of everything else because the bare `DELETE /projects/{id}` further down matches any
      path under `/projects/`, and a `DELETE .../shell-rules` falling into it would delete the
      PROJECT — a fake that removed the row a test is watching while reporting success is the worst
      kind of green.

      Stateful, because the assertion that matters is a write and then a read: a 204 proves the
      request was well formed, and what is under test is whether the list the shell shows afterwards
      is the list the daemon is now enforcing.
    */
    const policy = /^\/projects\/([^/]+)\/(shell-rules|github-ops|land-targets)$/.exec(path);
    if (path === "/github/declarable-ops") return state.declarableOps;
    if (policy !== null) {
      const method = init?.method ?? "GET";
      const table = policy[2];
      const body =
        typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null;

      if (method === "GET") {
        if (state.policyReadRefusal !== null) {
          const { status, code, detail } = state.policyReadRefusal;
          throw new ApiRefusal(status, code, detail);
        }
        if (table === "shell-rules") return [...state.shellRules].sort(byPrefix);
        if (table === "github-ops") return [...state.githubOps].sort();
        // Two halves, because one route owns both: where a landing goes by default, and the extra
        // places it may be sent. The default is never in `targets` — it is admissible with no row,
        // so a fake that listed it would make it look closeable.
        return { integration: state.landIntegration, targets: [...state.landTargets].sort() };
      }

      state.policyWrites.push({ path, method, body });
      if (state.policyRefusal !== null) {
        const { status, code, detail } = state.policyRefusal;
        throw new ApiRefusal(status, code, detail);
      }

      if (table === "shell-rules") {
        // The núcleo folds on the way in, so the fake does too: a prefix is identified by its
        // folded spelling and by nothing else.
        const prefix = foldPrefix(String(body?.prefix ?? ""));
        if (method === "DELETE") {
          state.shellRules = state.shellRules.filter((rule) => rule.prefix !== prefix);
          return undefined;
        }
        const already = state.shellRules.find((rule) => rule.prefix === prefix);
        const written: ShellRule = {
          prefix,
          verdict: body?.verdict as Verdict,
          // `note = excluded.note`, and NOT a merge — whatever arrived is now the note, `null`
          // included. That is the trap the `Note` union exists to make a caller choose out loud.
          note: (body?.note as string | null | undefined) ?? null,
          // Absent from the `DO UPDATE` in the núcleo, so a redeclaration keeps the day the prefix
          // was first written down.
          created_at: already?.created_at ?? DECLARED_ON,
        };
        state.shellRules = [
          ...state.shellRules.filter((rule) => rule.prefix !== prefix),
          written,
        ];
        return undefined;
      }

      if (table === "github-ops") {
        const kind = String(body?.op_kind ?? "");
        state.githubOps =
          method === "DELETE"
            ? state.githubOps.filter((op) => op !== kind)
            : [...state.githubOps.filter((op) => op !== kind), kind];
        return undefined;
      }

      const branch = String(body?.branch ?? "");
      state.landTargets =
        method === "DELETE"
          ? state.landTargets.filter((target) => target !== branch)
          : [...state.landTargets.filter((target) => target !== branch), branch];
      return undefined;
    }

    if (init?.method === "POST") {
      /*
        The one door both GitHub tools come through, first inside this block because it is the only
        POST here that is a READ — everything below records a write, and a listing that fell into
        one of those would be recorded as a change the shell never made.

        Recorded and answered rather than applied to state: nothing on this machine changes when
        `gh pr list` runs, so there is no row for a refetch to read back. What a test asserts is
        WHICH operation was sent and against which repository, because that is the mapping this
        section exists to prove — the repository must be the one the daemon named and never one the
        page worked out for itself.
      */
      if (path === "/github/requests" && typeof init.body === "string") {
        const sent = JSON.parse(init.body) as { op: { op: ListingRead; repo: string } };
        state.githubReads.push({ op: sent.op.op, repo: sent.op.repo });
        if (state.githubReadRefusal !== null) {
          const { status, code, detail } = state.githubReadRefusal;
          throw new ApiRefusal(status, code, detail);
        }
        return state.githubListings[sent.op.op];
      }

      // The one write the shell can make from the frame. Applied to the state so
      // that the refetch after the mutation reads back what was written.
      if (path === "/autopilot/kill" && typeof init.body === "string") {
        state.kill = JSON.parse(init.body) as { engaged: boolean };
      }
      if (path.includes("/write") && typeof init.body === "string") {
        if (state.writeRefusal !== null) {
          const { status, code, detail } = state.writeRefusal;
          throw new ApiRefusal(status, code, detail);
        }
        const body = JSON.parse(init.body) as { path: string; contents: string };
        state.writes.push(body);
        // Applied to the text the read route serves, so the refetch after a save reads back what
        // was written — the same thing the daemon does, and the only way a test can tell a save
        // that landed from one that only looked like it did.
        state.text.cat = body.contents;
      }
      if (path.endsWith("/commands") && typeof init.body === "string") {
        const body = JSON.parse(init.body) as Partial<ProjectCommand>;
        state.declared.push(body);
        // Applied to the list the read route serves, so the refetch after a declaration reads back
        // what was written — the same thing the daemon does, and the only way a test can tell a
        // declaration that landed from one that only looked like it did.
        const id = state.commands.length + 1;
        state.commands = [
          ...state.commands.filter((row) => row.name !== body.name),
          { ...projectCommand(), id, ...body },
        ];
        return { id };
      }
      if (path.includes("/commands/") && path.endsWith("/run")) {
        if (state.runRefusal !== null) {
          const { status, code, detail } = state.runRefusal;
          throw new ApiRefusal(status, code, detail);
        }
        // `/projects/{id}/commands/{command}/run` — the id is the second from last.
        const parts = path.split("/");
        state.started.push(Number(parts[parts.length - 2]));
      }
      // The four pin changes, recorded as sent. One branch because they differ only in the verb,
      // and four near-copies is four places for one of them to stop matching the route.
      if (path.endsWith("/workflows/adopt") && typeof init.body === "string") {
        if (state.workflowRefusal !== null) {
          const { status, code, detail } = state.workflowRefusal;
          throw new ApiRefusal(status, code, detail);
        }
        state.adopted.push(JSON.parse(init.body) as { name: string; path: string });
        return undefined;
      }
      // A node override is a workflow change too, but it names a node as well, so it is recorded
      // with one — asserting on what was SENT is the only way a test tells a save that landed from
      // one that only looked like it did.
      if (path.includes("/workflows/") && path.includes("/nodes/")) {
        if (state.workflowRefusal !== null) {
          const { status, code, detail } = state.workflowRefusal;
          throw new ApiRefusal(status, code, detail);
        }
        const segments = path.split("/");
        const at = segments.indexOf("workflows");
        state.overlays.push({
          name: segments[at + 1] ?? "",
          node: decodeURIComponent(segments[at + 3] ?? ""),
          body: typeof init.body === "string" ? JSON.parse(init.body) : {},
        });
        return undefined;
      }
      if (path.includes("/workflows")) {
        if (state.workflowRefusal !== null) {
          const { status, code, detail } = state.workflowRefusal;
          throw new ApiRefusal(status, code, detail);
        }
        const segments = path.split("/");
        const at = segments.indexOf("workflows");
        const verb = segments[at + 2] ?? "install";
        const body = typeof init.body === "string" ? JSON.parse(init.body) : {};
        state.workflowChanges.push({
          verb,
          name: (segments[at + 1] ?? body.name) as string,
          ...(body.version === undefined ? {} : { version: body.version as string }),
        });
        return undefined;
      }
      if (path.includes("/wip-limit") && typeof init.body === "string") {
        const { limit } = JSON.parse(init.body) as { limit: number | null };
        state.projects = state.projects.map((row) =>
          row.project_id === path.split("/")[2] ? { ...row, wip_limit: limit } : row,
        );
      }
      if (path === "/autopilot/state" && typeof init.body === "string") {
        const change = JSON.parse(init.body) as {
          project_id: string;
          mode: ProjectSummary["mode"];
          project_root?: string;
        };
        // An upsert, because the route is one: this is how a project is registered in the first
        // place, and a fake that could only change an existing row could not test adding one.
        const known = state.projects.some((row) => row.project_id === change.project_id);
        state.projects = known
          ? state.projects.map((row) =>
              row.project_id === change.project_id
                ? { ...row, mode: change.mode, project_root: change.project_root ?? row.project_root }
                : row,
            )
          : [
              ...state.projects,
              {
                ...project(),
                project_id: change.project_id,
                mode: change.mode,
                project_root: change.project_root ?? null,
              },
            ];
      }
      return undefined;
    }

    if (init?.method === "DELETE" && path.includes("/workflows/")) {
      if (state.workflowRefusal !== null) {
        const { status, code, detail } = state.workflowRefusal;
        throw new ApiRefusal(status, code, detail);
      }
      const name = path.split("/").pop() ?? "";
      state.workflowChanges.push({ verb: "forget", name });
      state.workflows = state.workflows.filter((row) => row.name !== name);
      return undefined;
    }

    if (init?.method === "DELETE" && path.includes("/commands/")) {
      const id = Number(path.split("/").pop());
      state.commands = state.commands.filter((row) => row.id !== id);
      return undefined;
    }

    // Ahead of the bare project DELETE below, because it has a segment after the project's and
    // that DELETE would otherwise swallow it.
    if (init?.method === "DELETE" && path.split("?")[0].endsWith("/folder")) {
      if (state.folderRefusal !== null) {
        const { status, code, detail } = state.folderRefusal;
        throw new ApiRefusal(status, code, detail);
      }
      const [route, query] = path.split("?");
      const projectId = decodeURIComponent(route.split("/")[2] ?? "");
      state.folderDeleted.push({
        projectId,
        forgetHistory: new URLSearchParams(query ?? "").get("forget_history") === "true",
      });
      state.projects = state.projects.filter((row) => row.project_id !== projectId);
      return undefined;
    }

    // `DELETE /projects/{id}?forget_history=` — last of the DELETEs, because it is the least
    // specific: every path above has a segment after the project's, and this one is the project.
    if (init?.method === "DELETE" && path.startsWith("/projects/")) {
      if (state.removeRefusal !== null) {
        const { status, code, detail } = state.removeRefusal;
        throw new ApiRefusal(status, code, detail);
      }
      const [route, query] = path.split("?");
      const projectId = decodeURIComponent(route.split("/")[2] ?? "");
      state.removed.push({
        projectId,
        forgetHistory: new URLSearchParams(query ?? "").get("forget_history") === "true",
      });
      // The row goes, because the roster behind the panel is what a test watches to know the
      // removal landed — a fake that recorded the call and left the row would pass a component
      // that never told react-query anything had changed.
      state.projects = state.projects.filter((row) => row.project_id !== projectId);
      return undefined;
    }

    // Parameterised before the exact matches: the readings route carries a project id, which a
    // `switch` over literals cannot express.
    if (path.startsWith("/projects/") && path.endsWith("/folder")) return state.folder;
    if (path.startsWith("/projects/") && path.endsWith("/record")) {
      // 404 for a name the roster does not have, which is what the daemon answers: a record of all
      // zeros is what an unregistered project and a brand new one both look like, and only one of
      // them has a remove control that could ever work.
      const projectId = decodeURIComponent(path.split("/")[2] ?? "");
      if (!state.projects.some((row) => row.project_id === projectId)) {
        throw new ApiRefusal(404, "not_found", "no project by that name");
      }
      return state.record;
    }
    // Ahead of `/branches` and the rest only by convention; it collides with none of them. `null`
    // is the 404 the route makes for a project the roster does not have, and it is the one answer
    // here that is an error rather than a state — the other five arrive as a 200.
    if (path.startsWith("/projects/") && path.endsWith("/github-repo")) {
      if (state.githubRepo === null) {
        throw new ApiRefusal(404, "not_found", "no project by that name");
      }
      return state.githubRepo;
    }
    if (path.startsWith("/projects/") && path.endsWith("/readings")) return state.readings;
    if (path.startsWith("/projects/") && path.endsWith("/branches")) return state.branches;
    if (path.startsWith("/projects/") && path.endsWith("/ownership")) return state.ownership;
    if (path.startsWith("/projects/") && path.endsWith("/commands")) return state.commands;
    if (path.startsWith("/projects/") && path.endsWith("/workflows")) {
      // The daemon resolves the library before it can measure anything against it, so a machine
      // with nowhere to keep bundles refuses here too — the same 503, from the same cause.
      if (state.library === null) {
        throw new ApiRefusal(503, "no_library", "this machine has nowhere for a library");
      }
      if (state.pinsError !== null) {
        throw new ApiRefusal(422, "unreadable_pins", state.pinsError);
      }
      return state.workflows;
    }
    if (path.startsWith("/projects/detect")) {
      if (state.detected === null) {
        throw new ApiRefusal(404, "no_such_folder", "there is nothing at that path");
      }
      return state.detected;
    }
    if (path.startsWith("/projects/") && path.endsWith("/graph")) {
      if (state.graph === null) {
        throw new ApiRefusal(404, "no_graph", "this bundle has no graph in it yet");
      }
      return state.graph;
    }
    if (path.startsWith("/projects/") && path.endsWith("/diff") && path.includes("/workflows/")) {
      if (state.workflowDiff === null) {
        throw new ApiRefusal(404, "not_installed", "this project does not use that workflow");
      }
      return state.workflowDiff;
    }
    if (path === "/workflows/library") {
      if (state.library === null) {
        throw new ApiRefusal(503, "no_library", "this machine has nowhere for a library");
      }
      return state.library;
    }
    if (path.startsWith("/scoreboard")) return state.scoreboard;
    if (path.startsWith("/projects/") && path.includes("/changed")) {
      if (state.changed === null) throw new ApiRefusal(422, "unprocessable", "no branch point");
      return state.changed;
    }
    if (path.startsWith("/projects/") && path.includes("/worktree")) {
      if (state.worktree === null) throw new ApiRefusal(404, "not_found", "gone");
      return state.worktree;
    }
    // The log route carries a query string, so it is matched on its segment rather than its end.
    if (path.startsWith("/projects/") && path.includes("/log")) return state.log;

    switch (path) {
      case "/autopilot/kill":
        return state.kill;
      case "/autopilot/budget":
        return state.budget;
      case "/projects":
        return state.projects;
      case "/proposals":
        return state.proposals;
      case "/concurrency":
        return state.concurrency;
      default:
        return undefined;
    }
  };
}

export interface HarnessOptions {
  initialPath?: string;
  queryClient?: QueryClient;
}

/**
 * The núcleo's text routes, over the same mutable state.
 *
 * A sibling of {@link daemonFetch} rather than part of it, because `apiText` and `apiFetch` are two
 * different functions on the seam and a test replaces them separately.
 */
export function daemonText(state: DaemonState): (path: string) => Promise<string> {
  return async (path) => {
    if (path.includes("/diff")) return state.text.diff;
    if (path.includes("/cat")) {
      // `null` is a file that is not there — a 404, and a different fact from an empty file. A
      // project with no rules file yet is the ordinary case, and the editor says a different
      // sentence for it.
      if (state.text.cat === null) throw new ApiRefusal(404, "not_found", "no such file");
      return state.text.cat;
    }
    return "";
  };
}

export interface HarnessResult extends RenderResult {
  /**
   * Enough of the router for a test to assert where the app ended up.
   *
   * `search` alongside `pathname`, because for some pages the location is not
   * only a path: `/calendar` carries its view and its selected day there, and
   * `/feed` and `/runs` carry their filters. A test that could only see the
   * pathname could not tell a page that keeps its URL honest from one that
   * quietly stops updating it.
   *
   * `unknown` and not a record, because that is what the router itself says
   * here: this tree is built without the app's `validateSearch`, so nothing
   * has promised a shape. A test narrows it to the fields it is asserting on,
   * which is a one-line cast from `unknown` rather than a claim layered over a
   * type that disagrees.
   */
  router: { state: { location: { pathname: string; search: unknown } } };
  queryClient: QueryClient;
}

export interface QueryHarnessResult extends RenderResult {
  queryClient: QueryClient;
}

/**
 * Mount a component that needs the cache but not the router.
 *
 * A fresh `QueryClient` per test, from the same factory the app uses — so the
 * retry policy under test is the app's policy and not a test-only one. Sharing
 * a client between tests would carry one test's answers into the next and turn
 * an ordering bug into a passing suite.
 */
export function renderWithQuery(
  ui: ReactNode,
  options: { queryClient?: QueryClient } = {},
): QueryHarnessResult {
  const queryClient = options.queryClient ?? createAppQueryClient();
  const result = render(<QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>);
  return { ...result, queryClient };
}

/**
 * Mount one component inside a real router that knows every real route.
 *
 * The route tree is built from `NAV_PATHS`, so a link in the component under
 * test resolves against the same paths the app has — a test that navigates
 * proves the destination exists, rather than proving a stub route was
 * registered next to it. The routes themselves render a marker: this helper is
 * for the *rail*, not for the pages.
 */
export async function renderWithRouter(ui: ReactNode, options: HarnessOptions = {}): Promise<HarnessResult> {
  const queryClient = options.queryClient ?? createAppQueryClient();

  const rootRoute = createRootRoute({
    /*
      Wrapped exactly the way the real shell wraps its own tree. A page under
      test gets the app's palette context the same way it gets the app's routes:
      from the root route, for free, whether or not the test is about a palette.
    */
    component: () => (
      <PaletteProvider>
        {ui}
        <Outlet />
      </PaletteProvider>
    ),
  });
  const routes = NAV_PATHS.map((path) =>
    createRoute({
      getParentRoute: () => rootRoute,
      path,
      component: () => <p data-testid="route-marker">{path}</p>,
    }),
  );
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: [options.initialPath ?? "/"] }),
    defaultPreload: false,
  });

  // Settle the first match before rendering, so the first paint is the route
  // rather than the router's pending state.
  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );

  return { ...result, router, queryClient };
}

/**
 * Mount the whole app — gate, rail, footer and page — at a path.
 *
 * Uses `createAppRouter`, not a copy of it, so a route that is missing from the
 * real tree is missing here too.
 */
export async function renderApp(options: HarnessOptions = {}): Promise<HarnessResult> {
  const queryClient = options.queryClient ?? createAppQueryClient();
  const router = createAppRouter(options.initialPath ?? "/");

  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );

  return { ...result, router, queryClient };
}
