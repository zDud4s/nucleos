import { Group } from "@visx/group";
import { scaleLinear } from "@visx/scale";
import { Bar } from "@visx/shape";

/**
 * A pulse, drawn small.
 *
 * The first `@visx` in this app, and the reason `csp-gate/surfaces.tsx` grew a
 * charts row: a chart library that builds a `<style>` for a tooltip is exactly
 * what that gate exists to catch, and this one has to be proved under the
 * production policy rather than assumed.
 *
 * **Bars, and not the area this started as.** An area anchored at zero over a
 * flat series is a filled rectangle — and a department that ran one task a day
 * for six days IS a flat series, which is the common case here rather than the
 * edge one. No scaling fixes it: anchored at the minimum a flat series has zero
 * height, anchored at zero it is a slab. Bars have no such failure — six equal
 * bars read as six days with something in them, which is what happened. Found
 * by looking at a screenshot; no test could have had an opinion about it.
 *
 * **`label` is not decoration and is not optional.** Every series this app can
 * draw comes out of `GET /team-runs`, which is a hard `LIMIT 100` across all
 * departments with no paging and no dates — so an axis reading "14 days" would
 * be an invention, and a chart with no label at all invites the reader to
 * assume one. The caller passes the window it really has: "the 23 runs in the
 * window".
 */

export interface SparklineProps {
  /** The series, oldest first. Fewer than two points is a real answer and draws a rail. */
  values: number[];
  /** The window these values are, in words. Rendered, and used as the accessible name. */
  label: string;
  /**
   * Keep the label as the accessible name, but stop drawing it.
   *
   * For a table cell, where the column header already says what the series is
   * and repeating it under every row is the noise a table exists to remove. The
   * label stays mandatory and stays in the accessible tree — this hides it from
   * the eye, never from a reader.
   */
  labelHidden?: boolean;
  /**
   * What each bar is, one string per value, in the order `values` is in.
   *
   * Rendered as an SVG `<title>` — the first child of each bar's `<g>`, which is the
   * tooltip the format has had all along and needs nothing from `@visx` to draw. The
   * label says what the window IS; this says what one mark in it is, which is the
   * question a reader has only once they have found the mark worth asking about.
   *
   * Absent is a real answer: a `<g>` with no `<title>` reads exactly as it did before.
   */
  titles?: string[];
  width?: number;
  height?: number;
}

/** Small enough to sit inside a card's footer, big enough for a shape to be visible. */
const WIDTH = 168;
const HEIGHT = 28;
/** So two adjacent bars read as two, at any count. */
const GAP = 2;
/**
 * The widest a bar gets, however few there are.
 *
 * Without it a six-day window fills a 168px box with six 26px tiles, and the
 * thing stops reading as a pulse and starts reading as a heatmap. Capped and
 * centred in its slot, the same six days read as six marks along a timeline —
 * which is what they are.
 */
const MAX_BAR = 9;
/**
 * The widest one bar's share of the box gets.
 *
 * Without this the slot is simply `width / count`, so three days in a 96px cell
 * get 32px each and the bars sit 23px apart — three loose blocks rather than a
 * series. Capping the slot keeps them adjacent at any count, and what is left
 * over goes to the right edge below.
 */
const MAX_SLOT = MAX_BAR + GAP;

export function Sparkline({
  values,
  label,
  labelHidden = false,
  titles,
  width = WIDTH,
  height = HEIGHT,
}: SparklineProps) {
  const labelClass = labelHidden ? "ui-spark-label ui-spark-label-said" : "ui-spark-label";

  /*
    One bar is not a pulse and zero bars are not anything. Both draw the same
    dashed rail the `Meter` uses for an absent ceiling, so "nothing to plot"
    looks like the app's other absences rather than like a real reading of zero.
  */
  if (values.length < 2) {
    return (
      <p className="ui-spark ui-spark-empty">
        <span className="ui-spark-rail" role="img" aria-label={`${label} — not enough to plot`} />
        <span className={labelClass}>{label}</span>
      </p>
    );
  }

  const top = Math.max(...values);
  /*
    The floor is zero and not the minimum: a bar's length is a quantity, and a
    chart starting at 3 would draw a day with 3 runs in it as nothing at all.
    `top || 1` keeps an all-zero series a valid scale rather than a degenerate
    one — it draws as a row of floors, which is true.
  */
  const y = scaleLinear<number>({ domain: [0, top === 0 ? 1 : top], range: [0, height - 1] });
  const slot = Math.min(width / values.length, MAX_SLOT);
  const bar = Math.max(slot - GAP, 1);
  /* Centred in its slot, so the spacing stays even at any count. */
  const inset = (slot - bar) / 2;
  /*
    Flushed right, so the newest bar sits at the same x on every row of a
    column. Down a table that is what makes the series comparable at a glance:
    a department with three days and one with twelve both end at "now".
  */
  const left = width - slot * values.length;
  const last = values.length - 1;

  return (
    <p className="ui-spark">
      <svg className="ui-spark-svg" width={width} height={height} role="img" aria-label={label}>
        <Group>
          {values.map((value, index) => {
            /* A day with nothing in it still gets a mark, or the row would have
               gaps that read as days the window does not cover. */
            const drawn = Math.max(y(value), 1);
            const said = titles?.[index];
            return (
              /* The `<title>` is the FIRST child of the group or it is not a tooltip —
                 that is the format's rule, not a convention. No child at all when the
                 caller has nothing to say about this bar. */
              <g key={index}>
                {said === undefined ? null : <title>{said}</title>}
                <Bar
                  className={index === last ? "ui-spark-bar ui-spark-now" : "ui-spark-bar"}
                  x={left + index * slot + inset}
                  y={height - drawn}
                  width={bar}
                  height={drawn}
                  rx={1}
                />
              </g>
            );
          })}
        </Group>
      </svg>
      <span className={labelClass}>{label}</span>
    </p>
  );
}
