import { AxisBottom, AxisLeft, type TickRendererProps } from "@visx/axis";
import { Group } from "@visx/group";
import { scaleBand, scaleLinear } from "@visx/scale";
import { Bar } from "@visx/shape";

/**
 * Days, stacked.
 *
 * The analytics view's one bar chart (spec §9.10): a bar per UTC day, each split into the series
 * `keys` names, in that order, bottom up. Built on `Sparkline`'s rules and on the same three visx
 * packages plus `@visx/axis`.
 *
 * **No inline style, anywhere.** Colour comes from a class per series that sets `--series`
 * (`ui.css`, "charts"); the CSP gate refuses `style=`. That is also why the axes never draw their
 * own text: `@visx/axis` renders tick labels through `@visx/text`, whose `Text` wraps every label
 * in an `<svg style="overflow: visible">`. `ChartTickLabel` replaces it with a plain `<text>`, and
 * no axis is ever given a `label` (an empty one skips `Text` entirely).
 *
 * **`label` is not optional**, for `Sparkline`'s reason: it is the window in words and the chart's
 * accessible name. Every day and every segment carries a `<title>` — the first child of its `<g>`,
 * which is the format's tooltip rule.
 *
 * **Empty is drawn, not blank.** A window with nothing in it keeps its axes and says why in `empty`.
 */

export interface BarPart {
  key: string;
  value: number;
}

export interface BarDatum {
  /** `YYYY-MM-DD`, a UTC day. */
  day: string;
  /** Parts in any order; missing keys are zero. */
  parts: BarPart[];
  /** Today, not over yet: drawn hollow and titled as such. */
  partial?: boolean;
}

export interface BarKey {
  key: string;
  label: string;
  /** A fixed series class (`ui-chart-judge`, `ui-chart-failed`, …). Otherwise one of the eight by position. */
  className?: string;
}

export interface BarChartProps {
  data: BarDatum[];
  /** The series, in stacking order (first at the bottom) and legend order. */
  keys: BarKey[];
  /** The window, in words. Rendered as the accessible name. */
  label: string;
  /** How one value is written — on the axis and in every title. */
  format: (value: number) => string;
  /** The sentence for a window with nothing in it. */
  empty: string;
  width?: number;
  height?: number;
}

const WIDTH = 640;
const HEIGHT = 180;
const MARGIN = { top: 8, right: 8, bottom: 22, left: 52 };
const CATEGORICAL = 8;
/** At most this many day labels under the axis, or they overlap at 90 days. */
const DAY_TICKS = 6;

/** The series class for a key: its own, or one of the eight categorical ones by position. */
export function seriesClass(keys: BarKey[], key: string): string {
  const index = keys.findIndex((candidate) => candidate.key === key);
  // A key the legend does not know is "other": falling back to series 1 would pass it off as that one.
  if (index < 0) return "ui-chart-other";
  const own = index >= 0 ? keys[index].className : undefined;
  if (own !== undefined) return own;
  return `ui-chart-s${(index % CATEGORICAL) + 1}`;
}

export interface StackedPart extends BarPart {
  from: number;
  to: number;
}

/** One day's positive parts, ordered by `keys` (unknown keys last), with cumulative bounds. */
export function stackDay(parts: BarPart[], keys: BarKey[]): StackedPart[] {
  const rank = (key: string) => {
    const index = keys.findIndex((candidate) => candidate.key === key);
    return index < 0 ? keys.length : index;
  };
  const ordered = parts.filter((part) => part.value > 0).sort((a, b) => rank(a.key) - rank(b.key));
  return ordered.reduce<StackedPart[]>((stack, part) => {
    const from = stack.length === 0 ? 0 : stack[stack.length - 1].to;
    return [...stack, { ...part, from, to: from + part.value }];
  }, []);
}

/**
 * A tick label as a bare `<text>`: what `@visx/text` would draw, minus the `style` attribute.
 * Shared with `StepLine`.
 */
export function ChartTickLabel({ x, y, dx, dy, textAnchor, formattedValue }: TickRendererProps) {
  return (
    <text x={x} y={y} dx={dx} dy={dy} textAnchor={textAnchor} className="ui-chart-tick-label">
      {formattedValue}
    </text>
  );
}

export function BarChart({ data, keys, label, format, empty, width = WIDTH, height = HEIGHT }: BarChartProps) {
  const innerWidth = width - MARGIN.left - MARGIN.right;
  const innerHeight = height - MARGIN.top - MARGIN.bottom;
  const stacks = data.map((datum) => stackDay(datum.parts, keys));
  const totals = stacks.map((stack) => (stack.length === 0 ? 0 : stack[stack.length - 1].to));
  const top = Math.max(0, ...totals);
  const nothing = top === 0;

  const x = scaleBand<string>({ domain: data.map((datum) => datum.day), range: [0, innerWidth], padding: 0.2 });
  // Zero floor for the reason `Sparkline` gives; a degenerate [0, 0] would put every tick on one line.
  const y = scaleLinear<number>({ domain: [0, nothing ? 1 : top], range: [innerHeight, 0], nice: true });
  const every = Math.max(1, Math.ceil(data.length / DAY_TICKS));
  const labelOf = (key: string) => keys.find((candidate) => candidate.key === key)?.label ?? key;

  return (
    <figure className="ui-chart">
      <svg className="ui-chart-svg" viewBox={`0 0 ${width} ${height}`} role="img" aria-label={label}>
        <Group left={MARGIN.left} top={MARGIN.top}>
          {data.map((datum, index) => {
            const left = x(datum.day) ?? 0;
            const today = datum.partial === true;
            return (
              <g key={datum.day} className={today ? "ui-bar-day ui-bar-partial" : "ui-bar-day"}>
                <title>{`${datum.day}: ${format(totals[index])}${today ? " — day in progress" : ""}`}</title>
                {stacks[index].map((part) => (
                  <g key={part.key}>
                    <title>{`${datum.day} · ${labelOf(part.key)}: ${format(part.value)}`}</title>
                    <Bar
                      className={`ui-bar ${seriesClass(keys, part.key)}`}
                      x={left}
                      y={y(part.to)}
                      width={x.bandwidth()}
                      height={Math.max(y(part.from) - y(part.to), 0)}
                    />
                  </g>
                ))}
              </g>
            );
          })}
        </Group>
        <AxisLeft
          left={MARGIN.left}
          top={MARGIN.top}
          scale={y}
          numTicks={4}
          tickFormat={(value) => format(Number(value))}
          axisClassName="ui-chart-axis"
          tickClassName="ui-chart-tick"
          tickComponent={ChartTickLabel}
        />
        <AxisBottom
          left={MARGIN.left}
          top={MARGIN.top + innerHeight}
          scale={x}
          tickValues={data.filter((_, index) => index % every === 0).map((datum) => datum.day)}
          tickFormat={(day) => String(day).slice(5)}
          axisClassName="ui-chart-axis"
          tickClassName="ui-chart-tick"
          tickComponent={ChartTickLabel}
        />
      </svg>
      {nothing ? <p className="ui-chart-empty">{empty}</p> : null}
      <ul className="ui-chart-legend" aria-label={`${label}, series`}>
        {keys.map((key) => (
          <li key={key.key} className="ui-chart-legend-item">
            <span aria-hidden="true" className={`ui-chart-swatch ${seriesClass(keys, key.key)}`} />
            {key.label}
          </li>
        ))}
      </ul>
    </figure>
  );
}
