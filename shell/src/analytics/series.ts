import type {
  CostBlock,
  DeliveredBlock,
  QuotaHistory,
  ThroughputBlock,
  ThroughputTotal,
  TokenBlock,
  TokenTotal,
} from "../data/analytics";
import type { ChartDatum, ChartKey, ShareItem, StepSeries } from "../ui";

/**
 * The §6 blocks as chart props, and the words a card says about a change. Pure, so the arithmetic
 * that decides what a chart claims is tested without rendering one.
 */

/** The client-side series for cost estimated from run time (§4.4: shown apart, never summed silently). */
export const ESTIMATED_KEY = "__estimated__";

/** Reserved keys with a tone of their own; never one of the eight categorical colours. */
const RESERVED_CLASS: Record<string, string> = {
  judge: "ui-chart-judge",
  __other__: "ui-chart-other",
};

/**
 * The legend, in the daemon's order (`judge` last), labels as the daemon wrote them — `"Judge"`,
 * `"Other projects"`, `"No project"` are its words, not the shell's — and the estimate appended.
 */
export function costKeys(cost: CostBlock): ChartKey[] {
  const keys = cost.keys.map((key): ChartKey => {
    const className = RESERVED_CLASS[key.key];
    return className === undefined ? { key: key.key, label: key.label } : { key: key.key, label: key.label, className };
  });
  const estimated =
    cost.total.estimated_runs > 0 || cost.series.some((point) => point.by.some((part) => part.estimated_usd > 0));
  return estimated
    ? [...keys, { key: ESTIMATED_KEY, label: "Estimated from run time", className: "ui-chart-estimated" }]
    : keys;
}

/** `by` carries one part per `keys[]` entry, in order, zeros included; the estimates of all of them sum into one part. */
export function costBars(cost: CostBlock): ChartDatum[] {
  return cost.series.map((point) => ({
    day: point.day,
    partial: point.partial,
    parts: [
      ...point.by.map((part) => ({ key: part.key, value: part.measured_usd })),
      { key: ESTIMATED_KEY, value: point.by.reduce((sum, part) => sum + part.estimated_usd, 0) },
    ],
  }));
}

export const TOKEN_KEYS: ChartKey[] = [
  { key: "input", label: "Input" },
  { key: "cache_read", label: "Cache read" },
  { key: "cache_creation", label: "Cache write" },
  { key: "output", label: "Output" },
];

export function tokenBars(tokens: TokenBlock): ChartDatum[] {
  return tokens.series.map((point) => ({
    day: point.day,
    partial: point.partial,
    parts: [
      { key: "input", value: point.input },
      { key: "cache_read", value: point.cache_read },
      { key: "cache_creation", value: point.cache_creation },
      { key: "output", value: point.output },
    ],
  }));
}

export function totalTokens(total: TokenTotal): number {
  return total.input + total.cache_read + total.cache_creation + total.output;
}

/** The four `GateTally` literals plus `other`, never merged (§4.6), in the tones `ModeState` gives them. */
export const GATE_KEYS: ChartKey[] = [
  { key: "passed", label: "Passed", className: "ui-chart-passed" },
  { key: "failed", label: "Failed", className: "ui-chart-failed" },
  { key: "errored", label: "Could not run", className: "ui-chart-errored" },
  { key: "no_gate", label: "No gate", className: "ui-chart-ungated" },
  { key: "other", label: "Other status", className: "ui-chart-unknown" },
];

export function gateBars(throughput: ThroughputBlock): ChartDatum[] {
  return throughput.series.map((point) => ({
    day: point.day,
    partial: point.partial,
    parts: [
      { key: "passed", value: point.passed },
      { key: "failed", value: point.failed },
      { key: "errored", value: point.errored },
      { key: "no_gate", value: point.no_gate },
      { key: "other", value: point.other },
    ],
  }));
}

/** `passed / (passed + failed + errored)`; `no_gate` and `other` are outside, as in `gateShare`. `null` when nothing was judged. */
export function passRate(total: ThroughputTotal): number | null {
  const judged = total.passed + total.failed + total.errored;
  return judged === 0 ? null : total.passed / judged;
}

export const DELIVERED_KEYS: ChartKey[] = [{ key: "landed", label: "Landed so far" }];

export function deliveredBars(delivered: DeliveredBlock): ChartDatum[] {
  return delivered.series.map((point) => ({
    day: point.day,
    partial: point.partial,
    parts: [{ key: "landed", value: point.landed }],
  }));
}

/**
 * Each day's parts summed, then summed again over the days before it: a running total under one
 * `key`. Delivered is charted this way because merges are rare — per day it is a row of 0s and 1s
 * that says nothing, while the running total says how the window went. `partial` is kept, so
 * today's segment still reads as not over.
 */
export function runningTotal(data: ChartDatum[], key: string): ChartDatum[] {
  let sum = 0;
  return data.map((datum) => {
    sum += datum.parts.reduce((total, part) => total + part.value, 0);
    const day: ChartDatum = { day: datum.day, parts: [{ key, value: sum }] };
    return datum.partial === undefined ? day : { ...day, partial: datum.partial };
  });
}

/**
 * The window's total per key, in `keys` order, as `ShareBar` items: the chart's legend, with the
 * same keys so it picks the same colours. A key no day carries totals 0, which `ShareBar` leaves
 * out; a part whose key is not in `keys` is not counted, as the legend could not name it.
 */
export function windowShares(data: ChartDatum[], keys: ChartKey[]): ShareItem[] {
  return keys.map((key) => {
    const value = data.reduce(
      (sum, datum) => sum + datum.parts.reduce((inDay, part) => (part.key === key.key ? inDay + part.value : inDay), 0),
      0,
    );
    return key.className === undefined
      ? { key: key.key, label: key.label, value }
      : { key: key.key, label: key.label, value, className: key.className };
  });
}

/** The window's gate tally as share items, from the totals block — the authority, not a sum of days. */
export function gateShares(total: ThroughputTotal): ShareItem[] {
  const value: Record<string, number> = {
    passed: total.passed,
    failed: total.failed,
    errored: total.errored,
    no_gate: total.no_gate,
    other: total.other,
  };
  return GATE_KEYS.map((key) => ({ ...key, value: value[key.key] ?? 0 }));
}

/** The window's tokens by kind as share items, from the totals block. */
export function tokenShares(total: TokenTotal): ShareItem[] {
  const value: Record<string, number> = {
    input: total.input,
    cache_read: total.cache_read,
    cache_creation: total.cache_creation,
    output: total.output,
  };
  return TOKEN_KEYS.map((key) => ({ ...key, value: value[key.key] ?? 0 }));
}

/**
 * The change against the previous window, in words and without arrows or percentages (§9.2). Generic
 * where `ModeState`'s `TREND_TEXT` is about tokens; same tenth-either-way band as `efficiencyTrend`.
 * Empty when either side was not measured — silence, never a made-up direction.
 */
export function versus(now: number | null, before: number | null, days: number): string {
  if (now === null || before === null) return "";
  const window = `the previous ${days} days`;
  if (before === 0) return now === 0 ? `level with ${window}` : `more than ${window}`;
  const change = (now - before) / before;
  if (Math.abs(change) < 0.1) return `level with ${window}`;
  return change > 0 ? `more than ${window}` : `less than ${window}`;
}

export function quotaSeries(history: QuotaHistory): StepSeries[] {
  return history.series.map((one) => ({
    key: `${one.provider}:${one.window}`,
    label: `${one.provider} ${one.window}${one.fidelity === "derived" ? " (derived)" : ""}`,
    points: one.points.map((point) => ({ at: Date.parse(point.at), value: point.used_fraction, stale: point.stale })),
  }));
}
