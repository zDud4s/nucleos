export interface SkeletonProps {
  /** Spoken to a screen reader, and the visible caption: what is loading. */
  label: string;
  /** How many text lines of height to reserve. Defaults to one. */
  lines?: number;
  /**
   * `"lines"` (default) reserves text lines; `"cards"` reserves a grid of card-shaped blocks, for
   * a page whose content is a wall of cards rather than a paragraph.
   */
  variant?: "lines" | "cards";
  /** How many card blocks to draw, for `variant="cards"`. Defaults to three. */
  count?: number;
}

/**
 * A placeholder that holds the height the content will take.
 *
 * `role="status"` + `aria-busy`: a polite announcement that something is on its
 * way. The reserved height (`--skeleton-lines`) is what stops the layout from
 * jumping when the data lands; a bare italic "loading…" reserves nothing.
 */
export function Skeleton({ label, lines = 1, variant = "lines", count = 3 }: SkeletonProps) {
  if (variant === "cards") {
    return (
      <div className="ui-skeleton ui-skeleton-cards" role="status" aria-busy="true">
        <span className="ui-skeleton-label">{label}</span>
        <div className="ui-skeleton-grid" aria-hidden="true">
          {Array.from({ length: Math.max(1, count) }, (_, i) => (
            <span key={i} className="ui-skeleton-card" />
          ))}
        </div>
      </div>
    );
  }
  return (
    <div
      className="ui-skeleton"
      role="status"
      aria-busy="true"
      style={{ ["--skeleton-lines" as string]: String(lines) }}
    >
      <span className="ui-skeleton-label">{label}</span>
    </div>
  );
}
