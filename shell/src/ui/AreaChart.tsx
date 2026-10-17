import { curveMonotoneX } from "@visx/curve";
import { scaleLinear } from "@visx/scale";
import { AreaClosed, LinePath } from "@visx/shape";
import { useId, useState, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";

import { seriesClass, type ChartDatum, type ChartKey } from "./chart-series";
import { useWidth } from "./useWidth";

/**
 * One metric, day by day, as a single smooth area.
 *
 * The analytics view's replacement for stacked bars: the chart draws only the TOTAL — a monotone
 * curve (smooth, and never overshooting a value the data does not reach) over a vertical gradient —
 * and the breakdown leaves the chart for two places that read better: the hover tooltip, which
 * lists a day's parts in `keys` order, and `ShareBar` under the chart, which is the legend.
 *
 * **Drawn at its real size.** The svg's `viewBox` is the measured width of its box (`useWidth`) by
 * a fixed height, so one unit is one pixel and an `--text-xs` label stays `--text-xs`. A fixed
 * 640-wide `viewBox` stretched into a wide card is what made `BarChart`'s axis text huge.
 *
 * **No inline style, anywhere** — `BarChart`'s rule and reason (the CSP gate refuses `style=`).
 * Colour reaches every mark through `--series`, set by the `tone` class: the line strokes with it,
 * and the gradient's stops take `stop-color` from it in `ui.css`, so the fill is a class too. No
 * `@visx/axis` and no `@visx/text`: the few labels are bare `<text>` elements placed here. The
 * tooltip is drawn inside the svg for the same reason — an HTML tooltip would need a computed
 * `left`/`top`, which is exactly the inline style the gate refuses; a `transform` attribute is not.
 *
 * **`apart`** names a key that is never folded into the drawn total (the estimate, beside the
 * measured spend): it does not raise the curve or the scale, and the tooltip lists it on its own
 * row after the `Total`, so the total visibly sums the rows above it.
 *
 * **Today is not over**: when the last datum is `partial`, the last segment of the line is dashed
 * and the last day label reads `today`.
 *
 * **`compact`** is the metric card's sparkline: no grid, no labels, no tooltip, decorative
 * (`aria-hidden` — the card around it says the number in words), with a dot on the last value.
 */

export interface AreaChartProps {
  data: ChartDatum[];
  /** The breakdown, listed in the tooltip in this order. */
  keys: ChartKey[];
  /** The window, in words: the chart's accessible name. */
  label: string;
  /** How one value is written — on the grid and in the tooltip. */
  format: (value: number) => string;
  /** The sentence shown under the grid when every total is 0. */
  empty: string;
  /** The class colouring the area, e.g. `ui-chart-cost`. */
  tone: string;
  /** A key never folded into the drawn total; listed apart in the tooltip. */
  apart?: string;
  height?: number;
  /** Sparkline mode for a metric card. */
  compact?: boolean;
}

const HEIGHT = 200;
const COMPACT_HEIGHT = 40;
/** Room above the top gridline for its label, below the plot for the day labels, and at the sides for the dot. */
const FULL = { top: 16, bottom: 22, side: 6 };
const COMPACT = { top: 4, bottom: 3, side: 3 };
/** Roughly this many day labels: one every `ceil((n - 1) / 5)` days, plus the last. */
const DAY_STEPS = 5;
/**
 * Tooltip metrics. Text cannot be measured without layout, so its width is estimated from the
 * character count at `--text-xs` in the mono face the values are set in; the minimum keeps a short
 * tooltip from looking cramped.
 */
const TIP = { pad: 8, row: 16, char: 6.6, swatch: 8, gap: 16, offset: 12, min: 120 };

/**
 * Gridline values from 0 to a round maximum, in about three steps of 1, 2 or 5 × 10ⁿ. When every
 * value is whole (runs, merges) a step is never fractional: half a run is not a reading.
 */
export function areaTicks(top: number, whole: boolean): number[] {
  const peak = top > 0 ? top : 1;
  const raw = peak / 3;
  const power = 10 ** Math.floor(Math.log10(raw));
  let step = [1, 2, 5, 10].map((factor) => factor * power).find((candidate) => candidate >= raw) ?? 10 * power;
  if (whole) step = Math.max(1, Math.ceil(step));
  const max = Math.ceil(peak / step - 1e-9) * step;
  const ticks: number[] = [];
  for (let k = 0; k * step <= max + 1e-9; k += 1) ticks.push(Number((k * step).toPrecision(12)));
  return ticks;
}

/** Which days get a label: every `step`-th from the first, and the last, minus one crowding it. */
export function dayLabelIndices(n: number): number[] {
  if (n <= 0) return [];
  const step = Math.max(1, Math.ceil((n - 1) / DAY_STEPS));
  const picked: number[] = [];
  for (let index = 0; index < n - 1; index += step) picked.push(index);
  // The last day always speaks (it may say `today`); the label before it yields if they would touch.
  if (picked.length > 1 && n - 1 - picked[picked.length - 1] < step * 0.75) picked.pop();
  if (n > 1 || picked.length === 0) picked.push(n - 1);
  return picked;
}

interface TipRow {
  key: string;
  label: string;
  value: string;
  className?: string;
  kind: "part" | "total" | "apart";
}

/** The tooltip's rows for one day: its non-zero parts in `keys` order, a `Total`, then `apart`. */
function tipRows(datum: ChartDatum, total: number, keys: ChartKey[], format: (value: number) => string, apart?: string): TipRow[] {
  const sums = new Map<string, number>();
  for (const part of datum.parts) sums.set(part.key, (sums.get(part.key) ?? 0) + part.value);
  // Keys the caller named, in its order; then any key it did not name, so the rows still add up to the total.
  const order = [
    ...keys.map((key) => key.key),
    ...[...sums.keys()].filter((key) => !keys.some((known) => known.key === key)),
  ].filter((key) => key !== apart);
  const labelOf = (key: string) => keys.find((candidate) => candidate.key === key)?.label ?? key;

  const parts: TipRow[] = order
    .filter((key) => (sums.get(key) ?? 0) !== 0)
    .map((key) => ({ key, label: labelOf(key), value: format(sums.get(key) ?? 0), className: seriesClass(keys, key), kind: "part" }));
  const rows = [...parts];
  if (parts.length > 1) rows.push({ key: "__total__", label: "Total", value: format(total), kind: "total" });
  const aside = apart === undefined ? 0 : (sums.get(apart) ?? 0);
  if (apart !== undefined && aside !== 0) {
    rows.push({ key: apart, label: labelOf(apart), value: format(aside), className: seriesClass(keys, apart), kind: "apart" });
  }
  return rows;
}

const clamp = (value: number, low: number, high: number) => Math.min(Math.max(value, low), high);

export function AreaChart({ data, keys, label, format, empty, tone, apart, height, compact = false }: AreaChartProps) {
  const { ref, width } = useWidth<HTMLElement>();
  // `useId` answers with characters (`:`, `«`) that are not safe inside `url(#…)`.
  const gradientId = `ui-area-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const [at, setAt] = useState<number | null>(null);

  const svgHeight = height ?? (compact ? COMPACT_HEIGHT : HEIGHT);
  const svgWidth = Math.max(width, 1);
  const pad = compact ? COMPACT : FULL;
  const base = svgHeight - pad.bottom;
  const n = data.length;

  const totals = data.map((datum) => datum.parts.reduce((sum, part) => (part.key === apart ? sum : sum + part.value), 0));
  const top = Math.max(0, ...totals);
  const nothing = top === 0;
  const ticks = areaTicks(top, totals.every((total) => Number.isInteger(total)));
  const y = scaleLinear<number>({ domain: [0, ticks[ticks.length - 1]], range: [base, pad.top] });
  const x = scaleLinear<number>({ domain: [0, Math.max(n - 1, 1)], range: [pad.side, svgWidth - pad.side] });
  const xAt = (index: number) => (n <= 1 ? svgWidth / 2 : x(index));

  const points = totals.map((total, index) => ({ index, total }));
  type Point = (typeof points)[number];
  const partialTail = n >= 2 && data[n - 1].partial === true;
  const solid = partialTail ? points.slice(0, -1) : points;
  const shown = at === null || at >= n ? null : at;

  const indexAt = (event: PointerEvent<SVGRectElement>) => {
    if (n <= 1) return 0;
    const box = event.currentTarget.getBoundingClientRect();
    // The svg is drawn 1:1, but if CSS ever scales it, a client pixel is not a user unit.
    const scale = box.width > 0 ? svgWidth / box.width : 1;
    const along = (event.clientX - box.left) * scale;
    return clamp(Math.round(x.invert(along)), 0, n - 1);
  };

  const onKeyDown = (event: KeyboardEvent<SVGRectElement>) => {
    const from = shown ?? n - 1;
    const next: Record<string, number | null> = {
      ArrowLeft: Math.max(0, from - 1),
      ArrowRight: Math.min(n - 1, from + 1),
      Home: 0,
      End: n - 1,
      Escape: null,
    };
    if (!(event.key in next)) return;
    event.preventDefault();
    setAt(next[event.key]);
  };

  const dayText = (index: number) => (index === n - 1 && data[index].partial === true ? "today" : data[index].day.slice(5));

  let tip: ReactNode = null;
  let said = "";
  if (!compact && !nothing && shown !== null) {
    const datum = data[shown];
    const rows = tipRows(datum, totals[shown], keys, format, apart);
    const head = `${datum.day}${datum.partial === true ? " · so far" : ""}`;
    const widest = Math.max(
      head.length * TIP.char,
      ...rows.map((row) => TIP.swatch + 6 + row.label.length * TIP.char + TIP.gap + row.value.length * TIP.char),
    );
    const tipWidth = Math.max(TIP.min, Math.ceil(widest + TIP.pad * 2));
    const tipHeight = TIP.pad * 2 + TIP.row * (rows.length + 1);
    const cx = xAt(shown);
    const cy = y(totals[shown]);
    // Beside the crosshair, never on it: to the right, or to the left near the right edge.
    const right = cx + TIP.offset;
    const left = right + tipWidth > svgWidth ? Math.max(0, cx - TIP.offset - tipWidth) : right;
    const down = clamp(cy - tipHeight / 2, 0, Math.max(0, svgHeight - tipHeight));
    const baseline = (row: number) => TIP.pad + TIP.row * row + 12;
    said = `${head}: ${rows.map((row) => `${row.label} ${row.value}`).join(", ")}`;
    tip = (
      <g className="ui-area-hover">
        <line className="ui-area-crosshair" x1={cx} x2={cx} y1={pad.top} y2={base} />
        <circle className="ui-area-dot" cx={cx} cy={cy} r={4} />
        <g className="ui-area-tip" transform={`translate(${left}, ${down})`}>
          <rect className="ui-area-tip-bg" width={tipWidth} height={tipHeight} rx={6} />
          <text className="ui-area-tip-head" x={TIP.pad} y={baseline(0)}>
            {head}
          </text>
          {rows.map((row, k) => (
            <g key={row.key} className={`ui-area-tip-row ui-area-tip-${row.kind}`}>
              {row.className === undefined ? null : (
                <rect
                  className={`ui-area-tip-swatch ${row.className}`}
                  x={TIP.pad}
                  y={baseline(k + 1) - TIP.swatch}
                  width={TIP.swatch}
                  height={TIP.swatch}
                  rx={2}
                />
              )}
              <text className="ui-area-tip-label" x={TIP.pad + TIP.swatch + 6} y={baseline(k + 1)}>
                {row.label}
              </text>
              <text className="ui-area-tip-value" x={tipWidth - TIP.pad} y={baseline(k + 1)} textAnchor="end">
                {row.value}
              </text>
            </g>
          ))}
        </g>
      </g>
    );
  }

  const svgClass = `ui-chart-svg ui-area-svg ${tone}${compact ? " ui-area-compact" : ""}`;
  const a11y = compact ? { "aria-hidden": true as const } : { role: "img", "aria-label": label };
  const last = points[n - 1];

  return (
    <figure ref={ref} className="ui-chart ui-area">
      <svg className={svgClass} width={svgWidth} height={svgHeight} viewBox={`0 0 ${svgWidth} ${svgHeight}`} {...a11y}>
        <defs>
          <linearGradient id={gradientId} className={`ui-area-gradient ${tone}`} x1="0" x2="0" y1="0" y2="1">
            <stop className="ui-area-stop-top" offset="0" />
            <stop className="ui-area-stop-bottom" offset="1" />
          </linearGradient>
        </defs>
        {compact
          ? null
          : ticks.map((tick, k) => {
              // Half a pixel off the integer so a 1px line lands on one row of pixels, not two.
              const row = Math.round(y(tick)) + 0.5;
              return (
                <g key={tick}>
                  <line className="ui-area-grid" x1={0} x2={svgWidth} y1={row} y2={row} />
                  {k === 0 || nothing ? null : (
                    <text className="ui-area-tick" x={0} y={row - 5}>
                      {format(tick)}
                    </text>
                  )}
                </g>
              );
            })}
        {nothing ? null : (
          <>
            <AreaClosed<Point>
              className="ui-area-fill"
              data={points}
              x={(point) => xAt(point.index)}
              y={(point) => y(point.total)}
              yScale={y}
              curve={curveMonotoneX}
              fill={`url(#${gradientId})`}
            />
            <LinePath<Point>
              className="ui-area-line"
              data={solid}
              x={(point) => xAt(point.index)}
              y={(point) => y(point.total)}
              curve={curveMonotoneX}
            />
            {partialTail ? (
              <LinePath<Point>
                className="ui-area-line ui-area-partial"
                data={points.slice(-2)}
                x={(point) => xAt(point.index)}
                y={(point) => y(point.total)}
              />
            ) : null}
            {compact && last !== undefined ? (
              <circle className="ui-area-end" cx={xAt(last.index)} cy={y(last.total)} r={2.5} />
            ) : null}
          </>
        )}
        {compact
          ? null
          : dayLabelIndices(n).map((index) => {
              const anchor = index === 0 ? "start" : index === n - 1 ? "end" : "middle";
              const along = anchor === "start" ? 0 : anchor === "end" ? svgWidth : xAt(index);
              return (
                <text key={data[index].day} className="ui-area-day" x={along} y={svgHeight - 5} textAnchor={anchor}>
                  {dayText(index)}
                </text>
              );
            })}
        {tip}
        {compact || nothing ? null : (
          <rect
            className="ui-area-overlay"
            x={0}
            y={0}
            width={svgWidth}
            height={svgHeight}
            fill="transparent"
            tabIndex={0}
            aria-label={`${label}: use the arrow keys to read each day`}
            onPointerMove={(event) => setAt(indexAt(event))}
            onPointerLeave={() => setAt(null)}
            onFocus={() => setAt((current) => current ?? n - 1)}
            onBlur={() => setAt(null)}
            onKeyDown={onKeyDown}
          />
        )}
      </svg>
      {compact ? null : (
        <p className="sr-only" aria-live="polite">
          {said}
        </p>
      )}
      {nothing && !compact ? <p className="ui-chart-empty">{empty}</p> : null}
    </figure>
  );
}
