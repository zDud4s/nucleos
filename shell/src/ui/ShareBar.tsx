import { useId } from "react";

import { seriesClass } from "./chart-series";

/**
 * Who took what share of a total, as one 100% bar and the legend under it.
 *
 * The breakdown `AreaChart` leaves out of its plot lands here: a thin bar split in proportion, and
 * per item its label, its value and its rounded percent. It is the chart's legend as well, so the
 * colour per item comes from `seriesClass` over the items taken as keys — the same call, with the
 * same keys in the same order, picks the same colour the chart's tooltip swatch shows.
 *
 * No inline style (the CSP gate refuses it): the bar is an svg `viewBox="0 0 1000 8"` stretched
 * with `preserveAspectRatio="none"`, so proportions are plain `x`/`width` attributes in thousandths,
 * and colour is a class setting `--series`. A zero item draws nothing and is not listed; a total of
 * zero renders nothing at all — an empty share bar says less than no bar.
 */

export interface ShareItem {
  key: string;
  label: string;
  value: number;
  /** A fixed series class; otherwise one of the eight by position, as in the chart. */
  className?: string;
}

export interface ShareBarProps {
  items: ShareItem[];
  format: (value: number) => string;
  /** The bar's accessible name. */
  label: string;
}

const SPAN = 1000;
const GAP = 2;

/** The rounded percent, except that a share too small to round up still reads as something. */
function percent(share: number): string {
  const rounded = Math.round(share * 100);
  return rounded === 0 && share > 0 ? "<1%" : `${rounded}%`;
}

export function ShareBar({ items, format, label }: ShareBarProps) {
  const clipId = `ui-share-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const keys = items.map(({ key, label: name, className }) => ({ key, label: name, className }));
  const shown = items.filter((item) => item.value > 0);
  const total = shown.reduce((sum, item) => sum + item.value, 0);
  if (total <= 0) return null;

  // The gaps come out of the span first, so the segments alone are in proportion.
  const room = SPAN - GAP * (shown.length - 1);
  let along = 0;
  const segments = shown.map((item) => {
    const width = (item.value / total) * room;
    const segment = { item, x: along, width };
    along += width + GAP;
    return segment;
  });

  return (
    <div className="ui-share">
      <svg
        className="ui-share-bar"
        viewBox={`0 0 ${SPAN} 8`}
        preserveAspectRatio="none"
        width="100%"
        height={8}
        role="img"
        aria-label={label}
      >
        <defs>
          <clipPath id={clipId}>
            <rect width={SPAN} height={8} rx={4} />
          </clipPath>
        </defs>
        <g clipPath={`url(#${clipId})`}>
          {segments.map(({ item, x, width }) => (
            <g key={item.key}>
              <title>{`${item.label}: ${format(item.value)} (${percent(item.value / total)})`}</title>
              <rect className={`ui-share-seg ${seriesClass(keys, item.key)}`} x={x} y={0} width={width} height={8} />
            </g>
          ))}
        </g>
      </svg>
      <ul className="ui-share-keys" aria-label={`${label}, by item`}>
        {shown.map((item) => (
          <li key={item.key} className="ui-share-key">
            <span aria-hidden="true" className={`ui-chart-swatch ${seriesClass(keys, item.key)}`} />
            <span className="ui-share-label">{item.label}</span>
            <span className="ui-share-value">
              {format(item.value)} <small className="ui-share-pct">{percent(item.value / total)}</small>
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
