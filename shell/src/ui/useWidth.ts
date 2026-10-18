import { useCallback, useEffect, useState } from "react";

/**
 * The content width of an element, in CSS pixels, kept current by a `ResizeObserver`.
 *
 * Built for charts that draw at their real size: a `viewBox` equal to the measured box means one
 * unit is one pixel, so a label set at `--text-xs` stays `--text-xs` at any width instead of
 * scaling with the box the way a fixed 640-wide `viewBox` does.
 *
 * The ref is a callback ref held in state rather than a `useRef`, so an element that mounts later
 * (behind a condition) is still observed. Until something is measured — the first frame, a box not
 * laid out yet, or an environment with no `ResizeObserver` at all (happy-dom) — the answer is
 * `fallback`. Zero is what an unlaid box reports, and is never passed on: a chart zero pixels wide
 * would compute every position as NaN-adjacent nonsense.
 */
export function useWidth<T extends Element = HTMLElement>(fallback = 640): { ref: (node: T | null) => void; width: number } {
  const [node, setNode] = useState<T | null>(null);
  const [measured, setMeasured] = useState<number | null>(null);
  const ref = useCallback((next: T | null) => setNode(next), []);

  useEffect(() => {
    if (node === null || typeof ResizeObserver === "undefined") return;
    const watch = new ResizeObserver((entries) => {
      const box = entries[entries.length - 1]?.contentRect;
      // Whole pixels: a fractional width would re-render on sub-pixel jitter for no visible change.
      const next = box === undefined ? 0 : Math.floor(box.width);
      if (next > 0) setMeasured(next);
    });
    watch.observe(node);
    return () => watch.disconnect();
  }, [node]);

  return { ref, width: measured ?? fallback };
}
