// @vitest-environment node
import { describe, expect, it } from "vitest";
import {
  RESOLVE_MIN_AGREE_PERCENT,
  RESOLVE_MIN_REVIEWED,
  outcomesFor,
  readOutcome,
  readReadiness,
  type ResolveReadiness,
} from "./autopilot";

describe("the resolver's words", () => {
  it("offers each event only the outcomes spec B gives it", () => {
    expect(outcomesFor("hard_deny")).toEqual(["deny", "warn", "stop"]);
    expect(outcomesFor("park")).toEqual(["explain", "park", "stop"]);
    expect(outcomesFor("gate_failed")).toEqual(["correction", "owner"]);
  });

  it("names every outcome in the owner's words", () => {
    expect(readOutcome("explain")).toBe("carry on without it");
    expect(readOutcome("owner")).toBe("hand it to me");
    expect(readOutcome("stop")).toBe("stop the run");
  });

  it("says what stands between a project and enforce, with the núcleo's bar", () => {
    expect([RESOLVE_MIN_REVIEWED, RESOLVE_MIN_AGREE_PERCENT]).toEqual([10, 90]);
    const readiness = (overrides: Partial<ResolveReadiness>): ResolveReadiness => ({
      reviewed: 10, agree: 10, less_cautious: 0, ready: true, ...overrides,
    });
    expect(readReadiness(readiness({}))).toBe("10 reviewed, 10 agree — ready");
    expect(readReadiness(readiness({ reviewed: 4, agree: 4, ready: false }))).toBe("4 of 10 reviews");
    expect(readReadiness(readiness({ agree: 8, ready: false }))).toBe("8 of 10 agree — under 90%");
    expect(readReadiness(readiness({ less_cautious: 1, ready: false }))).toBe(
      "1 review where the judge was less careful than you — not ready",
    );
  });
});
