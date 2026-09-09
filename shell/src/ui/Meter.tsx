/**
 * Two ways of drawing a ceiling, and the reason they are two.
 *
 * A department carries five ceilings and they are not the same kind of thing.
 * Two of them — live runs, open actions — have something occupying them right
 * now, and a bar is the honest picture: this much of that is taken. The other
 * three — rounds, parallel, spend — are rules applied to **each task, from
 * zero, every time**. Nothing is consuming them, so a bar would promise a
 * consumption that does not exist and would sit at 0% forever.
 *
 * So: `Meter` for occupancy, `LimitChip` for a per-task rule. A caller that
 * reaches for `Meter` and has no current value is reaching for the wrong one.
 *
 * Two primitives in one file, which is the second recorded exception to this
 * directory's one-per-file rule (`readings.tsx` is the first). They travel
 * together for the same reason those three do: every surface that draws a
 * ceiling draws both kinds at once, and the whole point of the pair is the
 * contrast between them. Split across two files, the next person to add a
 * ceiling finds one of them and draws a bar for a rule.
 *
 * The classes are `ui-gauge-*` and not `ui-meter-*`: `ContextMeter` in
 * `readings.tsx` already owns that prefix, and its rules live in `ui.css` too,
 * directly above the gauge's own. Reusing the name would have every rule
 * written for either one silently reach both.
 */

/**
 * How full a ceiling is drawn, and what the fill MEANS. Absent is the seventh tone's job.
 *
 * `quantity` is the neutral rung. Money taken is a fact, not work in flight or a summons; it is
 * drawn in `var(--text-muted)` and adds no token. Required, not optional: a caller drawing a bar
 * says what the bar means. `active` is a ceiling something occupies right now, `pending` asks
 * the reader for something, and `danger` is bad news.
 */
export type MeterTone = "active" | "pending" | "danger" | "quantity";

export interface MeterProps {
  /** What is being occupied — "at work", "waiting on you". Also the accessible name. */
  label: string;
  /** How many there are right now. Never null: a count the caller has not read is not this component's case. */
  value: number;
  /**
   * The ceiling, or `null` for no ceiling at all.
   *
   * `null` is **not** a ceiling of zero, and the daemon pays to keep the two
   * apart (`core/src/team.rs:2072`). A ceiling of zero is a real setting that
   * means *never*; no ceiling means the house budget is the only brake. Drawn
   * as a dashed rail with no fill, so it cannot be read as either an empty bar
   * or a full one.
   */
  ceiling: number | null;
  /** What the fill means. Required — see {@link MeterTone}. */
  tone: MeterTone;
  /**
   * How to write the two numbers, for a reading that is not a plain count.
   *
   * Money is the caller that needs it: a live task at `1.2 / 5` reads as one
   * and a bit of something countable. `$1.20 / $5.00` reads as money, which is
   * what it is.
   */
  format?: (value: number) => string;
  /** Whether to draw the label and figures above the bar. `true` unless already written nearby. */
  head?: boolean;
}

/**
 * A ceiling with something in it.
 *
 * The number is in text as well as in width, because a bar is not readable by
 * anything that does not render — and because the exact figure is what somebody
 * acts on. `aria-label` carries the same sentence the eye gets.
 */
export function Meter({ label, value, ceiling, tone, format, head = true }: MeterProps) {
  const write = format ?? String;

  if (ceiling === null) {
    return (
      <p className="ui-gauge ui-gauge-open">
        {head && (
          <span className="ui-gauge-head">
            <span className="ui-gauge-label">{label}</span>
            <span className="ui-gauge-value">{write(value)}</span>
          </span>
        )}
        <span className="ui-gauge-track" role="img" aria-label={`${label}: ${write(value)}, no ceiling`} />
        <span className="ui-gauge-note">no ceiling</span>
      </p>
    );
  }

  /*
    A ceiling of zero divides by nothing. It is also a real setting — "never
    start one" — and the only honest picture of `1 of 0` is a full bar over a
    ceiling that has already been passed, which is what `share` lands on.
  */
  const share = ceiling === 0 ? (value > 0 ? 1 : 0) : Math.min(value / ceiling, 1);
  const percent = Math.round(share * 100);
  const full = value >= ceiling;
  const classes = ["ui-gauge", `ui-gauge-${tone}`];
  if (full) classes.push("ui-gauge-full");

  return (
    <p className={classes.join(" ")}>
      {head && (
        <span className="ui-gauge-head">
          <span className="ui-gauge-label">{label}</span>
          <span className="ui-gauge-value">
            {write(value)} / {write(ceiling)}
          </span>
        </span>
      )}
      <span className="ui-gauge-track" role="img" aria-label={`${label}: ${write(value)} of ${write(ceiling)}`}>
        <span className="ui-gauge-fill" style={{ width: `${percent}%` }} />
      </span>
    </p>
  );
}

export interface LimitChipProps {
  /** The rule's short name — `rounds`, `parallel`, `spend`. */
  name: string;
  /** The rule's ceiling, or `null` for a rule that is not set. */
  ceiling: number | null;
  /**
   * How to write the number, for a ceiling that is not a plain count.
   *
   * Money is the only caller that needs it today, and it needs it badly: a
   * budget of `5` written bare reads as five of something countable.
   */
  format?: (ceiling: number) => string;
}

/**
 * A rule that applies to each task, with no bar.
 *
 * The absence of a bar is the whole design. `rounds ≤ 4` is a fact about every
 * task this department will ever run, and there is no partial state of it to
 * fill in.
 */
export function LimitChip({ name, ceiling, format }: LimitChipProps) {
  const written = ceiling === null ? "no ceiling" : (format ?? String)(ceiling);
  return (
    <span className={ceiling === null ? "ui-limit ui-limit-open" : "ui-limit"}>
      <span className="ui-limit-name">{name}</span>
      <span className="ui-limit-value">{ceiling === null ? written : `≤ ${written}`}</span>
    </span>
  );
}

/** Money, to the cent — the way a per-task budget is set and the way it is read. */
export function usd(amount: number): string {
  return `$${amount.toFixed(2)}`;
}
