import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { AnalyticsDays } from "./analytics";
import { keys } from "./keys";

/**
 * Context fit per tree node (spec `2026-11-12-encaixe-de-contexto-por-no` §2, §4): whether a node got
 * the right amount of context. Shape of `GET /analytics/context-fit?days=N`, written by hand like
 * `analytics.ts`. Global only: the route takes no project.
 */

/** `under` = too little context, `over` = too much, `split` = badly divided task, `fit` = right. */
export type FitVerdict = "under" | "over" | "split" | "fit";

export interface FitCounts {
  under: number;
  over: number;
  split: number;
  fit: number;
}

export interface FitLayerRow extends FitCounts {
  layer: string;
}

export interface FitEvidence {
  signal: string;
  value: number;
  threshold: number;
}

export interface FitNode {
  node_id: string;
  tree_id: string;
  layer: string;
  verdicts: FitVerdict[];
  evidence: FitEvidence[];
  given: Record<string, unknown>;
  used: Record<string, unknown>;
  rules_version: number;
  /** RFC3339. */
  computed_at: string;
}

export interface ContextFitReport {
  days: number;
  nodes: number;
  by_verdict: FitCounts;
  by_layer: FitLayerRow[];
  recent: FitNode[];
}

export function contextFitPath(days: AnalyticsDays): string {
  return `/analytics/context-fit?days=${days}`;
}

export function useContextFit(days: AnalyticsDays) {
  return useQuery({
    queryKey: keys.analytics.contextFit(days),
    queryFn: () => apiFetch<ContextFitReport>(contextFitPath(days)),
  });
}
