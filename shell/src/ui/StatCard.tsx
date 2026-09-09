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
  /**
   * The figure is bad news, and should say so.
   *
   * A union with one member rather than `danger?: boolean`, so the second tone
   * this eventually needs arrives as a value and not as a second flag that can
   * contradict the first. Absent is the normal card, and normal is the default
   * because a page of readings where every figure is toned is a page where none
   * of them is.
   */
  tone?: "danger";
}

/**
 * A number, what it counts, and one line of context.
 *
 * The Home page is four of these and nothing else, which is the point: the
 * first screen is a *reading*, not a console. Nothing on a stat card is
 * clickable and nothing behind one mutates.
 */
export function StatCard({ label, value, detail, bar, tone }: StatCardProps) {
  return (
    <article
      className={tone === undefined ? "ui-stat" : `ui-stat ui-stat-${tone}`}
      aria-label={label}
    >
      <p className="ui-stat-value">{value === undefined ? "—" : value}</p>
      <p className="ui-stat-label">{label}</p>
      {detail === undefined ? null : <p className="ui-stat-detail">{detail}</p>}
      {bar === undefined ? null : bar}
    </article>
  );
}
