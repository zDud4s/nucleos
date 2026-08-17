import type { ReactNode } from "react";

/**
 * `flat` drops the surface and keeps only the heading — for a block that is
 * already inside a panel. `dim` recedes: a panel that is present but not the
 * thing you came for, such as a read-only config readout under a control.
 */
export type PanelVariant = "flat" | "dim";

export interface PanelProps {
  /** Rendered as an `h2`. Omit for a panel that is a container, not a section. */
  title?: string;
  /**
   * The corner of the heading row: a count, a filter, a link out.
   *
   * Kept out of `children` so that every panel in the app puts its secondary
   * control in the same place, and so a heading row never becomes a layout
   * problem a page has to solve again.
   */
  aside?: ReactNode;
  variant?: PanelVariant;
  children: ReactNode;
}

/**
 * A titled block of the page.
 *
 * The unit almost every screen is assembled from. It exists so that section
 * headings, their spacing and the rule under them are decided once — a page
 * built from raw `div`s and `h2`s drifts within a week, and the drift shows up
 * as pages that feel subtly different for no reason anyone chose.
 */
export function Panel({ title, aside, variant, children }: PanelProps) {
  const classes = variant === undefined ? "ui-panel" : `ui-panel ui-panel-${variant}`;
  const hasHead = title !== undefined || aside !== undefined;
  return (
    <section className={classes}>
      {hasHead ? (
        <div className="ui-panel-head">
          {title === undefined ? null : <h2 className="ui-panel-title">{title}</h2>}
          {aside === undefined ? null : <div className="ui-panel-aside">{aside}</div>}
        </div>
      ) : null}
      <div className="ui-panel-body">{children}</div>
    </section>
  );
}
