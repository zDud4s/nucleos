/**
 * A fraction as a whole-number percentage: `percentText(0.424)` is "42%".
 *
 * `fallback` for a reading that is not a number (`undefined`, `null`, `NaN`, infinity) — a value
 * that is absent is not zero, and "0%" for "not measured yet" is a claim. Defaults to an em dash,
 * the app's mark for an unknown (`.ui-unknown`). Rounds to nearest; a fraction above 1 reads as
 * over 100%, which is a true thing for a ceiling that has been passed.
 */
export function percentText(fraction: number | null | undefined, fallback = "—"): string {
  if (fraction === null || fraction === undefined || !Number.isFinite(fraction)) return fallback;
  return `${Math.round(fraction * 100)}%`;
}
