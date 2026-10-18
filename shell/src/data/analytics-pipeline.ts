import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import type { AnalyticsDays, DayRange } from "./analytics";

/**
 * Pipeline analytics (spec `analytics-pipeline` §5.2): landing, gate, post-gate, jobs, autopilot and
 * slot pressure, global or for one project, in one answer. The shapes are the núcleo's as built
 * (`core/src/analytics_pipeline/*`), written by hand like `analytics.ts`.
 *
 * **`null` is absent, never zero** — a ratio with nothing measured, a median with no sample. Counts
 * that are zero are real zeros. A block that could not be computed arrives as `{ error }` instead
 * of its object (D5): test it with `isBlockError` before reading it.
 *
 * Read when the view opens and not polled: a window does not move between two ticks.
 */

/** A block that could not be computed. The only code today is `query_failed`. */
export interface BlockError {
  error: string;
}
export type Block<T> = T | BlockError;

export function isBlockError(block: unknown): block is BlockError {
  return typeof block === "object" && block !== null && "error" in block;
}

export function pipelinePath(projectId: string | undefined, days: AnalyticsDays | number): string {
  return projectId === undefined
    ? `/analytics/pipeline?days=${days}`
    : `/projects/${encodeURIComponent(projectId)}/analytics/pipeline?days=${days}`;
}

/** Percentiles of a duration; both `null` when nothing was sampled, `p95` also below five samples. */
export interface Dist {
  p50: number | null;
  p95: number | null;
  n: number;
}

export interface StatusMix {
  succeeded: number;
  failed: number;
  escalated: number;
  blocked: number;
  cancelled: number;
  other: number;
}

export interface FailureBuckets {
  conflict: number;
  gate: number;
  blocked: number;
  interrupted: number;
  other: number;
}

export interface LandingDay {
  day: string;
  partial: boolean;
  total: number;
  bad: number;
}

export interface WorkLost {
  nothing: number;
  lost: number;
}

export interface Conflicts {
  count: number;
  rate: number | null;
  /** Settled `resolved` through a landing the daemon filed, no person's verdict; counted only from when landings record their origin. */
  auto_resolved: number;
  /** Settled merged, superseded, or resolved but not by the daemon alone. */
  settled_otherwise: number;
  work_lost: WorkLost;
}

export interface OwnerBlocked {
  count: number;
  oldest_age_s: number | null;
  median_age_s: number | null;
}

export interface Landing {
  total: number;
  prev_total: number;
  wait_s: Dist;
  wait_s_machine: Dist;
  run_s: Dist;
  status_mix: StatusMix;
  unexecutable: number;
  bad_share: number | null;
  prev_bad_share: number | null;
  failure_buckets: FailureBuckets;
  /** Exactly `days` points. */
  daily: LandingDay[];
  /** `null` while nothing is recorded to measure it from. */
  conflicts: Conflicts | null;
  owner_blocked: OwnerBlocked | null;
}

export interface DurationDay {
  day: string;
  partial: boolean;
  p50: number | null;
  n: number;
}

export interface RedGroup {
  group: string;
  red: number;
  terminal: number;
  rate: number | null;
}

export interface FlakyGroup {
  project_id: string;
  group: string;
  observed: number;
  flaky: number;
  rate: number | null;
  suspect: boolean;
}

export interface GateCache {
  hit_rate: number | null;
  prev_hit_rate: number | null;
}

export interface GateBypass {
  observed: number;
  verify_units: number;
  ratio: number | null;
}

export interface Gate {
  pass_rate: number | null;
  prev_pass_rate: number | null;
  terminal: number;
  first_pass_rate: number | null;
  retry_share: number | null;
  /** Exactly `days` points. */
  duration_ms_daily: DurationDay[];
  /** At most five. */
  most_red: RedGroup[];
  legacy_runs: number;
  /** At most ten. */
  flaky: FlakyGroup[];
  cache: GateCache | null;
  bypass: GateBypass | null;
}

export interface ProjectRed {
  project_id: string;
  red_now: boolean;
  red_s: number;
}

export interface RedDay {
  day: string;
  partial: boolean;
  red_s: number;
}

export interface Postgate {
  red_now: boolean;
  red_since: string | null;
  /** `flake_check` or `bisect` while red, `null` otherwise; kept a string so a new phase still parses. */
  red_phase: string | null;
  red_groups: string[];
  culprit_sha: string | null;
  red_s_in_window: number;
  prev_red_s: number;
  red_events: number;
  reverts: number;
  per_project: ProjectRed[];
  /** Exactly `days` points. */
  red_s_daily: RedDay[];
}

export interface WaitingReasons {
  slot: number;
  budget: number;
  quota: number;
  disk: number;
  excluded: number;
  "kill-switch": number;
  attention: number;
  other: number;
}

export interface StuckJob {
  id: number;
  project_id: string;
}

export interface Stuck {
  age_over_lifetime: number;
  dry_rounds_at_limit: number;
  rounds_exhausted: number;
  gate_retry_loop: number;
  item_stuck: number;
  /** At most ten. */
  jobs: StuckJob[];
}

/** Jobs held back for a reason; they are not stuck. */
export interface Waiting {
  by_reason: WaitingReasons;
  /** At most ten. */
  jobs: StuckJob[];
}

export interface JobDay {
  day: string;
  partial: boolean;
  created: number;
  completed: number;
}

export interface Jobs {
  total: number;
  completed: number;
  failed: number;
  interrupted: number;
  live: number;
  completion_rate: number | null;
  prev_completion_rate: number | null;
  duration_s: Dist;
  /** Exactly `days` points. */
  daily: JobDay[];
  stuck: Stuck;
  waiting: Waiting;
}

export interface ClassAgreement {
  action_class: string;
  n: number;
  agree: number | null;
  pending: number;
}

export interface JudgeBlock {
  n: number;
  reviewed: number;
  latency_ms: Dist;
  input_tokens: number;
  cost_usd: number;
  error_rate: number | null;
  enforced_share: number | null;
  agree: number | null;
}

export interface Refused {
  total: number;
  by_status: Record<string, number>;
}

export interface Autopilot {
  classifier: ClassAgreement[];
  judge: JudgeBlock;
  refused_actions: Refused;
}

export interface P95Day {
  day: string;
  partial: boolean;
  in_use: number | null;
}

export interface PressureDay {
  day: string;
  partial: boolean;
  avg_in_use: number | null;
  max_in_use: number | null;
}

export interface Pressure {
  at_limit_share: number | null;
  prev_at_limit_share: number | null;
  peak_in_use: number | null;
  prev_peak_in_use: number | null;
  limit: number | null;
  /** `null` when the raw samples that feed it have all been rolled up. */
  daily_p95: P95Day[] | null;
  daily: PressureDay[];
  slot_wait_share: number | null;
  slot_wait_share_is_lower_bound: boolean;
}

/** Since when the two new records hold data; `null` before the first row. */
export interface PipelineHistory {
  postgate_since: string | null;
  slots_since: string | null;
}

export interface PipelineReport {
  scope: "global" | "project";
  project_id: string | null;
  days: number;
  timezone: "UTC";
  /** RFC3339; the end of the time axis. */
  now: string;
  window: DayRange;
  previous: DayRange;
  history: PipelineHistory;
  landing: Block<Landing>;
  gate: Block<Gate>;
  postgate: Block<Postgate>;
  jobs: Block<Jobs>;
  autopilot: Block<Autopilot>;
  /** Always sent by the núcleo now; `null` only in a fixture standing for an older answer. */
  pressure: Block<Pressure> | null;
}

export function usePipelineAnalytics(projectId: string | undefined, days: AnalyticsDays) {
  return useQuery({
    queryKey:
      projectId === undefined ? keys.analytics.pipeline(days) : keys.projects.analyticsPipeline(projectId, days),
    queryFn: () => apiFetch<PipelineReport>(pipelinePath(projectId, days)),
  });
}
