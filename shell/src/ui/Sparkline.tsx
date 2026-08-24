import { Group } from "@visx/group";
import { scaleLinear } from "@visx/scale";
import { AreaClosed, LinePath } from "@visx/shape";

/**
 * A pulse, drawn small.
 *
 * The first `@visx` in this app, and the reason `csp-gate/surfaces.tsx` grew a
 * charts row: a chart library that builds a `<style>` for a tooltip is exactly
 * what that gate exists to catch, and this one has to be proved under the
 * production policy rather than assumed.
 *
 * **`label` is not decoration and is not optional.** Every series this app can
 * draw comes out of `GET /team-runs`, which is a hard `LIMIT 100` across all
 * departments with no paging and no dates — so an axis reading "14 days" would
 * be an invention, and a chart with no label at all invites the reader to
 * assume one. The caller passes the window it really has: "the 23 runs in the
 * window".
 *
 * Deliberately no curve import. `@visx/curve` is on disk as somebody else's
 * transitive dependency and is not in `package.json`; importing it would work
 * today and break on the first clean install. The default linear interpolation
 * is also the honest one for a series of discrete events.
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
const HEIGHT = 34;

export function Sparkline({ values, label, width = WIDTH, height = HEIGHT }: SparklineProps) {
  /*
    One point cannot be a line and zero points cannot be anything. Both draw the
    same dashed rail the `Meter` uses for an absent ceiling, so "nothing to plot"
    looks like the app's other absences rather than like a flat line at zero —
    which is a reading, and a wrong one.
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
  const x = scaleLinear<number>({ domain: [0, values.length - 1], range: [1, width - 1] });
  /*
    The domain floor is zero and not the minimum. A sparkline scaled between its
    own extremes turns a series of 3, 4, 3 into a mountain range; anchored at
    zero it stays what it is, which is nearly flat.

    The ceiling is `top || 1` so an all-zero series produces a valid scale
    rather than a degenerate one — it draws along the floor, which is true.
  */
  const y = scaleLinear<number>({ domain: [0, top === 0 ? 1 : top], range: [height - 2, 2] });

  const last = values.length - 1;

  return (
    <p className="ui-spark">
      <svg className="ui-spark-svg" width={width} height={height} role="img" aria-label={label}>
        <Group>
          <AreaClosed<number>
            data={values}
            x={(_, index) => x(index)}
            y={(value) => y(value)}
            yScale={y}
            className="ui-spark-area"
          />
          <LinePath<number>
            data={values}
            x={(_, index) => x(index)}
            y={(value) => y(value)}
            className="ui-spark-line"
          />
          {/* The endpoint, because the question a pulse answers is "and now?" */}
          <circle className="ui-spark-now" cx={x(last)} cy={y(values[last])} r={2.5} />
        </Group>
      </svg>
      <span className="ui-spark-label">{label}</span>
    </p>
  );
}
