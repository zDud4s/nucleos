/**
 * Three readings of a number the núcleo recorded: when, how much it cost, and
 * how full the context is.
 *
 * One file for three components, which is a deliberate exception to this
 * directory's one-primitive-per-file rule and is recorded as such. They are
 * short pure formatters, they are always used together — every surface that
 * shows a run shows all three — and splitting them would give the design system
 * three files whose entire content is a `toFixed` and a `span`.
 *
 * All three take the raw field, nulls included, and decide what absence means
 * themselves. That is the point: `cost_usd: null` is *not recorded*, not zero,
 * and a page that wrote `?? 0` would be inventing a spend of nothing.
 */

/* ------------------------------------------------------------------ when -- */

export interface RelativeTimeProps {
  /** An RFC 3339 timestamp, as every `created_at` in the núcleo is written. */
  at: string;
  /** Now, in epoch milliseconds. Injected so the text can be asserted without a clock. */
  now?: number;
}

/**
 * A time, said the way a person reads it, with the exact one on hover.
 *
 * Relative because the question being asked of a run list is always "how long
 * ago", never "at what o'clock". The absolute timestamp stays in `title` and in
 * `dateTime` rather than being thrown away — relative text is unusable the
 * moment you need to correlate with a log.
 */
export function RelativeTime({ at, now }: RelativeTimeProps) {
  const when = Date.parse(at);
  // A timestamp we cannot parse is shown verbatim. Rendering "Invalid Date", or
  // nothing at all, would hide a daemon that changed its format.
  if (Number.isNaN(when)) return <span className="ui-reading-raw">{at}</span>;
  return (
    <time className="ui-reading-time" dateTime={at} title={new Date(when).toLocaleString()}>
      {relativeText(when, now ?? Date.now())}
    </time>
  );
}

/** The relative phrase, as a pure function so it can be asserted directly. */
export function relativeText(when: number, now: number): string {
  const seconds = Math.round((now - when) / 1000);
  const size = Math.abs(seconds);
  if (size < 5) return "just now";
  const scaled =
    size < 60
      ? `${size}s`
      : size < 3600
        ? `${Math.floor(size / 60)}min`
        : size < 86400
          ? `${Math.floor(size / 3600)}h`
          : `${Math.floor(size / 86400)}d`;
  return seconds >= 0 ? `${scaled} ago` : `in ${scaled}`;
}

/* ------------------------------------------------------------------ cost -- */

export interface CostLineProps {
  costUsd: number | null;
  inputTokens: number | null;
  outputTokens: number | null;
  cachedTokens: number | null;
}

/**
 * What a run spent, in money and in tokens.
 *
 * Four decimal places on the money because a single run costs cents, and a
 * two-decimal `$ 0.00` for a run that really spent $0.004 reads as free. Cached
 * reads are shown beside the fresh ones rather than folded into them: they are
 * the cheap part, and a run whose input is mostly cache is a different fact
 * about cost than one that paid full price for the same window.
 */
export function CostLine({ costUsd, inputTokens, outputTokens, cachedTokens }: CostLineProps) {
  return (
    <p className="ui-cost">
      <span className="ui-cost-money">
        {costUsd === null ? "cost not recorded" : `$ ${costUsd.toFixed(4)}`}
      </span>
      <span className="ui-cost-tokens">{tokenCount(inputTokens)} in</span>
      <span className="ui-cost-tokens">{tokenCount(outputTokens)} out</span>
      <span className="ui-cost-tokens">{tokenCount(cachedTokens)} cached</span>
    </p>
  );
}

/** A token count, short. An em dash for absent, which is not the same as `0`. */
export function tokenCount(count: number | null): string {
  if (count === null) return "—";
  return count < 1000 ? String(count) : `${(count / 1000).toFixed(1)}k`;
}

/* --------------------------------------------------------------- context -- */

/**
 * The window a handoff is measured against — `HANDOFF_CONTEXT_LIMIT_FLOOR` in
 * `core/src/runs.rs`. Duplicated rather than asked for, because the daemon
 * exposes no route that reports it; a drift here only mis-scales a bar.
 */
export const CONTEXT_WINDOW_TOKENS = 200_000;

/** Four fifths — `HANDOFF_THRESHOLD_FRACTION` in `core/src/handoff.rs`. */
export const HANDOFF_SHARE = 0.8;

export interface ContextMeterProps {
  /** `context_fill`, in tokens. `null` is *not reported*, never zero. */
  fill: number | null;
}

/**
 * How full the context is, against the point where the run splits.
 *
 * The mark at four fifths is what makes this a reading rather than decoration:
 * a bar without it says "quite full", and the thing worth knowing is that the
 * daemon is about to hand this run to a successor. The percentage is in text as
 * well as in width, because a bar is not readable by anything that does not
 * render.
 */
export function ContextMeter({ fill }: ContextMeterProps) {
  if (fill === null) return <p className="ui-meter-absent">context fill not reported</p>;
  const share = Math.min(fill / CONTEXT_WINDOW_TOKENS, 1);
  const percent = Math.round(share * 100);
  const past = share >= HANDOFF_SHARE;
  return (
    <p className={past ? "ui-meter ui-meter-past" : "ui-meter"}>
      <span
        className="ui-meter-track"
        role="img"
        aria-label={`context ${percent}% full, handoff at ${Math.round(HANDOFF_SHARE * 100)}%`}
      >
        <span className="ui-meter-fill" style={{ width: `${percent}%` }} />
        <span className="ui-meter-mark" style={{ left: `${HANDOFF_SHARE * 100}%` }} />
      </span>
      <span className="ui-meter-text">
        {percent}% of context{past ? " — at the handoff mark" : ""}
      </span>
    </p>
  );
}
