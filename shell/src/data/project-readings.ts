import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The four readings a project's State mode leads with.
 *
 * One query, because the núcleo answers all four from one walk of one table — they are aggregations
 * over the same rows in the same window. Four hooks would be four requests for one panel.
 *
 * **Every number here can be absent, and absent is never zero.** That is not defensive typing: a
 * project too new to have thirty days behind it, a daemon that reports no token usage, a project
 * with no gate command — all three are ordinary, and all three produce a reading nobody took. The
 * page says so in words.
 */

/** How hard the project is working for its tokens. */
export interface Efficiency {
  /** Sessions that reported all three token counts. Sessions, not runs: a resumed run is one. */
  measured_runs: number;
  /**
   * Runs that reported nothing at all.
   *
   * Its own number and never inside the median's denominator. A Codex or local-model daemon reports
   * no usage, and counting that silence as zero would drag the middle down until every cloud run
   * looked like a regression.
   */
  unmeasured_runs: number;
  median_total_tokens: number | null;
  /** The same statistic over the window before this one, so a number has something to move against. */
  previous_median_total_tokens: number | null;
}

export interface Cost {
  usd: number;
  /** Runs behind the figure, so a large number standing on three runs cannot read as a trend. */
  runs: number;
}

/**
 * The daemon's own verification, in four numbers that must stay four.
 *
 * `failed` means the code is broken. `errored` means the measurement broke and says nothing about
 * the code. `no_gate` means this project never defined green, so nothing was asked. Merging any two
 * of them tells somebody to go and fix the wrong thing.
 */
export interface GateTally {
  passed: number;
  failed: number;
  errored: number;
  no_gate: number;
}

export interface Delivered {
  landed: number;
  /** How many of those could be timed — a merge asked for by hand has no run to measure from. */
  timed: number;
  median_minutes: number | null;
}

export interface ProjectReadings {
  window_days: number;
  efficiency: Efficiency;
  cost: Cost;
  gate: GateTally;
  delivered: Delivered;
}

/**
 * Read once when the mode opens, and not polled.
 *
 * A thirty-day aggregate does not move between two ticks of a three-second timer, and putting it on
 * one would be a table walk per tick for a number that changes when a run ends. The live parts of
 * this page — the queue, the slots — already poll on their own routes.
 */
export function useProjectReadings(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.readings(projectId ?? ""),
    queryFn: () =>
      apiFetch<ProjectReadings>(`/projects/${encodeURIComponent(projectId ?? "")}/readings`),
    enabled: projectId !== null && projectId !== "",
  });
}

/**
 * How the gate tally divides, as fractions of the runs that were actually judged.
 *
 * `no_gate` is deliberately outside the denominator. A project with fifty ungated runs and two
 * failures is not 96% green — it is two failures out of two measurements, and the fifty are a
 * separate fact to state rather than a cushion to hide behind.
 */
export function gateShare(gate: GateTally): {
  judged: number;
  passed: number;
  failed: number;
  errored: number;
} {
  const judged = gate.passed + gate.failed + gate.errored;
  if (judged === 0) return { judged: 0, passed: 0, failed: 0, errored: 0 };
  return {
    judged,
    passed: gate.passed / judged,
    failed: gate.failed / judged,
    errored: gate.errored / judged,
  };
}

/**
 * Which way the efficiency went, or nothing when there is nothing to compare.
 *
 * Fewer tokens is better, so a drop is `improved`. Named rather than returned as a signed number
 * because the caller would otherwise have to remember which direction is good, and that is exactly
 * the kind of thing a page gets backwards once and keeps backwards.
 */
export function efficiencyTrend(
  efficiency: Efficiency,
): "improved" | "worsened" | "level" | "unknown" {
  const { median_total_tokens: now, previous_median_total_tokens: before } = efficiency;
  if (now === null || before === null || before === 0) return "unknown";
  const change = (now - before) / before;
  // A tenth either way is noise on a median of a few dozen runs, and a page that announced a
  // direction for every wobble would be announcing something every day.
  if (Math.abs(change) < 0.1) return "level";
  return change < 0 ? "improved" : "worsened";
}

/** A token count at a glance. Full precision belongs in a tooltip, not in a headline. */
export function compactTokens(total: number): string {
  if (total >= 1_000_000) return `${(total / 1_000_000).toFixed(1)}M`;
  if (total >= 1_000) return `${Math.round(total / 1_000)}k`;
  return String(total);
}

/** Minutes as something a person says out loud. */
export function humanMinutes(minutes: number): string {
  if (minutes < 60) return `${minutes} min`;
  const hours = minutes / 60;
  if (hours < 24) return `${hours.toFixed(hours < 10 ? 1 : 0)} h`;
  return `${(hours / 24).toFixed(1)} d`;
}
