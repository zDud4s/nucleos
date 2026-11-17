import {
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ElementType,
  type ReactNode,
} from "react";

export interface ClampedTextProps {
  children: ReactNode;
  /** Visible lines while collapsed. Defaults to 3. */
  lines?: number;
  /** The element that holds the text. Defaults to a `div`. */
  as?: ElementType;
  /** Class for the text element itself (typography), not the wrapper. */
  className?: string;
  /**
   * Force the "overflows" answer instead of measuring. jsdom has no layout, so `scrollHeight`
   * and `clientHeight` are both 0 there; a test passes `true` to see the toggle.
   */
  overflowing?: boolean;
}

/**
 * Text cut to a few lines, with a "Show more" / "Show less" toggle to read the rest in place.
 *
 * The toggle exists only when the text really is cut, measured on the element in a layout
 * effect (the width decides the line count, and it is unknown until layout) and re-measured
 * when the box resizes. The answer is kept while expanded: once the clamp is lifted the text no
 * longer overflows, and a button that vanished on the click that opened it could not close it.
 * Whitespace is preserved (`pre-wrap`), so a multi-line command keeps its breaks.
 */
export function ClampedText({
  children,
  lines = 3,
  as: Tag = "div",
  className,
  overflowing,
}: ClampedTextProps) {
  const ref = useRef<HTMLElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [measured, setMeasured] = useState(false);
  const id = useId();

  useLayoutEffect(() => {
    const element = ref.current;
    if (element === null || expanded) return;
    const measure = () => setMeasured(element.scrollHeight > element.clientHeight + 1);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [children, lines, expanded]);

  const overflows = overflowing ?? measured;

  return (
    <div className="ui-clamped">
      <Tag
        ref={ref}
        id={id}
        className={["ui-clamped-text", className].filter(Boolean).join(" ")}
        data-expanded={expanded ? "true" : undefined}
        data-clamped={overflows && !expanded ? "true" : undefined}
        style={{ "--ui-clamp-lines": lines } as CSSProperties}
      >
        {children}
      </Tag>
      {overflows && (
        <button
          type="button"
          className="ui-clamped-toggle"
          aria-expanded={expanded}
          aria-controls={id}
          onClick={() => setExpanded((current) => !current)}
        >
          {expanded ? "Show less" : "Show more"}
        </button>
      )}
    </div>
  );
}
