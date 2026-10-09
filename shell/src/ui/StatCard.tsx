import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";

export interface StatCardProps {
  /** What is being counted. Also the card's accessible name. */
  label: string;
  /**
   * The figure.
   *
   * `undefined` is not zero, and the two must never render the same way: a
   * count that has not been read yet becomes an em dash, because "no proposals
   * are waiting" and "we have not managed to ask" are different pieces of news
   * and only one of them means you can go to lunch.
   */
  value: ReactNode | undefined;
  /** The line under the figure: a split, a ceiling, a trend. */
  detail?: ReactNode;
  /** A bar under the reading, for a figure that runs against a ceiling. */
  bar?: ReactNode;
  /** Makes the whole card a link to this route; the card's label names the link. */
  to?: string;
  /** The detail shown while `value` is `undefined`: why the figure could not be read. */
  unread?: ReactNode;
}

/**
 * A number, what it counts, and one line of context.
 *
 * The Home page is five of these and nothing else, which is the point: the
 * first screen is a *reading*, not a console. A card may link to the page that holds the
 * figure (`to`); nothing behind one mutates.
 *
 * No tone of its own. A card whose figure is bad news says so by wrapping its `detail` in
 * `.ui-wrong` at the call site, which is what every other wrong clause in the app does. The
 * `tone="danger"` prop that used to paint the figure is gone: one piece of news wearing two
 * treatments is the thing round 9 fixed on the boundary readout, and `6/10` is a reading
 * that is true either way.
 */
export function StatCard({ label, value, detail, bar, to, unread }: StatCardProps) {
  const unreadFigure = value === undefined;
  const shown = unreadFigure && unread !== undefined ? unread : detail;
  const card = (
    <article className="ui-stat" aria-label={label}>
      <p className="ui-stat-value">
        {unreadFigure ? (
          <>
            <span aria-hidden="true">—</span>
            <span className="sr-only">not read</span>
          </>
        ) : (
          value
        )}
      </p>
      <p className="ui-stat-label">{label}</p>
      {shown === undefined ? null : <p className="ui-stat-detail">{shown}</p>}
      {bar === undefined ? null : bar}
    </article>
  );
  if (to === undefined) return card;
  return (
    <Link to={to} className="ui-stat-link" aria-label={label}>
      {card}
    </Link>
  );
}
