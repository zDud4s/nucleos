import { describe, expect, it } from "vitest";
import { countWaitingDecisions, sumWaitingCount } from "./waiting";

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

describe("sumWaitingCount", () => {
  const none = {
    wheel: null,
    approvals: null,
    team_actions: null,
    recruits: null,
    merges: null,
    exclusions: null,
    git: null,
  };

  it("is no answer when every list failed, as with no list at all", () => {
    expect(sumWaitingCount(none)).toBeUndefined();
  });

  it("sums what was counted and treats a failed list as nothing", () => {
    expect(sumWaitingCount({ ...none, approvals: 2, git: 1, merges: 0 })).toBe(3);
  });
});
