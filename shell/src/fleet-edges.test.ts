import { describe, expect, it } from "vitest";

import { edgeGeometry } from "./fleet-edges";
import type { ExclusionEdge } from "./fleet-derive";

const SIZE = { width: 200, height: 100 };

describe("where an exclusion line goes", () => {
  /** Middle to middle, so the line reads as joining the two cards and not their corners. */
  it("joins the middle of one node to the middle of the other", () => {
    const edges: ExclusionEdge[] = [{ low: 41, high: 42, state: "active", id: 3 }];
    const positions = { "job:41": { x: 0, y: 0 }, "job:42": { x: 400, y: 200 } };

    expect(edgeGeometry(edges, positions, SIZE)).toEqual([
      { id: 3, state: "active", from: { x: 100, y: 50 }, to: { x: 500, y: 250 } },
    ]);
  });

  /**
   * An edge naming a job that is not on the canvas is not drawn.
   *
   * It happens on an ordinary tick: one of the two jobs ends while the rule is still in force, and
   * the exclusion outlives the card by however long the poll takes. A line to a card that is not
   * there points at nothing — and, worse, would make the drawing the thing that decides who exists,
   * when only the daemon knows.
   */
  it("draws nothing for an edge whose node is gone", () => {
    const edges: ExclusionEdge[] = [
      { low: 41, high: 42, state: "active", id: 3 },
      { low: 41, high: 99, state: "pending", id: 4 },
      { low: 98, high: 42, state: "pending", id: 5 },
    ];
    const positions = { "job:41": { x: 0, y: 0 }, "job:42": { x: 400, y: 200 } };

    expect(edgeGeometry(edges, positions, SIZE).map((line) => line.id)).toEqual([3]);
  });

  /**
   * The line carries the id and the state the card carries.
   *
   * Pending and active are two drawings — dashed and solid — for the same reason the cards draw
   * them apart: a request has changed nothing about how either job is scheduled, and a line that
   * claimed otherwise would have somebody wondering why both jobs are still running.
   */
  it("keeps which rule it is and which of its two lives", () => {
    const edges: ExclusionEdge[] = [{ low: 41, high: 42, state: "pending", id: 9 }];
    const positions = { "job:41": { x: 10, y: 10 }, "job:42": { x: 20, y: 20 } };

    const [line] = edgeGeometry(edges, positions, SIZE);
    expect(line.id).toBe(9);
    expect(line.state).toBe("pending");
  });

  /** A run holds a slot but is never one end of an exclusion: the daemon's rules name jobs. */
  it("does not mistake a run for the job that shares its number", () => {
    const edges: ExclusionEdge[] = [{ low: 41, high: 7, state: "active", id: 3 }];
    const positions = { "job:41": { x: 0, y: 0 }, "run:7": { x: 400, y: 200 } };

    expect(edgeGeometry(edges, positions, SIZE)).toEqual([]);
  });
});
