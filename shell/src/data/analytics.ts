import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import type { Delivered } from "./project-readings";

/**
 * Analytics, phase 1 (spec `2026-10-17-analytics-fundacao`): daily series and window totals, global
 * or for one project, plus the quota history. The shapes are §6's, written by hand like
 * `project-readings.ts`.
 *
 * **`null` is absent, never zero** — a cache ratio with nothing measured, a median with no timed
 * merge. Counts that are zero are real zeros: a day with no runs is a day with no runs (§4.1).
 *
 * Read when the view opens and not polled: a 30-day series does not move between two ticks.
 */

/** The only windows the routes accept; anything else is a 400 (§4.1), so the shell never sends one. */
export const WINDOWS = [7, 30, 90] as const;
export type AnalyticsDays = (typeof WINDOWS)[number];
export const DEFAULT_DAYS: AnalyticsDays = 30;

/** A `?days=` value as one of the three windows, or `undefined` — never clamped, as the route never clamps. */
export function parseDays(raw: unknown): AnalyticsDays | undefined {
  const asked =
    typeof raw === "number" ? raw : typeof raw === "string" && /^\d+$/.test(raw) ? Number(raw) : Number.NaN;
  return (WINDOWS as readonly number[]).includes(asked) ? (asked as AnalyticsDays) : undefined;
}

export function analyticsPath(projectId: string | undefined, days: AnalyticsDays): string {
  return projectId === undefined
    ? `/analytics?days=${days}`
    : `/projects/${encodeURIComponent(projectId)}/analytics?days=${days}`;
}

export function quotaPath(days: AnalyticsDays): string {
  return `/analytics/quota?days=${days}`;
}

/** Inclusive UTC days, `YYYY-MM-DD`. */
export interface DayRange {
  from: string;
  to: string;
}

export interface CostTotal {
  measured_usd: number;
  /** Runs with no reported cost, estimated from elapsed time. Shown apart, never summed silently (§4.4). */
  estimated_usd: number;
  /** The judge's spend (§4.9), by the verdict's own `created_at`. */
  judge_usd: number;
  runs: number;
  estimated_runs: number;
}

export interface CostPart {
  /** A `project_id` (global), a `mode` (project), or a reserved key: `judge`, `__other__`, `__none__`. */
  key: string;
  measured_usd: number;
  /** Always `0` on the `judge` part. */
  estimated_usd: number;
}

export interface CostPoint {
  day: string;
  partial: boolean;
  /** One part per `CostBlock.keys` entry, in the same order, zeros included. */
  by: CostPart[];
}

export interface SeriesKey {
  key: string;
  /** Display text, from the núcleo: `Judge`, `Other projects`, `No project`, else the key itself. */
  label: string;
}

export interface CostBlock {
  total: CostTotal;
  previous: CostTotal;
  /** Exactly `days` points. */
  series: CostPoint[];
  by_label: "project" | "mode";
  /** Stable order: cost desc, then key; global at most 8 projects + `__other__`; `judge` always last (both scopes). */
  keys: SeriesKey[];
}

export interface TokenTotal {
  input: number;
  cache_read: number;
  cache_creation: number;
  output: number;
  measured_sessions: number;
  /** Runs that reported no usage — counted here, never as zero tokens. */
  unmeasured_runs: number;
  cache_hit_ratio: number | null;
}

export interface TokenPoint {
  day: string;
  partial: boolean;
  input: number;
  cache_read: number;
  cache_creation: number;
  output: number;
}

export interface TokenBlock {
  total: TokenTotal;
  previous: TokenTotal;
  series: TokenPoint[];
}

/** The four gate literals `GateTally` keeps apart, plus `other` so the sum is always `runs_completed` (§4.6). */
export interface ThroughputTotal {
  runs_completed: number;
  passed: number;
  failed: number;
  errored: number;
  no_gate: number;
  other: number;
}

export interface ThroughputPoint {
  day: string;
  partial: boolean;
  passed: number;
  failed: number;
  errored: number;
  no_gate: number;
  other: number;
}

export interface ModeCount {
  mode: string;
  runs_completed: number;
}

export interface ThroughputBlock {
  total: ThroughputTotal;
  previous: ThroughputTotal;
  series: ThroughputPoint[];
  /** Runs desc, then mode. Unknown modes arrive raw and are shown raw (§4.5). */
  by_mode: ModeCount[];
  in_flight: number;
}

export interface DeliveredPoint {
  day: string;
  partial: boolean;
  landed: number;
}

export interface DeliveredBlock {
  /** Same contract as the State mode's readings: `{landed, timed, median_minutes}`. */
  total: Delivered;
  previous: Delivered;
  series: DeliveredPoint[];
}

export interface TopRun {
  /** The session's last run — an integer, linked as `/runs/$runId`. */
  run_id: number;
  /** `null` for a run that belongs to no project. */
  project_id: string | null;
  mode: string;
  completed_at: string;
  cost_usd: number;
  cost_is_estimated: boolean;
}

export interface AnalyticsReport {
  scope: "global" | "project";
  project_id: string | null;
  days: number;
  timezone: "UTC";
  /** RFC3339; the end of the time axis. */
  now: string;
  window: DayRange;
  previous: DayRange;
  counted: { modes: "all" };
  cost: CostBlock;
  tokens: TokenBlock;
  throughput: ThroughputBlock;
  delivered: DeliveredBlock;
  /** At most ten, ordered as §4.8. */
  top_runs: TopRun[];
}

export interface QuotaHistoryPoint {
  at: string;
  /** In [0, 1] — a fraction, never a percentage (the `quota.ts` rule). */
  used_fraction: number;
  resets_at: string | null;
  stale: boolean;
}

export interface QuotaHistorySeries {
  provider: string;
  window: "5h" | "7d";
  fidelity: "official" | "derived";
  /** Ascending by `at`. */
  points: QuotaHistoryPoint[];
}

export interface QuotaHistory {
  days: number;
  timezone: "UTC";
  /** When the history began; `null` before the first recorded reading. */
  since: string | null;
  series: QuotaHistorySeries[];
}

export function useAnalytics(projectId: string | undefined, days: AnalyticsDays) {
  return useQuery({
    queryKey:
      projectId === undefined ? keys.analytics.global(days) : keys.projects.analytics(projectId, days),
    queryFn: () => apiFetch<AnalyticsReport>(analyticsPath(projectId, days)),
  });
}

/** Global only (§9.8); `enabled` is false in a project's view so the route is never asked there. */
export function useQuotaHistory(days: AnalyticsDays, enabled: boolean) {
  return useQuery({
    queryKey: keys.analytics.quota(days),
    queryFn: () => apiFetch<QuotaHistory>(quotaPath(days)),
    enabled,
  });
}

/**
 * The `/analytics` page's search params: `?days=` when it is a window the route serves, nothing
 * otherwise. Here rather than in the page so `router.tsx` can import it without loading the page.
 */
export function validateAnalyticsSearch(search: Record<string, unknown>): { days?: AnalyticsDays } {
  const days = parseDays(search.days);
  return days === undefined ? {} : { days };
}
