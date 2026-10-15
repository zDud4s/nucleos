// §spec atlas-sequence
import { describe, expect, it } from "vitest";
import { layoutSequence, readFlows, type FlowCheck, type FlowStep } from "./map-flows";

/** One verified step; each test overrides only what it is about. */
function step(participant: string, fn: string, over: Partial<FlowStep> = {}): FlowStep {
  return {
    participant,
    path: `core/src/${participant}.rs`,
    fn,
    via: null,
    from: null,
    line: 10,
    ok: true,
    reason: null,
    ...over,
  };
}

function flow(steps: FlowStep[], over: Partial<FlowCheck> = {}): FlowCheck {
  return { name: "land", stale: false, broken_step: null, steps, ...over };
}

describe("the sequence layout", () => {
  it("orders participants by first appearance and draws arrows from the declared caller", () => {
    const layout = layoutSequence(
      flow([
        step("cli", "land"),
        step("http", "land_worktree", { via: "POST /vcs/land" }),
        step("land", "submit"),
        step("queue", "drain_once", { via: "async drain", from: 1 }),
        step("land", "publish"),
      ]),
    );

    // `land` appears twice and keeps the place of its first appearance.
    expect(layout.participants).toEqual(["cli", "http", "land", "queue"]);
    // One arrow per step after the first.
    expect(layout.arrows).toHaveLength(4);
    expect(layout.arrows.map((arrow) => arrow.step)).toEqual([1, 2, 3, 4]);
    expect(layout.arrows.map((arrow) => arrow.label)).toEqual([
      "land_worktree",
      "submit",
      "drain_once",
      "publish",
    ]);
    // Step 1 is called by step 0, step 2 by the previous step...
    expect([layout.arrows[0].from, layout.arrows[0].to]).toEqual([0, 1]);
    expect([layout.arrows[1].from, layout.arrows[1].to]).toEqual([1, 2]);
    // ...but step 3 declares `from: 1`, so its arrow leaves step 1's lifeline, not step 2's.
    expect([layout.arrows[2].from, layout.arrows[2].to]).toEqual([1, 3]);
    // Step 4 is a return to a lifeline that already exists, called by the previous step.
    expect([layout.arrows[3].from, layout.arrows[3].to]).toEqual([3, 2]);
    // The kind comes from `via`: none is a direct call, `async` a queue hop, a route an HTTP hop.
    expect(layout.arrows.map((arrow) => arrow.kind)).toEqual(["http", "call", "async", "call"]);
    expect(layout.arrows.every((arrow) => !arrow.broken && !arrow.unchecked)).toBe(true);
  });

  it("marks the broken step and reads a malformed answer as no flows", () => {
    const layout = layoutSequence(
      flow(
        [
          step("cli", "land"),
          step("http", "land_worktree"),
          step("land", "submit", { ok: false, line: null, reason: "http never calls submit" }),
          step("queue", "drain_once"),
        ],
        { stale: true, broken_step: 2 },
      ),
    );
    expect(layout.arrows.map((arrow) => arrow.broken)).toEqual([false, true, false]);
    // What comes after the break was never checked, and the layout says so.
    expect(layout.arrows.map((arrow) => arrow.unchecked)).toEqual([false, false, true]);

    // `apiFetch` is a cast, and unknown GETs answer `[]`: none of these may throw.
    const none = { file: ".nucleos/flows.yaml", declared: false, root: "", flows: [], error: null };
    expect(readFlows([])).toEqual(none);
    expect(readFlows(null)).toEqual(none);
    expect(readFlows("nope")).toEqual(none);
    expect(readFlows({ flows: "not a list" })).toEqual(none);

    const answer = {
      file: ".nucleos/flows.yaml",
      declared: true,
      root: "C:/repo",
      flows: [flow([step("a", "x"), step("b", "y")])],
      error: null,
    };
    expect(readFlows(answer).flows).toHaveLength(1);
    expect(readFlows(answer).root).toBe("C:/repo");
  });
});
