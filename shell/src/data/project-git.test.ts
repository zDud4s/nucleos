// @vitest-environment node
import { describe, expect, it } from "vitest";
import { standingOf, STANDING_TONE, type BranchRow } from "./project-git";

function branch(overrides: Partial<BranchRow> = {}): BranchRow {
  return {
    name: "feature",
    ahead: 0,
    behind: 0,
    measured: true,
    last_commit_at: "2026-08-23T09:00:00Z",
    last_subject: "a commit",
    ...overrides,
  };
}

describe("standingOf", () => {
  it("names the integration branch as itself rather than as a branch that is level with it", () => {
    // Both would read `0/0`, and only one of them is a *place*. "Level with master" said about
    // master is a sentence that makes somebody read it twice.
    expect(standingOf(branch({ name: "master" }), "master")).toBe("integration");
  });

  /**
   * The distinction the whole `measured` flag exists for. `0/0` means *identical to where work
   * lands*; unmeasured means *nobody knows*. A panel that drew the second as the first would report
   * every branch as up to date at exactly the moment the measurement stopped working.
   */
  it("keeps an unmeasured branch apart from one that is level", () => {
    expect(standingOf(branch({ measured: false }), "master")).toBe("unmeasured");
    expect(standingOf(branch({ measured: true }), "master")).toBe("level");
  });

  it("separates ahead, behind and diverged, which want three different next moves", () => {
    expect(standingOf(branch({ ahead: 3 }), "master")).toBe("ahead");
    expect(standingOf(branch({ behind: 3 }), "master")).toBe("behind");
    // The one that matters: diverged looks like ahead until somebody tries to fast-forward.
    expect(standingOf(branch({ ahead: 3, behind: 2 }), "master")).toBe("diverged");
  });

  it("calls every branch unmeasured when there is no integration branch to measure against", () => {
    expect(standingOf(branch({ measured: false }), null)).toBe("unmeasured");
  });

  /**
   * Neither of these is a fault, and neither should be drawn as one. `off` for unmeasured says
   * "nothing is known" without saying "something is broken", and `info` for the integration branch
   * says "this is the place" without congratulating it.
   */
  it("gives no standing a tone that reads as an alarm", () => {
    expect(STANDING_TONE.unmeasured).toBe("off");
    expect(STANDING_TONE.integration).toBe("info");
    expect(Object.values(STANDING_TONE)).not.toContain("danger");
  });
});
