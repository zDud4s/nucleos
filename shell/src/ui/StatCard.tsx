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
}

/**
 * A number, what it counts, and one line of context.
 *
 * The Home page is four of these and nothing else, which is the point: the
 * first screen is a *reading*, not a console. Nothing on a stat card is
 * clickable and nothing behind one mutates.
 */
export function StatCard({ label, value, detail }: StatCardProps) {
  return (
    <article className="ui-stat" aria-label={label}>
      <p className="ui-stat-value">{value === undefined ? "—" : value}</p>
      <p className="ui-stat-label">{label}</p>
      {detail === undefined ? null : <p className="ui-stat-detail">{detail}</p>}
    </article>
  );
}
