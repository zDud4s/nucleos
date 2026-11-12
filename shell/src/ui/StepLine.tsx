import { curveStepAfter } from "@visx/curve";
import { scaleLinear } from "@visx/scale";
import { AreaClosed, LinePath } from "@visx/shape";
import { useId, useState, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";

import { dayLabelIndices } from "./AreaChart";
import { seriesClass } from "./chart-series";
import { ChartTip, TIP, clamp, tipSentence, type TipRow } from "./chart-tip";
import { useWidth } from "./useWidth";

/**
 * A fraction over continuous time, in steps (spec §9.10, decided: its own component, not a mode of
 * the day charts). Built for the quota history: each reading holds until the next one — never
 * interpolated — and the last one holds until `to`. Before the first point there is nothing, which
 * is the honest gap of a history that only records while the app is open (spec §7, §13.6).
 *
 * It speaks `AreaChart`'s visual language: drawn at its measured width (`useWidth`), so a label set
 * at `--text-xs` stays `--text-xs`; only faint gridlines at 0, 50 and 100 % with the value just above
 * each at the left, no axis lines; at most six day labels, the first anchored at the start and the
 * last — the day `to` falls on, which is now — reading `today` at the end.
 *
 * **One subject, the rest context.** The first series is drawn as a solid step line over a soft
 * vertical gradient; every other one as a dashed line, muted unless it brings a class of its own
 * (the third onwards fall back to the categorical colours, so two context lines stay apart). The
 * legend under the chart repeats that: a solid swatch for the first, a dashed stroke for the rest.
 * A reading the provider marked stale is a hollow dot.
 *
 * **Hover and keyboard** move between reading times, not pixels: the crosshair snaps to the nearest
 * reading, and the tooltip lists the value every series holds at that instant. Arrow keys, Home and
 * End do the same from the keyboard, and a polite live region says what the tooltip shows.
 *
 * **No inline style, anywhere** — the CSP gate refuses `style=`. Colour reaches every mark through
 * `--series`, set by a series class; the gradient's stops read it in `ui.css`. No `@visx/axis` and no
 * `@visx/text` (which wraps each label in an `<svg style="overflow: visible">`): every label is a
 * bare `<text>`, and the tooltip is drawn inside the svg (`chart-tip.tsx`).
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
  /** End of the time axis, epoch ms: now, so its day is labelled `today`. */
  to: number;
  label: string;
  format: (value: number) => string;
  empty: string;
  height?: number;
}

const HEIGHT = 160;
/** Room above the top gridline for its label, below the plot for the day labels, and at the sides for a dot. */
const PAD = { top: 16, bottom: 22, side: 6 };
const GRID = [0, 0.5, 1];
const DAY = 24 * 3600 * 1000;
/** The least room between two day labels. */
const LABEL_GAP = 8;

const unit = (value: number) => clamp(value, 0, 1);

/** `10-11 08:00 UTC` — the tooltip speaks UTC, like the rest of the view. */
function utcStamp(at: number): string {
  return `${new Date(at).toISOString().slice(5, 16).replace("T", " ")} UTC`;
}

/** The reading a series holds at `at`: its last point at or before it, if any. */
function heldAt(points: StepPoint[], at: number): StepPoint | undefined {
  let held: StepPoint | undefined;
  for (const point of points) {
    if (point.at > at) break;
    held = point;
  }
  return held;
}

/** Each series' class: the first its own or the first categorical colour, the second muted, the rest categorical. */
function seriesTones(series: StepSeries[]): string[] {
  const keys = series.map((one) => ({ key: one.key, label: one.label, className: one.className }));
  return series.map((one, index) => (index === 1 ? (one.className ?? "ui-chart-muted") : seriesClass(keys, one.key)));
}

interface DayLabel {
  day: number;
  text: string;
  x: number;
  anchor: "start" | "middle" | "end";
  left: number;
  right: number;
}

/**
 * The day labels along the bottom: `dayLabelIndices` picks the days, then any label that would touch
 * its neighbour yields — time is continuous here, so `today` at the right edge can sit closer to the
 * midnight before it than a day's width.
 */
function dayLabels(from: number, to: number, x: (at: number) => number, width: number): DayLabel[] {
  const days: number[] = [];
  for (let day = Math.floor(from / DAY) * DAY; day <= to; day += DAY) days.push(day);
  const n = days.length;
  const labels = dayLabelIndices(n).map((index): DayLabel => {
    const text = index === n - 1 ? "today" : new Date(days[index]).toISOString().slice(5, 10);
    const anchor = index === n - 1 ? "end" : index === 0 ? "start" : "middle";
    const along = anchor === "start" ? 0 : anchor === "end" ? width : x(days[index]);
    const span = text.length * TIP.char;
    const left = anchor === "start" ? along : anchor === "end" ? along - span : along - span / 2;
    return { day: days[index], text, x: along, anchor, left, right: left + span };
  });
  const kept: DayLabel[] = [];
  labels.forEach((label, k) => {
    if (k === labels.length - 1) {
      // The last day always speaks; the labels before it yield, though never the first.
      while (kept.length > 1 && kept[kept.length - 1].right + LABEL_GAP > label.left) kept.pop();
      kept.push(label);
    } else if (kept.length === 0 || kept[kept.length - 1].right + LABEL_GAP <= label.left) {
      kept.push(label);
    }
  });
  return kept;
}

export function StepLine({ series, from, to, label, format, empty, height = HEIGHT }: StepLineProps) {
  const { ref, width } = useWidth<HTMLElement>();
  // `useId` answers with characters (`:`, `«`) that are not safe inside `url(#…)`.
  const gradientId = `ui-step-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const [at, setAt] = useState<number | null>(null);

  const svgWidth = Math.max(width, 1);
  const base = height - PAD.bottom;
  const end = Math.max(to, from + 1);
  const x = scaleLinear<number>({ domain: [from, end], range: [PAD.side, svgWidth - PAD.side] });
  const y = scaleLinear<number>({ domain: [0, 1], range: [base, PAD.top] });
  const tones = seriesTones(series);

  // Nothing after `to` is drawn; of the readings before `from`, only the last still holds into the
  // window, and it starts at the left edge. The last reading holds until now: a step has to end
  // somewhere, and "now" is where it is still true.
  const lines = series.map((one) => {
    const upTo = one.points.filter((point) => point.at <= to);
    const inWindow = upTo.filter((point) => point.at >= from);
    const before = heldAt(upTo, from - 1);
    const kept = before === undefined ? inWindow : [before, ...inWindow];
    const last = kept[kept.length - 1];
    const held =
      last === undefined
        ? []
        : [...kept, { at: to, value: last.value }].map((point) => ({ at: Math.max(point.at, from), value: unit(point.value) }));
    return { held, inWindow };
  });
  const nothing = lines.every((line) => line.held.length === 0);

  // The instants the crosshair stops at: every reading in the window, once.
  const times = [...new Set(lines.flatMap((line) => line.inWindow.map((point) => point.at)))].sort((a, b) => a - b);
  const shown = at === null || at >= times.length ? null : at;

  const nearest = (instant: number) => {
    let best = 0;
    for (let k = 1; k < times.length; k += 1) {
      if (Math.abs(times[k] - instant) < Math.abs(times[best] - instant)) best = k;
    }
    return best;
  };

  const indexAt = (event: PointerEvent<SVGRectElement>) => {
    const box = event.currentTarget.getBoundingClientRect();
    // The svg is drawn 1:1, but if CSS ever scales it, a client pixel is not a user unit.
    const scale = box.width > 0 ? svgWidth / box.width : 1;
    return nearest(x.invert((event.clientX - box.left) * scale));
  };

  const onKeyDown = (event: KeyboardEvent<SVGRectElement>) => {
    const last = times.length - 1;
    const current = shown ?? last;
    const next: Record<string, number | null> = {
      ArrowLeft: Math.max(0, current - 1),
      ArrowRight: Math.min(last, current + 1),
      Home: 0,
      End: last,
      Escape: null,
    };
    if (!(event.key in next)) return;
    event.preventDefault();
    setAt(next[event.key]);
  };

  let tip: ReactNode = null;
  let said = "";
  if (shown !== null) {
    const instant = times[shown];
    const held = series.map((one, index) => ({ one, index, point: heldAt(one.points, instant) }));
    const rows: TipRow[] = held.flatMap(({ one, index, point }) =>
      point === undefined
        ? []
        : [
            {
              key: one.key,
              label: one.label,
              value: `${format(unit(point.value))}${point.stale === true ? " (stale)" : ""}`,
              className: tones[index],
              dashed: index > 0,
            },
          ],
    );
    const head = utcStamp(instant);
    const cx = x(instant);
    const anchor = held.find((entry) => entry.point !== undefined)?.point;
    const cy = anchor === undefined ? (base + PAD.top) / 2 : y(unit(anchor.value));
    said = tipSentence(head, rows);
    tip = (
      <g className="ui-area-hover">
        <line className="ui-area-crosshair" x1={cx} x2={cx} y1={PAD.top} y2={base} />
        {held.map(({ one, index, point }) =>
          point === undefined ? null : (
            <circle key={one.key} className={`ui-area-dot ${tones[index]}`} cx={cx} cy={y(unit(point.value))} r={4} />
          ),
        )}
        <ChartTip head={head} rows={rows} cx={cx} cy={cy} width={svgWidth} height={height} />
      </g>
    );
  }

  type Held = (typeof lines)[number]["held"][number];

  return (
    <figure ref={ref} className="ui-chart ui-step">
      <svg className="ui-chart-svg ui-step-svg" width={svgWidth} height={height} viewBox={`0 0 ${svgWidth} ${height}`} role="img" aria-label={label}>
        <defs>
          <linearGradient id={gradientId} className={`ui-step-gradient ${tones[0] ?? ""}`} x1="0" x2="0" y1="0" y2="1">
            <stop className="ui-area-stop-top" offset="0" />
            <stop className="ui-area-stop-bottom" offset="1" />
          </linearGradient>
        </defs>
        {GRID.map((tick) => {
          // Half a pixel off the integer so a 1px line lands on one row of pixels, not two.
          const row = Math.round(y(tick)) + 0.5;
          return (
            <g key={tick}>
              <line className="ui-area-grid" x1={0} x2={svgWidth} y1={row} y2={row} />
              {tick === 0 ? null : (
                <text className="ui-area-tick" x={0} y={row - 5}>
                  {format(tick)}
                </text>
              )}
            </g>
          );
        })}
        {series.map((one, index) => {
          const { held, inWindow } = lines[index];
          if (held.length === 0) return null;
          return (
            <g key={one.key} className={tones[index]}>
              {index === 0 ? (
                <AreaClosed<Held>
                  className="ui-step-fill"
                  data={held}
                  x={(point) => x(point.at)}
                  y={(point) => y(point.value)}
                  yScale={y}
                  curve={curveStepAfter}
                  fill={`url(#${gradientId})`}
                />
              ) : null}
              <LinePath<Held>
                className={index === 0 ? "ui-step-line" : "ui-step-line ui-step-dashed"}
                data={held}
                x={(point) => x(point.at)}
                y={(point) => y(point.value)}
                curve={curveStepAfter}
              />
              {inWindow.map((point) => (
                <circle
                  key={point.at}
                  className={point.stale === true ? "ui-step-dot ui-step-stale" : "ui-step-dot"}
                  cx={x(point.at)}
                  cy={y(unit(point.value))}
                  r={2.5}
                />
              ))}
            </g>
          );
        })}
        {dayLabels(from, end, x, svgWidth).map((day) => (
          <text key={day.day} className="ui-area-day" x={day.x} y={height - 5} textAnchor={day.anchor}>
            {day.text}
          </text>
        ))}
        {tip}
        {times.length === 0 ? null : (
          <rect
            className="ui-area-overlay"
            x={0}
            y={0}
            width={svgWidth}
            height={height}
            fill="transparent"
            tabIndex={0}
            aria-label={`${label}: use the arrow keys to step through the readings`}
            onPointerMove={(event) => setAt(indexAt(event))}
            onPointerLeave={() => setAt(null)}
            onFocus={() => setAt((current) => current ?? times.length - 1)}
            onBlur={() => setAt(null)}
            onKeyDown={onKeyDown}
          />
        )}
      </svg>
      <p className="sr-only" aria-live="polite">
        {said}
      </p>
      {nothing ? <p className="ui-chart-empty">{empty}</p> : null}
      {series.length === 0 ? null : (
        <ul className="ui-share-keys ui-step-keys" aria-label={`${label}, series`}>
          {series.map((one, index) => {
            const latest = heldAt(one.points, to);
            return (
              <li key={one.key} className="ui-share-key">
                <span aria-hidden="true" className={`${index === 0 ? "ui-chart-swatch" : "ui-step-key-dash"} ${tones[index]}`} />
                <span className="ui-share-label">{one.label}</span>
                {latest === undefined ? null : (
                  <span className="ui-share-value">
                    {format(unit(latest.value))}{" "}
                    <small className="ui-share-pct">{latest.stale === true ? "latest, stale" : "latest"}</small>
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </figure>
  );
}
