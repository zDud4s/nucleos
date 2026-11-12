import { useLayoutEffect, useRef } from "react";
import { DUR, EASE } from "@/lib/motion";

export interface SlideMarkProps {
  /** Shared by every item of one strip, and unique per strip (`useId`), so two strips never trade it. */
  layoutId: string;
  /** `rule` is the tab strip's 2px underline; `segment` is the pressed segment's raised ground and rule. */
  kind: "rule" | "segment";
}

/** The last rect each strip's mark was laid out at, so the next mount can slide in from it. */
const lastRect = new Map<string, DOMRect>();

/**
 * The "this is the current one" mark of a tab strip or a segmented track, as one element that
 * moves rather than one rule per item that switches on and off.
 *
 * Every item renders this only while it is the active one, with the same `layoutId`. The mark that
 * unmounts leaves its rect in a module map; the one that mounts reads it and plays a FLIP from
 * there (a `translateX` + `scaleX` back to none, so it travels by transform and costs the layout
 * nothing). The item's own reserved 2px border stays transparent, so nothing shifts whichever way
 * the mark is drawn.
 *
 * Hand-written with `element.animate()` instead of motion's `layoutId`: motion's shared layout
 * leaves a `style` attribute (`opacity: 1;`, then transforms) on the element, and the app forbids
 * inline styles in rendered markup (production CSP is `style-src 'self'`, see `WorkflowCanvas`).
 * The Web Animations API writes none. The origin is set in `ui.css`.
 *
 * Decorative: `aria-hidden`, and `pointer-events: none` in `ui.css`. The state is still carried by
 * `aria-selected` / `aria-current` / `aria-pressed` on the item; this only draws it.
 *
 * The global `prefers-reduced-motion` clamp in `base.css` does not reach the Web Animations API,
 * so the effect asks for itself: a still mark jumps. A different row (vertical move) jumps too.
 * The curve and duration are the app's single ones (`lib/motion.ts`, DESIGN.md "Motion").
 */
export function SlideMark({ layoutId, kind }: SlideMarkProps) {
  const ref = useRef<HTMLSpanElement>(null);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const next = el.getBoundingClientRect();
    const prev = lastRect.get(layoutId);
    lastRect.set(layoutId, next);

    const still =
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (
      prev &&
      !still &&
      typeof el.animate === "function" &&
      prev.top === next.top &&
      next.width > 0 &&
      (prev.left !== next.left || prev.width !== next.width)
    ) {
      el.animate(
        [
          { transform: `translateX(${prev.left - next.left}px) scaleX(${prev.width / next.width})` },
          { transform: "none" },
        ],
        { duration: DUR.slide * 1000, easing: `cubic-bezier(${EASE.join(", ")})` },
      );
    }

    return () => {
      // Leave the rect the mark has now, not the one it mounted at: a resize or a reflow since
      // would otherwise slide the successor in from where the strip used to be.
      const last = el.isConnected ? el.getBoundingClientRect() : next;
      lastRect.set(layoutId, last);
      // Forget the strip once it is really gone: a successor mounts in the same commit and
      // replaces the entry before this microtask runs.
      queueMicrotask(() => {
        if (lastRect.get(layoutId) === last) lastRect.delete(layoutId);
      });
    };
  }, [layoutId]);

  return (
    <span
      ref={ref}
      aria-hidden="true"
      className={kind === "rule" ? "ui-slide-mark ui-slide-mark-rule" : "ui-slide-mark ui-slide-mark-segment"}
    />
  );
}
