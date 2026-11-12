/**
 * The in-svg tooltip `AreaChart` and `StepLine` share: a head line over rows of swatch, label and
 * value, beside a crosshair at `cx`, never on it.
 *
 * Drawn inside the svg for the reason every chart here gives: an HTML tooltip would need a computed
 * `left`/`top`, which is exactly the inline style the CSP gate refuses; a `transform` attribute is
 * not. Text cannot be measured without layout, so the width is estimated from the character count
 * at `--text-xs` in the mono face the values are set in; the minimum keeps a short tooltip from
 * looking cramped.
 */

export const TIP = { pad: 8, row: 16, char: 6.6, swatch: 8, gap: 16, offset: 12, min: 120 };

export interface TipRow {
  key: string;
  label: string;
  value: string;
  /** The series class colouring the swatch; no class, no swatch. */
  className?: string;
  /** Adds `ui-area-tip-<kind>` to the row (`total` is set in bold). */
  kind?: string;
  /** The swatch as a short dashed stroke, for a series drawn as a dashed line. */
  dashed?: boolean;
}

export const clamp = (value: number, low: number, high: number) => Math.min(Math.max(value, low), high);

/** What the live region says for one hovered point: the head, then every row. */
export function tipSentence(head: string, rows: TipRow[]): string {
  return `${head}: ${rows.map((row) => `${row.label} ${row.value}`).join(", ")}`;
}

export interface ChartTipProps {
  head: string;
  rows: TipRow[];
  /** The crosshair, and the height the tooltip centres on. */
  cx: number;
  cy: number;
  /** The svg's size, which the tooltip never leaves. */
  width: number;
  height: number;
}

export function ChartTip({ head, rows, cx, cy, width, height }: ChartTipProps) {
  const widest = Math.max(
    head.length * TIP.char,
    ...rows.map((row) => TIP.swatch + 6 + row.label.length * TIP.char + TIP.gap + row.value.length * TIP.char),
  );
  const tipWidth = Math.max(TIP.min, Math.ceil(widest + TIP.pad * 2));
  const tipHeight = TIP.pad * 2 + TIP.row * (rows.length + 1);
  // Beside the crosshair, never on it: to the right, or to the left near the right edge.
  const right = cx + TIP.offset;
  const left = right + tipWidth > width ? Math.max(0, cx - TIP.offset - tipWidth) : right;
  const down = clamp(cy - tipHeight / 2, 0, Math.max(0, height - tipHeight));
  const baseline = (row: number) => TIP.pad + TIP.row * row + 12;

  return (
    <g className="ui-area-tip" transform={`translate(${left}, ${down})`}>
      <rect className="ui-area-tip-bg" width={tipWidth} height={tipHeight} rx={6} />
      <text className="ui-area-tip-head" x={TIP.pad} y={baseline(0)}>
        {head}
      </text>
      {rows.map((row, k) => (
        <g key={row.key} className={`ui-area-tip-row${row.kind === undefined ? "" : ` ui-area-tip-${row.kind}`}`}>
          {row.className === undefined ? null : row.dashed === true ? (
            <line
              className={`ui-area-tip-dash ${row.className}`}
              x1={TIP.pad}
              x2={TIP.pad + TIP.swatch}
              y1={baseline(k + 1) - TIP.swatch / 2}
              y2={baseline(k + 1) - TIP.swatch / 2}
            />
          ) : (
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
  );
}
