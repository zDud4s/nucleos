import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * The local llm-router's trail, as the runs surfaces read it.
 *
 * `routeTriple` is shared by the run page's route block and the list row, so the
 * two spell "what ran" the same way.
 */

/** `runner · model · effort`, skipping what is unknown; null when nothing is known. */
export function routeTriple(parts: Array<string | null | undefined>): string | null {
  const known = parts.filter((part): part is string => typeof part === "string" && part !== "");
  return known.length === 0 ? null : known.join(" · ");
}

/** One (ran, advised) pairing in `GET /route/report`. Strings are nullable, counts are not. */
export interface RoutePair {
  runner: string | null;
  model: string | null;
  effort: string | null;
  advised_runner: string | null;
  advised_model: string | null;
  advised_effort: string | null;
  runs: number;
  passed: number;
  failed: number;
}

/** `GET /route/report?days=N`. */
export interface RouteReport {
  days: number;
  runs: number;
  shadow: number;
  apply: number;
  advised: number;
  /** Shadow runs with usable advice — the match rate's denominator. */
  shadow_advised: number;
  matched: number;
  pairs: RoutePair[];
}

export const ROUTE_REPORT_DAYS = 30;

/**
 * The router report.
 *
 * `retry: false` and no refetch interval: an older daemon answers 404 and that
 * will not change in three seconds, and the panel that reads this renders
 * nothing on any failure, so retrying would only delay the page settling.
 */
export function useRouteReport(days: number = ROUTE_REPORT_DAYS) {
  return useQuery({
    queryKey: keys.runs.routeReport(days),
    queryFn: () => apiFetch<RouteReport>(`/route/report?days=${days}`),
    retry: false,
    refetchInterval: POLL.slow,
  });
}
