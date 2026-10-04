import type { BadgeTone } from "./Badge";
import { readState } from "./state-map";

/**
 * Where a feed line sits on the Feed's trace, and how much weight it carries.
 *
 * Two readings the Feed draws from a kind, kept beside the map rather than inside a page because
 * both are claims about núcleo kinds — the same reason `state-map.ts` exists — and because the
 * page may not hold a tone of its own (`badge-authorship.test.ts`). The tone still comes from the
 * one map; this module only says what the Feed DOES with it.
 *
 * **Lanes are subsystems, not severities.** Six, in the order a reader goes looking: the work the
 * núcleo was asked for (jobs), the processes doing it (runs and their worktrees), what reached the
 * repository (git), the multi-agent pillars (teams and the council), the outside world (schedules,
 * mail and the web), and the machine and its projects' own settings. A lane is where a
 * line came FROM; its gravity is what the tone already says.
 */

export type FeedLane = "jobs" | "runs" | "git" | "teams" | "mail" | "machine";

export interface FeedLaneInfo {
  id: FeedLane;
  /** The lane's name on the trace. */
  label: string;
}

export const FEED_LANES: readonly FeedLaneInfo[] = [
  { id: "jobs", label: "Jobs" },
  { id: "runs", label: "Runs & worktrees" },
  { id: "git", label: "Git" },
  { id: "teams", label: "Teams & council" },
  { id: "mail", label: "Mail & web" },
  { id: "machine", label: "Projects & machine" },
];

/**
 * Every kind the map reads, placed in a lane.
 *
 * Exhaustive over `statesOf("feed")`, and `lanes.test.ts` holds it so in both directions: a
 * kind added to the map without a lane fails there, rather than landing silently in the machine
 * lane. The borderline calls, and why:
 *
 * - `resume_did_not_act` is about an APPROVAL the núcleo never acted on, not about the run that
 *   carried it — a governance fact, so the machine lane beside `action_authorized`.
 * - `token_efficiency` names runs but judges a project's prompts over many of them; it is advice
 *   about configuration, so the machine lane, and not runs.
 * - `land_resolution_failed` is git: what failed was landing a resolution on the branch.
 * - `schedule_rule_invalid` sits in the mail lane, with the other lines about the outside world.
 * - `promotion_ready` is a project earning the next autopilot mode — the machine lane.
 */
const LANE_OF: Record<string, FeedLane> = {
  job_started: "jobs",
  job_planned: "jobs",
  job_replanned: "jobs",
  job_plan_failed: "jobs",
  job_item_failed: "jobs",
  job_item_conflicted: "jobs",
  job_item_orphaned: "jobs",
  job_gate_failed: "jobs",
  job_review_skipped: "jobs",
  job_review_retried: "jobs",
  job_waiting: "jobs",
  job_finished: "jobs",
  job_failed: "jobs",
  job_stopped: "jobs",
  job_cancelled: "jobs",
  job_expired: "jobs",
  job_interrupted: "jobs",

  run_retry: "runs",
  run_failed_final: "runs",
  run_interrupted: "runs",
  run_stopped_probing: "runs",
  run_stopped_by_judge: "runs",
  shadow_run_completed: "runs",
  worktree_run_completed: "runs",
  worktree_gate_failed: "runs",
  worktree_provision_failed: "runs",
  worktree_workflow_missing: "runs",
  worktree_released: "runs",
  worktree_branch_kept: "runs",
  worktree_removed: "runs",
  worktree_gc_failed: "runs",
  judge_needs_owner: "runs",
  judge_correction_started: "runs",
  judge_correction_failed: "runs",

  vcs_request_finished: "git",
  vcs_request_cancelled: "git",
  vcs_request_interrupted: "git",
  vcs_request_settled: "git",
  vcs_resolution_started: "git",
  vcs_resolution_cancelled: "git",
  vcs_resolution_discarded: "git",
  land_resolution_failed: "git",

  team_run_started: "teams",
  team_run_finished: "teams",
  team_item_dropped: "teams",
  team_action: "teams",
  team_trigger_armed: "teams",
  team_trigger_skipped: "teams",
  council_started: "teams",
  council_stage: "teams",
  council_finished: "teams",

  schedule_rule_invalid: "mail",
  email_digest: "mail",
  email_urgent: "mail",
  email_triage_failed: "mail",
  email_triage_paused: "mail",
  email_triage_stalled: "mail",
  email_fetch_skipped: "mail",
  email_sent_mailbox_foreign: "mail",
  "web.read": "mail",

  config_written: "machine",
  project_onboarded: "machine",
  health_breach_intent: "machine",
  workflow_changed: "machine",
  command_finished: "machine",
  action_authorized: "machine",
  proposal_record_failed: "machine",
  promotion_ready: "machine",
  judge_demoted: "machine",
  token_efficiency: "machine",
  // The machine's own ceiling, like the budget: it is about what this laptop may still spend, not
  // about any one job — the burn it reports was made by all of them at once.
  quota_warning: "machine",
  judge_resolve_demoted: "machine",
  quota_blind: "machine",
  resume_did_not_act: "machine",
  secret_stored: "machine",
  secret_forgotten: "machine",
};

/** The kinds this module places by name. For the completeness test. */
export function feedLaneKinds(): string[] {
  return Object.keys(LANE_OF);
}

/**
 * The lane a kind is drawn in.
 *
 * A kind the map does not read still has to be drawn somewhere, and it goes to the machine lane —
 * the one place for lines nobody has classified — with one exception by prefix: `email_<class>`
 * is built from a project's configured notify classes (`triage.rs`) and `web.*` from the web
 * pillar, so an unknown member of either family is still mail or the web, and the lane can say so
 * without the badge pretending to know the class.
 */
export function feedLaneOf(kind: string): FeedLane {
  const named = LANE_OF[kind];
  if (named !== undefined) return named;
  if (kind.startsWith("email_") || kind.startsWith("web.")) return "mail";
  return "machine";
}

/**
 * How much a line weighs on the Feed: went wrong, held, asks for you, or routine.
 *
 * Read off the map's tone and nothing else, so the Feed can never disagree with a badge about a
 * line: Wrong Red went wrong, Held Ember was held, Awaiting-You Amber asks something of the
 * reader, and every other tone — a fact, a shadow decision, something switched off — is routine.
 *
 * `job_waiting` is the one kind the owner named: routine unless its wait is an approval. No park
 * reason the núcleo writes is one (see the row's comment in `state-map.ts`), and the row is Stated
 * Blue, so the rule falls out of the tone rather than being a second special case here. An
 * unmapped kind is routine: the shell does not know it, and weight would be a claim.
 */
export type FeedGravity = "wrong" | "held" | "asks" | "routine";

export function feedGravityOf(kind: string): FeedGravity {
  const tone = readState("feed", kind)?.tone;
  if (tone === "danger") return "wrong";
  if (tone === "paused") return "held";
  if (tone === "pending") return "asks";
  return "routine";
}

/** The tone a mark is drawn in: the map's, or Switched Off Grey for a kind it cannot read. */
export function feedMarkTone(kind: string): BadgeTone {
  return readState("feed", kind)?.tone ?? "off";
}

/** The tone each exceptional gravity is drawn in — for the key beside the verdict's counts. */
export function feedGravityTone(gravity: Exclude<FeedGravity, "routine">): BadgeTone {
  if (gravity === "wrong") return "danger";
  if (gravity === "held") return "paused";
  return "pending";
}

/**
 * The kinds that leave a sequence still going when they are the last thing it said.
 *
 * A trace row is a sequence of lines about one subject — a job, a run, a council — and it is
 * drawn open (a dashed ghost to now, "still open" in the header) when its newest line is one of
 * these. Explicit rather than inferred from a tone, because "not finished" is not a severity: all
 * of them read Stated Blue, and so do `job_finished` and `council_finished`, which close.
 *
 * - `job_waiting` is parked by a brake and will be picked up again; `run_retry` is an attempt
 *   that failed with another one coming, and `job_review_retried` is a review that will run again.
 * - The starts and middles — a job started, planned or replanned, a team run or a council
 *   started, a council stage, a conflict resolution started — are open only because nothing has
 *   been written after them yet; the next line from the same subject closes or continues them.
 * - a correction started (`judge_correction_started`) is open until its own run ends it —
 *   `worktree_run_completed` or `judge_correction_failed` (spec .ai/specs/2026-09-27-autopilot-juiz-resolve-bloqueios-design.md, D7).
 */
const OPEN_KINDS: ReadonlySet<string> = new Set([
  "job_started",
  "job_planned",
  "job_replanned",
  "job_waiting",
  "job_review_retried",
  "run_retry",
  "team_run_started",
  "council_started",
  "council_stage",
  "vcs_resolution_started",
  "judge_correction_started",
]);

/** The kinds {@link feedKindLeavesOpen} names. For the completeness test. */
export function feedOpenKinds(): string[] {
  return [...OPEN_KINDS];
}

/** Whether a sequence whose newest line is this kind is still going. */
export function feedKindLeavesOpen(kind: string): boolean {
  return OPEN_KINDS.has(kind);
}
