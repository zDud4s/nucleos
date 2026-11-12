/**
 * The pure geometry under `ForceGraph`: which node a pointer is on, and when a label is drawn.
 *
 * Kept out of the component so it is tested without a canvas: everything here is arithmetic on
 * world coordinates (the simulation's) and a d3-zoom transform.
 */

/** A node where the simulation put it, in world units, with its drawn radius at zoom 1. */
export interface Placed {
  id: string;
  x: number;
  y: number;
  r: number;
}

/** The shape of a d3-zoom `ZoomTransform`: screen = world * k + (x, y). */
export interface Transform {
  k: number;
  x: number;
  y: number;
}

/** A screen point (CSS pixels, relative to the canvas) back into world units. */
export function toWorld(p: { x: number; y: number }, t: Transform): { x: number; y: number } {
  return { x: (p.x - t.x) / t.k, y: (p.y - t.y) / t.k };
}

/**
 * The node under a screen point, or null.
 *
 * A node is hit when the point is within its radius plus `slop` SCREEN pixels — so the forgiveness
 * stays the same size under the finger whatever the zoom. Of several hits the nearest wins, and a
 * tie goes to the last in the array, which is the one drawn on top.
 */
export function hitTest(
  nodes: Placed[],
  screen: { x: number; y: number },
  t: Transform,
  slop = 4,
): string | null {
  const w = toWorld(screen, t);
  const reach = slop / t.k;
  let best: string | null = null;
  let bestDistance = Infinity;
  for (const n of nodes) {
    const d = Math.hypot(n.x - w.x, n.y - w.y);
    if (d <= n.r + reach && d <= bestDistance) {
      best = n.id;
      bestDistance = d;
    }
  }
  return best;
}

/** The zoom from which every label shows; below it only the hovered and focused ones do. */
export const LABEL_ZOOM = 1.4;

/** Whether a node's label is drawn: always for the hovered or focused one, always when compact. */
export function labelVisible(k: number, hovered: boolean, focused: boolean, compact: boolean): boolean {
  if (hovered || focused) return true;
  if (compact) return true;
  return k >= LABEL_ZOOM;
}
