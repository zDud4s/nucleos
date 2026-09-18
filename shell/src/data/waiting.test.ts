import { describe, expect, it } from "vitest";
import { countWaitingDecisions } from "./waiting";

describe("countWaitingDecisions", () => {
  it("the queue's arithmetic is the seven decision lists", () => {
    expect(
      countWaitingDecisions({
        wheel: undefined,
        approvals: undefined,
        teamActions: undefined,
        recruits: undefined,
        merges: undefined,
        exclusions: undefined,
        git: undefined,
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
        git: undefined,
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
        git: [{ status: "escalated" }, { status: "blocked" }, { status: "succeeded" }],
      }),
    ).toBe(7);
  });

  it("counts only git rows that want a person", () => {
    expect(
      countWaitingDecisions({
        wheel: [], approvals: [], teamActions: [], recruits: [], merges: [], exclusions: [],
        git: [{ status: "escalated" }, { status: "blocked" }, { status: "succeeded" }, { status: "failed" }],
      }),
    ).toBe(2);
  });

  it("does not add parked runs, which are the run side of an action approval", () => {
    expect(
      countWaitingDecisions({
        wheel: [], approvals: [{}], teamActions: [], recruits: [], merges: [], exclusions: [], git: [],
      }),
    ).toBe(1);
  });
});
