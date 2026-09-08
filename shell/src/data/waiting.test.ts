import { describe, expect, it } from "vitest";
import { countWaitingDecisions } from "./waiting";

describe("countWaitingDecisions", () => {
  it("the queue's arithmetic is the six decision lists", () => {
    expect(
      countWaitingDecisions({
        wheel: undefined,
        approvals: undefined,
        teamActions: undefined,
        recruits: undefined,
        merges: undefined,
        exclusions: undefined,
      }),
    ).toBeUndefined();
    expect(
      countWaitingDecisions({
        wheel: undefined,
        approvals: undefined,
        teamActions: [],
        recruits: undefined,
        merges: undefined,
        exclusions: undefined,
      }),
    ).toBe(0);
    expect(
      countWaitingDecisions({
        wheel: [{}],
        approvals: [{}, {}],
        teamActions: [],
        recruits: [{}],
        merges: [],
        exclusions: [{}],
      }),
    ).toBe(5);
  });
});
