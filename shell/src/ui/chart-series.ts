/**
 * The vocabulary every day-by-day chart shares: a day, its parts, and the keys that name them.
 *
 * Lifted out of `BarChart` so `AreaChart` and `ShareBar` speak the same types and, above all, pick
 * the same colour for the same key. `seriesClass` is the one place that decides it: a chart's
 * tooltip swatch and the share bar under it agree because they call this with the same keys.
 */

export interface ChartPart {
  key: string;
  value: number;
}

export interface ChartDatum {
  /** `YYYY-MM-DD`, a UTC day. */
  day: string;
  /** Parts in any order; missing keys are zero. */
  parts: ChartPart[];
  /** Today, not over yet: drawn as such (hollow bar, dashed segment) and titled as such. */
  partial?: boolean;
}

export interface ChartKey {
  key: string;
  label: string;
  /** A fixed series class (`ui-chart-judge`, `ui-chart-failed`, …). Otherwise one of the eight by position. */
  className?: string;
}

const CATEGORICAL = 8;

/** The series class for a key: its own, or one of the eight categorical ones by position. */
export function seriesClass(keys: ChartKey[], key: string): string {
  const index = keys.findIndex((candidate) => candidate.key === key);
  // A key the legend does not know is "other": falling back to series 1 would pass it off as that one.
  if (index < 0) return "ui-chart-other";
  const own = keys[index].className;
  if (own !== undefined) return own;
  return `ui-chart-s${(index % CATEGORICAL) + 1}`;
}
