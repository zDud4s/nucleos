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
}

export interface DevtimeTokens {
  by_model: ModelTokens[];
  cache_hit_ratio: number | null;
  cost_usd: number | null;
  cost_estimated: true;
  prices_as_of: string | null;
  unpriced_models: string[];
  /** `"<model id>: <reason>"` per price the daemon dropped as unreadable from nucleos-models.yaml. */
  dropped_prices: string[];
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

export interface DevtimeFindings { items: DevtimeFinding[]; next_cursor: string | null; rules: RuleView[] }
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
