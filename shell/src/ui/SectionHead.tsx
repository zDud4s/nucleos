import type { ReactNode } from "react";

export interface SectionHeadProps {
  title: string;
  /** A small figure after the title (how many rows the section holds). */
  count?: ReactNode;
  /** The right end of the row: an action, or a link out. */
  aside?: ReactNode;
}

/**
 * The header row of a compact section inside a panel: the section title, an optional count
 * and, pushed to the right, an optional action. One row, so several sections stack without
 * each spending a paragraph on a heading.
 */
export function SectionHead({ title, count, aside }: SectionHeadProps) {
  return (
    <div className="ui-section-head">
      <h3 className="ui-section-title">{title}</h3>
      {count === undefined ? null : <span className="ui-section-head-count">{count}</span>}
      {aside === undefined ? null : <div className="ui-section-head-aside">{aside}</div>}
    </div>
  );
}
