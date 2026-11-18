import type { ReactNode } from "react";
import { Tip } from "../ui";
import "./detail.css";

/**
 * The one frame of a channel's detail (System > Channels), shared by the sidecar channels
 * (`ChannelCard`) and the running configurations (Email, Voice, Calendar), so the two cannot drift:
 *
 * 1. a header: name, state, age and the about mark on the left, the actions on the right;
 * 2. `ChannelTiles`: a row of equal-width stat tiles;
 * 3. `ChannelCards`: a balanced grid of bordered cards, each a `ChannelCardBox`.
 *
 * The body is a plain column; whatever sits between the tiles and the cards (a note, a notice) stacks
 * in order.
 */
export function ChannelDetail({
  title,
  state,
  meta,
  about,
  actions,
  children,
}: {
  title: string;
  /** The state badge(s). */
  state?: ReactNode;
  /** A faint reading after the state, e.g. a relative time. */
  meta?: ReactNode;
  /** The about mark (a `Tip`). */
  about?: ReactNode;
  /** Ghost buttons, drawn at their natural width on the right. */
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="ui-panel chd">
      <div className="ui-panel-head chd-head">
        <div className="chd-id">
          <h2 className="ui-panel-title">{title}</h2>
          {state}
          {meta === undefined || meta === null ? null : <span className="chd-meta">{meta}</span>}
          {about}
        </div>
        {actions === undefined || actions === null ? null : <div className="chd-actions">{actions}</div>}
      </div>
      <div className="ui-panel-body chd-body">{children}</div>
    </section>
  );
}

/** One stat tile: a small uppercase term over a larger tabular value. */
export interface ChannelTile {
  term: string;
  value: ReactNode;
  /** An about mark inside the tile, after the value. */
  tip?: ReactNode;
}

/** The row of tiles: equal columns that fill the full width of the detail. */
export function ChannelTiles({ label, tiles }: { label: string; tiles: readonly ChannelTile[] }) {
  return (
    <dl className="chd-tiles" aria-label={label}>
      {tiles.map(({ term, value, tip }) => (
        <div key={term} className="chd-tile">
          <dt>{term}</dt>
          <dd>
            {value}
            {tip === undefined ? null : (
              <>
                {" "}
                <Tip name={`About ${term.toLowerCase()}`} persist tip={tip} />
              </>
            )}
          </dd>
        </div>
      ))}
    </dl>
  );
}

/** The grid the cards sit in: two columns when wide, one when narrow; cards in a row share a height. */
export function ChannelCards({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={className === undefined ? "chd-cards" : `chd-cards ${className}`}>{children}</div>;
}

/** A bordered sub-panel with a small heading (omitted when the card has no title). */
export function ChannelCardBox({
  title,
  wide = false,
  children,
}: {
  title?: string;
  /** Spans the whole row instead of one column. */
  wide?: boolean;
  children: ReactNode;
}) {
  return (
    <section className={wide ? "chd-card chd-card-wide" : "chd-card"}>
      {title === undefined ? null : <h3 className="chd-card-title">{title}</h3>}
      {children}
    </section>
  );
}
