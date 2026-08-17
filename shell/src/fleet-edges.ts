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

/**
 * Whether a line pulled from one job to another could become a rule.
 *
 * The three refusals are the daemon's own, answered here so the gesture never ends in one. A line
 * that can only finish in a 409 is the defect the visual pass already caught once in the columns:
 * an action offered where it cannot succeed teaches the reader that the screen shows things that
 * were never available.
 *
 * - **Itself.** `pair()` in the core returns `None` for `a == b`.
 * - **Another project.** The core answers `DifferentProjects`.
 * - **A pair that already has an edge**, in force or merely asked about. The core refuses a second
 *   request for a pair that already has a rule, and a second question is not worth asking either.
 */
export function pairable(
  from: number,
  to: number,
  sameProject: boolean,
  edges: ExclusionEdge[],
): boolean {
  if (from === to || !sameProject) return false;
  const low = Math.min(from, to);
  const high = Math.max(from, to);
  return !edges.some((edge) => edge.low === low && edge.high === high);
}
