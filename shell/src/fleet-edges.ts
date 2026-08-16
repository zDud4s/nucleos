/**
 * Where the exclusion lines go on the canvas.
 *
 * Its own module, and pure, because it is arithmetic — and arithmetic inside a component is
 * arithmetic nobody tests. The component is then left with the drawing, which is the part only a
 * browser can judge.
 */

import type { ExclusionEdge } from "./fleet-derive";
import type { Layout, Point } from "./fleet-layout";

/** One line to draw, and which rule it is. */
export interface DrawnEdge {
  /** The proposal while pending, the rule once active — the same id the card's buttons carry. */
  id: number;
  state: "pending" | "active";
  from: Point;
  to: Point;
}

/**
 * The lines, from the middle of one node to the middle of the other.
 *
 * `size` is the NOMINAL size of a node and not its real one, which is fine only because the cards
 * are painted over the lines: the last stretch of every line runs under a card, so an anchor that
 * is a few tens of pixels off is an anchor nobody can see. Measuring the real heights would mean
 * reading layout on every frame of a drag, to fix something invisible.
 *
 * **An edge naming a job that is not on the canvas is not drawn.** Same principle as pruning the
 * layout: what exists is the daemon's to say, and a line to a card that is not there is a line
 * pointing at nothing. It happens on any ordinary tick — one of the two jobs ends while the rule is
 * still in force, and the exclusion outlives the card by however long the poll takes.
 */
export function edgeGeometry(
  edges: ExclusionEdge[],
  positions: Layout,
  size: { width: number; height: number },
): DrawnEdge[] {
  const middle = (jobId: number): Point | null => {
    const at = positions[`job:${jobId}`];
    return at === undefined ? null : { x: at.x + size.width / 2, y: at.y + size.height / 2 };
  };

  const drawn: DrawnEdge[] = [];
  for (const edge of edges) {
    const from = middle(edge.low);
    const to = middle(edge.high);
    if (from === null || to === null) continue;
    drawn.push({ id: edge.id, state: edge.state, from, to });
  }
  return drawn;
}
