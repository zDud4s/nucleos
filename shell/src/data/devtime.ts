import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { AnalyticsDays, DayRange } from "./analytics";
import { keys } from "./keys";

/**
 * Development time (spec `2026-10-17-analytics-devtime`): the owner's own Claude Code sessions, read
 * from the devtime tables. Shapes are the core plan's "JSON contract (pinned)", written by hand like
 * `analytics.ts`. Never summed with phase 1's run cost — different populations.
 *
 * **`null` is absent, never zero** (a ratio without a denominator, a cost with nothing priced).
 */

export interface DevtimeQuality {
  ingest_enabled: boolean;
  last_cycle_at: string | null;
  unmapped_sessions_last_cycle: number | null;
  skipped_daemon_sessions_last_cycle: number | null;
  sessions: number;
  dirty_sessions: number;
  unruled_sessions: number;
}

export interface DevtimeByKind { model: number; tool: number; subagent: number; wait_background: number; wait_machine: number }
export interface DevtimeByWaste { useful: number; rework: number; avoidable: number }

export interface DevtimeDay {
  day: string;
  partial: boolean;
  active_ms: number;
  wasted_ms: number;
  by_kind: DevtimeByKind;
  by_waste: DevtimeByWaste;
  turns: number;
  interrupted: number;
  corrections: number;
  /** That UTC day's estimated cost; `null` when nothing that day could be priced (or no usage). */
  cost_usd: number | null;
}

export interface DevtimePreviousTotals {
  active_ms: number;
  wasted_ms: number;
  waste_ratio: number | null;
  interruption_rate: number | null;
  correction_rate: number | null;
}

export interface DevtimeTotals extends DevtimePreviousTotals {
  wait_human_ms: number;
  idle_ms: number;
  inferred_waste_ms: number;
  turns: number;
  correction_coverage: number | null;
  previous: DevtimePreviousTotals;
}

export interface RuleWaste { rule_id: string; level: string | null; waste: string | null; lever: string | null; ms: number; findings: number }
export interface LeverWaste { lever: string; ms: number }

export interface ModelTokens {
  model: string; effort: string;
  input: number; cache_read: number; cache_creation: number; output: number;
  messages: number; cost_usd: number | null;
  /** Bounds of `cost_usd` over cache writes of unknown duration (5-minute / 1-hour); equal when known. */
  cost_low: number | null; cost_high: number | null;
}

/** Outcome of the daemon's last price refresh (`POST /analytics/devtime/prices/refresh` answers 200 even when `ok` is false). */
export interface PriceRefreshStatus {
  /** RFC3339. */
  at: string;
  ok: boolean;
  prices_as_of: string | null;
  problems: string[];
}

export interface DevtimeTokens {
  by_model: ModelTokens[];
  cache_hit_ratio: number | null;
  /** The best estimate: cache writes of unknown duration priced as 1-hour, which Claude Code writes. */
  cost_usd: number | null;
  /** Bounds of `cost_usd`: unknown-duration cache writes all 5-minute (low) or all 1-hour (high). */
  cost_low: number | null; cost_high: number | null;
  cost_estimated: true;
  prices_as_of: string | null;
  unpriced_models: string[];
  /** `"<model id>: <reason>"` per price the daemon dropped as unreadable from nucleos-models.yaml. */
  dropped_prices: string[];
  /** The daemon's last price refresh; `null` when none ran since it started. */
  price_refresh: PriceRefreshStatus | null;
  previous: { cost_usd: number | null; cache_hit_ratio: number | null };
}

export interface DevtimeOverview {
  scope: "global" | "project";
  project_id: string | null;
  days: AnalyticsDays;
  timezone: "UTC";
  now: string;
  window: DayRange;
  previous: DayRange;
  quality: DevtimeQuality;
  days_series: DevtimeDay[];
  totals: DevtimeTotals;
  waste_by_rule: RuleWaste[];
  waste_by_lever: LeverWaste[];
  tokens: DevtimeTokens;
  /** Metric 7 (follow-up A). Absent from a daemon older than it: the contract adds keys, never changes them. */
  tool_failures?: DevtimeToolFailures;
  /** Metric 9 (follow-up B). Absent from a daemon older than it: the contract adds keys, never changes them. */
  turn_classes?: DevtimeTurnClasses;
  /** Metric 9 (follow-up B): slow turns no rule explains. Absent from an older daemon. */
  unexplained?: DevtimeUnexplained;
  /** Metric 10 (follow-up C). Absent from a daemon older than it: the contract adds keys, never changes them. */
  agents?: DevtimeAgents;
}

export interface ErrorClassFailures { error_class: string; failures: number }
export interface ToolFailureRow {
  tool_name: string; cmd_program: string | null;
  attempts: number; failures: number; failure_rate: number | null; top_error_class: string | null;
}
export interface DevtimeToolFailures {
  attempts: number; failures: number; failure_rate: number | null;
  interrupted: number; unresolved: number;
  by_error_class: ErrorClassFailures[]; by_tool: ToolFailureRow[];
}

export interface TurnClassRow {
  turn_class: string; min_calls: number | null; max_calls: number | null;
  turns: number; calls: number; active_ms: number; explained_ms: number;
  explained_ratio: number | null; median_active_ms: number; ms_per_call: number | null;
  unexplained_turns: number;
}
export interface DevtimeTurnClasses { turns: number; classes: TurnClassRow[] }
export interface UnexplainedCriteria {
  median_multiple: number; min_turn_ms: number; max_explained_fraction: number;
  min_class_turns: number; medians_from: string;
}
export interface UnexplainedItem {
  project_id: string; day: string; started_at: string; turn_class: string;
  calls: number; active_ms: number; explained_ms: number; class_median_ms: number;
}
export interface DevtimeUnexplained { turns: number; active_ms: number; criteria: UnexplainedCriteria; items: UnexplainedItem[] }

export interface AgentOutcomes { ok: number; failed: number; interrupted: number; other: number }
export interface AgentVerification { passed: number; failed: number; not_measured: number; unruled: number }
export interface AgentTypeRow {
  agent_type: string; role: string | null;
  attempts: number; ms: number; median_ms: number | null;
  outcome: AgentOutcomes; verified: AgentVerification;
  failure_rate: number | null; pass_rate: number | null;
}
export interface DevtimeAgents {
  attempts: number; ms: number;
  outcome: AgentOutcomes; verified: AgentVerification;
  failure_rate: number | null; pass_rate: number | null;
  by_type: AgentTypeRow[];
}

export interface RuleView {
  rule_id: string; level: string; n_cases: number; n_not_rework: number;
  precision: number | null; prior: number; eligible: boolean; low_precision: boolean;
}

export type Verdict = "confirmed" | "not_rework";
export interface FindingFeedback { verdict: Verdict; cause: string | null; marked_at: string }

export interface DevtimeFinding {
  finding_key: string; session_id: string; project_id: string; day: string;
  rule_id: string; scope: "session" | "cross"; level: string; waste: string; lever: string;
  confidence: "exact" | "inferred"; count: number; cost_ms: number;
  feedback: FindingFeedback | null;
  rule: RuleView | null;
}

/** Where the precision thresholds came from: the file, its last valid version, or the built-in defaults. */
export type ThresholdsSource = "file" | "last_good" | "default";
export interface DevtimeFindings {
  items: DevtimeFinding[]; next_cursor: string | null; rules: RuleView[]; thresholds_source: ThresholdsSource;
}
export interface MarkOutcome { finding_key: string; feedback: FindingFeedback; rule: RuleView | null }

export const FINDINGS_PAGE = 50;
export type FindingsOrder = "cost" | "recent";
export interface FindingsQuery { order?: FindingsOrder; cursor?: string; ruleId?: string }

function scoped(projectId: string | undefined): string {
  return projectId === undefined ? "/analytics/devtime" : `/projects/${encodeURIComponent(projectId)}/analytics/devtime`;
}

export function devtimePath(projectId: string | undefined, days: AnalyticsDays): string {
  return `${scoped(projectId)}?days=${days}`;
}

export function devtimeFindingsPath(projectId: string | undefined, days: AnalyticsDays, query: FindingsQuery): string {
  const params = new URLSearchParams({ days: String(days), order: query.order ?? "cost", limit: String(FINDINGS_PAGE) });
  if (query.ruleId !== undefined) params.set("rule_id", query.ruleId);
  if (query.cursor !== undefined) params.set("cursor", query.cursor);
  return `${scoped(projectId)}/findings?${params.toString()}`;
}

/** Global: a finding's key names it everywhere (spec §5.3). `days` only picks the precision window. */
export function devtimeMarkPath(findingKey: string, days: AnalyticsDays): string {
  return `/analytics/devtime/findings/${encodeURIComponent(findingKey)}/mark?days=${days}`;
}

export const DEVTIME_PRICES_REFRESH_PATH = "/analytics/devtime/prices/refresh";

export function useDevtime(projectId: string | undefined, days: AnalyticsDays) {
  return useQuery({
    queryKey: projectId === undefined ? keys.analytics.devtime(days) : keys.projects.devtime(projectId, days),
    queryFn: () => apiFetch<DevtimeOverview>(devtimePath(projectId, days)),
  });
}

/** Asked only while the findings disclosure is open (`enabled`), spec §4.6. */
export function useDevtimeFindings(projectId: string | undefined, days: AnalyticsDays, query: FindingsQuery, enabled: boolean) {
  return useQuery({
    queryKey:
      projectId === undefined
        ? keys.analytics.devtimeFindings(days, query.order ?? "cost", query.cursor ?? "")
        : keys.projects.devtimeFindings(projectId, days, query.order ?? "cost", query.cursor ?? ""),
    queryFn: () => apiFetch<DevtimeFindings>(devtimeFindingsPath(projectId, days, query)),
    enabled,
  });
}

export function useMarkFinding(days: AnalyticsDays) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ findingKey, verdict, cause }: { findingKey: string; verdict: Verdict; cause: string | null }) =>
      apiFetch<MarkOutcome>(devtimeMarkPath(findingKey, days), { method: "POST", body: JSON.stringify({ verdict, cause }) }),
    retry: false,
    // Another finding of the same rule shows the same precision: re-read every findings page.
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["analytics", "devtime-findings"] });
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}

/** Runs a price refresh in the daemon now, then re-reads every devtime overview (the cost depends on the prices). */
export function useRefreshPrices() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => apiFetch<PriceRefreshStatus>(DEVTIME_PRICES_REFRESH_PATH, { method: "POST" }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["analytics", "devtime"] });
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}
