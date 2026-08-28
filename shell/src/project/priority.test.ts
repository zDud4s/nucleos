// §spec mapa-do-projeto
import { describe, expect, it } from "vitest";
import { CONCERN_ORDER, leadingConcern, type ProjectConcerns } from "./priority";

const calm: ProjectConcerns = {
  killSwitch: false,
  budgetPaused: false,
  openProposals: 0,
  failedGatesWithoutRescue: 0,
  interruptedRuns: 0,
  workflowDrift: false,
};

describe("leadingConcern", () => {
  it("is calm when nothing is wrong", () => {
    expect(leadingConcern(calm).kind).toBe("calm");
  });

  it("puts the kill switch above everything, including a waiting proposal", () => {
    expect(leadingConcern({ ...calm, killSwitch: true, openProposals: 3 }).kind).toBe("kill-switch");
  });

  it("puts a paused budget above a waiting proposal", () => {
    expect(leadingConcern({ ...calm, budgetPaused: true, openProposals: 3 }).kind).toBe(
      "budget-paused",
    );
  });

  it("puts a waiting proposal above a failed gate", () => {
    expect(leadingConcern({ ...calm, openProposals: 1, failedGatesWithoutRescue: 2 }).kind).toBe(
      "proposal-waiting",
    );
  });

  it("puts a failed gate above an interrupted run", () => {
    expect(leadingConcern({ ...calm, failedGatesWithoutRescue: 1, interruptedRuns: 4 }).kind).toBe(
      "gate-failed",
    );
  });

  it("puts an interrupted run above unresolved workflow drift", () => {
    expect(leadingConcern({ ...calm, interruptedRuns: 1, workflowDrift: true }).kind).toBe(
      "run-interrupted",
    );
  });

  it("puts workflow drift last of the concerns, and still above calm", () => {
    expect(leadingConcern({ ...calm, workflowDrift: true }).kind).toBe("workflow-drift");
  });

  /**
   * The whole point of the ladder is that exactly one thing leads. A tie broken
   * by whichever branch happened to be written first is a page whose top line
   * changes when somebody reorders an `if`.
   */
  it("names one leader when everything is wrong at once", () => {
    const everything: ProjectConcerns = {
      killSwitch: true,
      budgetPaused: true,
      openProposals: 9,
      failedGatesWithoutRescue: 9,
      interruptedRuns: 9,
      workflowDrift: true,
    };
    expect(leadingConcern(everything).kind).toBe("kill-switch");
  });

  /**
   * A count is a fact the page shows; the ladder decides which fact leads and
   * must hand over the number rather than making the caller look it up again
   * against a different rule.
   */
  it("carries the count of whatever leads", () => {
    expect(leadingConcern({ ...calm, openProposals: 3 }).count).toBe(3);
    expect(leadingConcern({ ...calm, failedGatesWithoutRescue: 2 }).count).toBe(2);
    // Calm and the two switches are not countable, and a zero would read as one.
    expect(leadingConcern(calm).count).toBeNull();
    expect(leadingConcern({ ...calm, killSwitch: true }).count).toBeNull();
  });

  /**
   * The ladder is data, and the order in it is the design's §4.1 verbatim. A
   * reordering should have to happen here, in a list somebody reads, and not by
   * moving a branch in a function.
   */
  it("keeps the ladder in the design's order, calm last", () => {
    expect(CONCERN_ORDER).toEqual([
      "kill-switch",
      "budget-paused",
      "proposal-waiting",
      "gate-failed",
      "run-interrupted",
      "workflow-drift",
    ]);
  });

  /**
   * Not a real number. A project the shell has not heard about yet has no
   * concerns *and* no evidence of calm, and the difference is the one §12 keeps
   * insisting on: not measured never reads as measured and fine.
   */
  it("does not call a project calm before anything has been read", () => {
    expect(leadingConcern(null).kind).toBe("unknown");
  });
});
