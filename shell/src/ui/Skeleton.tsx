export type SkeletonVariant =
  /** Reserves `lines` text lines in one padded bar (the default). */
  | "lines"
  /** A single text-height bar, `width` wide. */
  | "line"
  /** A rectangle of `height` (default 6rem): a chart, an image, a panel. */
  | "block"
  /** `count` list rows: a leading dot, a title bar and a short meta bar. */
  | "row"
  /** A grid of `count` card-shaped blocks. */
  | "cards"
  /** One card-shaped block. */
  | "card"
  /** A table: `count` body rows of `columns` cells under a header row. */
  | "table"
  /** A round avatar, `size` px. */
  | "avatar";

export interface SkeletonProps {
  /** Spoken to a screen reader, and the visible caption: what is loading. */
  label: string;
  /** How many text lines of height to reserve. Defaults to one. */
  lines?: number;
  /** The shape to draw; see {@link SkeletonVariant}. Defaults to `"lines"`. */
  variant?: SkeletonVariant;
  /** How many cards / rows / table rows to draw. Defaults to three (table: five). */
  count?: number;
  /** Table cells per row. Defaults to three. */
  columns?: number;
  /** CSS width of a `line` / `block`. */
  width?: string;
  /** CSS height of a `block`; for `row`, each row's height, drawn flush and ruled like a form's rows. */
  height?: string;
  /** Diameter in px of an `avatar`. Defaults to 32. */
  size?: number;
  /** Extra class on the `cards` grid, so it reuses the real grid's tracks (`height` is the card's min-height). */
  gridClassName?: string;
  /** Per-card min-heights of a `cards` grid (one card each, overriding `count` and `height`), for cards of unequal height. */
  heights?: string[];
}

const n = (value: number | undefined, fallback: number) => Math.max(1, Math.floor(value ?? fallback));

/**
 * A placeholder that holds the height the content will take.
 *
 * `role="status"` + `aria-busy`: a polite announcement that something is on its
 * way. The reserved height (`--skeleton-lines`) is what stops the layout from
 * jumping when the data lands; a bare italic "loading…" reserves nothing. The
 * shimmer is a CSS animation that `prefers-reduced-motion` clamps to nothing, so
 * under reduced motion the shapes are simply still. Shaped variants draw
 * `aria-hidden` bars under a visually hidden label; only `lines` shows the label.
 */
export function Skeleton({
  label,
  lines = 1,
  variant = "lines",
  count,
  columns,
  width,
  height,
  size = 32,
  gridClassName,
  heights,
}: SkeletonProps) {
  if (variant === "lines") {
    return (
      <div
        className="ui-skeleton ui-skeleton-textlines"
        role="status"
        aria-busy="true"
        style={{ ["--skeleton-lines" as string]: String(lines) }}
      >
        <span className="ui-skeleton-label sr-only">{label}</span>
        {Array.from({ length: n(lines, 1) }, (_, i) => (
          <span
            key={i}
            className="ui-skeleton-bar ui-skeleton-textline"
            aria-hidden="true"
            style={{ width: n(lines, 1) > 1 && i === n(lines, 1) - 1 ? "60%" : "100%" }}
          />
        ))}
      </div>
    );
  }

  let shape;
  switch (variant) {
    case "cards":
      shape = (
        <div
          className={gridClassName ? `ui-skeleton-grid ${gridClassName}` : "ui-skeleton-grid"}
          aria-hidden="true"
        >
          {Array.from({ length: heights ? heights.length : n(count, 3) }, (_, i) => {
            const h = heights ? heights[i] : height;
            return <span key={i} className="ui-skeleton-card" style={h ? { minHeight: h } : undefined} />;
          })}
        </div>
      );
      break;
    case "card":
      shape = <span className="ui-skeleton-card" aria-hidden="true" />;
      break;
    case "line":
      shape = <span className="ui-skeleton-bar" aria-hidden="true" style={width ? { width } : undefined} />;
      break;
    case "block":
      shape = (
        <span
          className="ui-skeleton-blockbox"
          aria-hidden="true"
          style={{ ...(width ? { width } : null), ...(height ? { height } : null) }}
        />
      );
      break;
    case "avatar":
      shape = (
        <span
          className="ui-skeleton-avatar"
          aria-hidden="true"
          style={{ width: `${size}px`, height: `${size}px` }}
        />
      );
      break;
    case "row":
      shape = (
        <div
          className={height ? "ui-skeleton-rows ui-skeleton-rows-ruled" : "ui-skeleton-rows"}
          aria-hidden="true"
        >
          {Array.from({ length: n(count, 3) }, (_, i) => (
            <span key={i} className="ui-skeleton-row" style={height ? { minHeight: height } : undefined}>
              <span className="ui-skeleton-avatar" style={{ width: "1.25rem", height: "1.25rem" }} />
              <span className="ui-skeleton-bar" style={{ flex: 1 }} />
              <span className="ui-skeleton-bar" style={{ width: "4rem" }} />
            </span>
          ))}
        </div>
      );
      break;
    case "table": {
      const cols = n(columns, 3);
      shape = (
        <div className="ui-skeleton-table" aria-hidden="true">
          {Array.from({ length: n(count, 5) + 1 }, (_, r) => (
            <span
              key={r}
              className={r === 0 ? "ui-skeleton-tr ui-skeleton-tr-head" : "ui-skeleton-tr"}
              style={{ gridTemplateColumns: `repeat(${cols}, 1fr)` }}
            >
              {Array.from({ length: cols }, (_, c) => (
                <span key={c} className="ui-skeleton-bar" />
              ))}
            </span>
          ))}
        </div>
      );
      break;
    }
  }

  return (
    <div className={`ui-skeleton ui-skeleton-shaped ui-skeleton-${variant}`} role="status" aria-busy="true">
      <span className="ui-skeleton-label sr-only">{label}</span>
      {shape}
    </div>
  );
}
