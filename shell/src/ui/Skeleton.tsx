export interface SkeletonProps {
  /** Spoken to a screen reader, and the visible caption: what is loading. */
  label: string;
  /** How many text lines of height to reserve. Defaults to one. */
  lines?: number;
}

/**
 * A placeholder that holds the height the content will take.
 *
 * `role="status"` + `aria-busy`: a polite announcement that something is on its
 * way. The reserved height (`--skeleton-lines`) is what stops the layout from
 * jumping when the data lands; a bare italic "loading…" reserves nothing.
 */
export function Skeleton({ label, lines = 1 }: SkeletonProps) {
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
