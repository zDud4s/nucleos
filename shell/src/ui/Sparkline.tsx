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

export function Sparkline({ values, label, width = WIDTH, height = HEIGHT }: SparklineProps) {
  /*
    One bar is not a pulse and zero bars are not anything. Both draw the same
    dashed rail the `Meter` uses for an absent ceiling, so "nothing to plot"
    looks like the app's other absences rather than like a real reading of zero.
  */
  if (values.length < 2) {
    return (
      <p className="ui-spark ui-spark-empty">
        <span className="ui-spark-rail" role="img" aria-label={`${label} — not enough to plot`} />
        <span className="ui-spark-label">{label}</span>
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
  const slot = width / values.length;
  const bar = Math.min(Math.max(slot - GAP, 1), MAX_BAR);
  /* Centred in its slot, so the spacing stays even at any count. */
  const inset = (slot - bar) / 2;
  const last = values.length - 1;

  return (
    <p className="ui-spark">
      <svg className="ui-spark-svg" width={width} height={height} role="img" aria-label={label}>
        <Group>
          {values.map((value, index) => {
            /* A day with nothing in it still gets a mark, or the row would have
               gaps that read as days the window does not cover. */
            const drawn = Math.max(y(value), 1);
            return (
              <Bar
                key={index}
                className={index === last ? "ui-spark-bar ui-spark-now" : "ui-spark-bar"}
                x={index * slot + inset}
                y={height - drawn}
                width={bar}
                height={drawn}
                rx={1}
              />
            );
          })}
        </Group>
      </svg>
      <span className="ui-spark-label">{label}</span>
    </p>
  );
}
