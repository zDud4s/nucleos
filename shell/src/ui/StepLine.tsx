import { AxisBottom, AxisLeft, type TickRendererProps } from "@visx/axis";
import { curveStepAfter } from "@visx/curve";
import { Group } from "@visx/group";
import { scaleLinear, scaleUtc } from "@visx/scale";
import { LinePath } from "@visx/shape";

import { seriesClass } from "./chart-series";

/**
 * A fraction over continuous time, in steps (spec §9.10, decided: its own component, not a mode of
 * the day charts). Built for the quota history: each reading holds until the next one — never
 * interpolated — and the last one holds until `to`. Before the first point there is nothing, which
 * is the honest gap of a history that only records while the app is open (spec §7, §13.6).
 *
 * The scale is fixed at 0–1 (the caller's `format` writes it as a percentage); the chart rules
 * apply: mandatory `label`, a `<title>` per point, no inline style, axes kept when empty.
 *
 * **No inline style, anywhere** — the CSP gate refuses `style=`. That is why the axes never draw
 * their own text: `@visx/axis` renders tick labels through `@visx/text`, whose `Text` wraps every
 * label in an `<svg style="overflow: visible">`. `ChartTickLabel` replaces it with a plain
 * `<text>`, and no axis is ever given a `label` (an empty one skips `Text` entirely).
 */

export interface StepPoint {
  /** Epoch milliseconds. */
  at: number;
  /** In [0, 1]. */
  value: number;
  stale?: boolean;
}

export interface StepSeries {
  key: string;
  label: string;
  /** Ascending by `at`. */
  points: StepPoint[];
  className?: string;
}

export interface StepLineProps {
  series: StepSeries[];
  /** Start of the time axis, epoch ms. */
  from: number;
  /** End of the time axis, epoch ms (now). */
  to: number;
  label: string;
  format: (value: number) => string;
  empty: string;
  width?: number;
  height?: number;
}

const WIDTH = 640;
const HEIGHT = 160;
const MARGIN = { top: 8, right: 8, bottom: 22, left: 44 };

/** A tick label as a bare `<text>`: what `@visx/text` would draw, minus the `style` attribute. */
export function ChartTickLabel({ x, y, dx, dy, textAnchor, formattedValue }: TickRendererProps) {
  return (
    <text x={x} y={y} dx={dx} dy={dy} textAnchor={textAnchor} className="ui-chart-tick-label">
      {formattedValue}
    </text>
  );
}

/** `2026-10-11 08:00 UTC` — the axis and every title speak UTC, like the rest of the view. */
function utcMinute(at: number): string {
  return `${new Date(at).toISOString().slice(0, 16).replace("T", " ")} UTC`;
}

export function StepLine({ series, from, to, label, format, empty, width = WIDTH, height = HEIGHT }: StepLineProps) {
  const innerWidth = width - MARGIN.left - MARGIN.right;
  const innerHeight = height - MARGIN.top - MARGIN.bottom;
  const x = scaleUtc<number>({ domain: [new Date(from), new Date(to)], range: [0, innerWidth] });
  const y = scaleLinear<number>({ domain: [0, 1], range: [innerHeight, 0] });
  const keys = series.map((one) => ({ key: one.key, label: one.label, className: one.className }));
  const nothing = series.every((one) => one.points.length === 0);

  return (
    <figure className="ui-chart">
      <svg className="ui-chart-svg" viewBox={`0 0 ${width} ${height}`} role="img" aria-label={label}>
        <Group left={MARGIN.left} top={MARGIN.top}>
          {series.map((one) => {
            if (one.points.length === 0) return null;
            // Nothing after `to` is drawn; a reading from before `from` still holds into the window, so it
            // starts at the left edge. Values are clamped to the scale: the svg does not clip.
            const upTo = one.points.filter((point) => point.at <= to);
            if (upTo.length === 0) return null;
            const last = upTo[upTo.length - 1];
            // The last reading holds until now: a step has to end somewhere, and "now" is where it is still true.
            const held = [...upTo, { at: to, value: last.value }].map((point) => ({
              ...point,
              at: Math.max(point.at, from),
              value: Math.min(Math.max(point.value, 0), 1),
            }));
            const inWindow = upTo.filter((point) => point.at >= from);
            const tone = seriesClass(keys, one.key);
            return (
              <g key={one.key} className={tone}>
                <LinePath<StepPoint>
                  className="ui-step-line"
                  data={held}
                  x={(point) => x(point.at)}
                  y={(point) => y(point.value)}
                  curve={curveStepAfter}
                />
                {inWindow.map((point) => (
                  <g key={point.at} className={point.stale === true ? "ui-step-point ui-step-stale" : "ui-step-point"}>
                    <title>{`${one.label} · ${utcMinute(point.at)}: ${format(point.value)}${point.stale === true ? " (stale)" : ""}`}</title>
                    <circle className="ui-step-dot" cx={x(point.at)} cy={y(Math.min(Math.max(point.value, 0), 1))} r={2.5} />
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
          tickValues={[0, 0.5, 1]}
          tickFormat={(value) => format(Number(value))}
          axisClassName="ui-chart-axis"
          tickClassName="ui-chart-tick"
          tickComponent={ChartTickLabel}
        />
        <AxisBottom
          left={MARGIN.left}
          top={MARGIN.top + innerHeight}
          scale={x}
          numTicks={6}
          tickFormat={(value) => new Date(Number(value)).toISOString().slice(5, 10)}
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
