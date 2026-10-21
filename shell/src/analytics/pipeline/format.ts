import type { Dist } from "../../data/analytics-pipeline";
import { percentText } from "../../lib/format";

/**
 * The words the Pipeline section writes numbers in (spec analytics-pipeline §8). Pure, so a card's
 * sentence can be tested without a render. A figure nobody measured is `no data`, never a zero.
 */

export const NO_DATA = "no data";

/** The non-empty clauses of a line, joined by ` · `. */
export function clauses(parts: string[]): string {
  return parts.filter((part) => part !== "").join(" · ");
}

/** A span of seconds as its largest sensible unit: `12 s`, `4 min`, `1 h 30`, `2.2 d`. */
export function humanSeconds(seconds: number): string {
  const whole = Math.round(Math.max(0, seconds));
  if (whole < 60) return `${whole} s`;
  const minutes = Math.round(whole / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    const rest = minutes % 60;
    return rest === 0 ? `${hours} h` : `${hours} h ${rest}`;
  }
  return `${(whole / 86_400).toFixed(1)} d`;
}

/** A fraction as a whole percent, or `no data` when it was not measured. */
export function percentOr(fraction: number | null): string {
  return percentText(fraction, NO_DATA);
}

/**
 * A distribution in a line. The p95 only exists from five samples on (the núcleo sends `null`
 * below), and a missing one is left out rather than made up. `format` is for a unit other than seconds.
 */
export function distLine(dist: Dist, format: (value: number) => string = humanSeconds): string {
  if (dist.p50 === null) return NO_DATA;
  return clauses([`p50 ${format(dist.p50)}`, dist.p95 === null ? "" : `p95 ${format(dist.p95)}`, `${dist.n} measured`]);
}
