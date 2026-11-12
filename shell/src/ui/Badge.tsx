import type { ReactNode } from "react";

/**
 * The app's whole colour vocabulary for state.
 *
 * Seven, and closed. Every state machine in the núcleo has to land on one of
 * these, which is the point: a page that wants an eighth colour is a page
 * inventing a meaning nobody else in the app shares. The mapping from a
 * domain's states to these tones lives in one table (`state-map.ts`), never at
 * a call site.
 */
export type BadgeTone = "active" | "shadow" | "off" | "pending" | "paused" | "danger" | "info";

export interface BadgeProps {
  tone: BadgeTone;
  children: ReactNode;
  /** Extra classes for the rare caller that needs to mark its badge; not a styling hook. */
  className?: string;
  /** Hover text. Used by `StateBadge` to explain a state it has no reading for. */
  title?: string;
}

/**
 * A small coloured word.
 *
 * Text carries the meaning and colour only reinforces it — the label is never
 * a bare dot or a coloured square. Roughly one in twelve of the people who
 * would use this app cannot separate the red from the green, and a status they
 * cannot read is worse than no status at all.
 */
export function Badge({ tone, children, className, title }: BadgeProps) {
  return (
    <span className={`ui-badge ui-badge-${tone}${className === undefined ? "" : ` ${className}`}`} title={title}>
      {children}
    </span>
  );
}
