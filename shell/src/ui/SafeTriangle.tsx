import { useCallback, useEffect, useRef } from "react";

/**
 * SafeTriangle (#20): pointer-cone intent detection for submenus and flyouts.
 *
 * When the pointer leaves a trigger heading for an open flyout, closing it the instant the pointer
 * crosses a neighbouring row is the classic "diagonal" annoyance. `useSafeTriangle` keeps the
 * flyout open while the pointer travels inside the triangle (last pointer position → the two near
 * corners of the flyout rect), and closes it once the pointer leaves the triangle or stalls.
 *
 *   const st = useSafeTriangle({ getRect: () => flyoutEl.getBoundingClientRect(), onClose });
 *   <Row onPointerLeave={st.onTriggerLeave} onPointerMove={st.onTriggerMove} />
 *   // call st.cancel() when the pointer enters the flyout.
 */
export interface Point {
  x: number;
  y: number;
}
export interface RectLike {
  left: number;
  right: number;
  top: number;
  bottom: number;
}

function sign(p: Point, a: Point, b: Point): number {
  return (p.x - b.x) * (a.y - b.y) - (a.x - b.x) * (p.y - b.y);
}

/** Whether `p` lies inside (or on the edge of) triangle `a`,`b`,`c`. */
export function pointInTriangle(p: Point, a: Point, b: Point, c: Point): boolean {
  const d1 = sign(p, a, b);
  const d2 = sign(p, b, c);
  const d3 = sign(p, c, a);
  const neg = d1 < 0 || d2 < 0 || d3 < 0;
  const pos = d1 > 0 || d2 > 0 || d3 > 0;
  return !(neg && pos);
}

/**
 * Whether moving from `from` to `to` heads into the cone toward `rect` on the given side.
 * The cone apex is `from`; its base is the flyout's near edge.
 */
export function movingTowardRect(
  from: Point,
  to: Point,
  rect: RectLike,
  side: "right" | "left" = "right",
): boolean {
  const x = side === "right" ? rect.left : rect.right;
  return pointInTriangle(to, from, { x, y: rect.top }, { x, y: rect.bottom });
}

export interface SafeTriangleOptions {
  /** The flyout's rect, read lazily at leave time. `null` → no flyout, close at once. */
  getRect: () => RectLike | null;
  onClose: () => void;
  side?: "right" | "left";
  /** How long the pointer may stall inside the cone before the flyout closes. Default 300ms. */
  delay?: number;
}

export function useSafeTriangle({ getRect, onClose, side = "right", delay = 300 }: SafeTriangleOptions) {
  const last = useRef<Point | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const rectRef = useRef<RectLike | null>(null);
  const apex = useRef<Point | null>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  const clear = useCallback(() => {
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = null;
  }, []);

  const cancel = useCallback(() => {
    clear();
    rectRef.current = null;
    apex.current = null;
  }, [clear]);

  const arm = useCallback(() => {
    clear();
    timer.current = setTimeout(() => {
      cancel();
      closeRef.current();
    }, delay);
  }, [clear, cancel, delay]);

  const onTriggerMove = useCallback((e: { clientX: number; clientY: number }) => {
    last.current = { x: e.clientX, y: e.clientY };
  }, []);

  const onTriggerLeave = useCallback(
    (e: { clientX: number; clientY: number }) => {
      const rect = getRect();
      if (rect === null) {
        closeRef.current();
        return;
      }
      rectRef.current = rect;
      apex.current = last.current ?? { x: e.clientX, y: e.clientY };
      arm();
    },
    [getRect, arm],
  );

  /** Wire to pointermove on the document while a flyout is open: keeps the grace alive inside the cone. */
  const onDocumentMove = useCallback(
    (e: { clientX: number; clientY: number }) => {
      const rect = rectRef.current;
      const from = apex.current;
      if (rect === null || from === null) return;
      const to = { x: e.clientX, y: e.clientY };
      if (movingTowardRect(from, to, rect, side)) arm();
      else {
        cancel();
        closeRef.current();
      }
    },
    [side, arm, cancel],
  );

  useEffect(() => cancel, [cancel]);

  return { onTriggerMove, onTriggerLeave, onDocumentMove, cancel };
}
