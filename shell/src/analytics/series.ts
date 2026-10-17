import type {
  CostBlock,
  DeliveredBlock,
  QuotaHistory,
  ThroughputBlock,
  ThroughputTotal,
  TokenBlock,
  TokenTotal,
} from "../data/analytics";
import type { BarDatum, BarKey, StepSeries } from "../ui";

/**
 * The §6 blocks as chart props, and the words a tile says about a change. Pure, so the arithmetic
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
export function costKeys(cost: CostBlock): BarKey[] {
  const keys = cost.keys.map((key): BarKey => {
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
export function costBars(cost: CostBlock): BarDatum[] {
  return cost.series.map((point) => ({
    day: point.day,
    partial: point.partial,
    parts: [
      ...point.by.map((part) => ({ key: part.key, value: part.measured_usd })),
      { key: ESTIMATED_KEY, value: point.by.reduce((sum, part) => sum + part.estimated_usd, 0) },
    ],
  }));
}

export const TOKEN_KEYS: BarKey[] = [
  { key: "input", label: "Input" },
  { key: "cache_read", label: "Cache read" },
  { key: "cache_creation", label: "Cache write" },
  { key: "output", label: "Output" },
];

export function tokenBars(tokens: TokenBlock): BarDatum[] {
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
export const GATE_KEYS: BarKey[] = [
  { key: "passed", label: "Passed", className: "ui-chart-passed" },
  { key: "failed", label: "Failed", className: "ui-chart-failed" },
  { key: "errored", label: "Could not run", className: "ui-chart-errored" },
  { key: "no_gate", label: "No gate", className: "ui-chart-ungated" },
  { key: "other", label: "Other status", className: "ui-chart-unknown" },
];

export function gateBars(throughput: ThroughputBlock): BarDatum[] {
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

export const DELIVERED_KEYS: BarKey[] = [{ key: "landed", label: "Landed" }];

export function deliveredBars(delivered: DeliveredBlock): BarDatum[] {
  return delivered.series.map((point) => ({
    day: point.day,
    partial: point.partial,
    parts: [{ key: "landed", value: point.landed }],
  }));
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
