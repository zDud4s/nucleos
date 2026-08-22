import { describe, expect, it } from "vitest";
import {
  compactTokens,
  efficiencyTrend,
  gateShare,
  humanMinutes,
  type Efficiency,
  type GateTally,
} from "./project-readings";

function gate(overrides: Partial<GateTally> = {}): GateTally {
  return { passed: 0, failed: 0, errored: 0, no_gate: 0, ...overrides };
}

function efficiency(overrides: Partial<Efficiency> = {}): Efficiency {
  return {
    measured_runs: 0,
    unmeasured_runs: 0,
    median_total_tokens: null,
    previous_median_total_tokens: null,
    ...overrides,
  };
}

describe("gateShare", () => {
  /**
   * The distinction this function exists for, and the reason it is a function at all rather than
   * arithmetic inside the card: `no_gate` must never be a cushion.
   */
  it("leaves ungated runs out of the denominator instead of letting them flatter the number", () => {
    const share = gateShare(gate({ passed: 0, failed: 2, no_gate: 48 }));

    expect(share.judged).toBe(2);
    // Fifty runs and two failures is not 96% green. It is nought out of two.
    expect(share.passed).toBe(0);
    expect(share.failed).toBe(1);
  });

  it("keeps a gate that could not run apart from a gate that said no", () => {
    const share = gateShare(gate({ passed: 2, failed: 1, errored: 1 }));

    expect(share.judged).toBe(4);
    expect(share.failed).toBe(0.25);
    expect(share.errored).toBe(0.25);
    // Same size, different fact. Adding them would say three runs are broken when one is.
    expect(share.failed).not.toBe(share.failed + share.errored);
  });

  it("does not divide by nothing when nothing was judged", () => {
    const share = gateShare(gate({ no_gate: 7 }));

    expect(share.judged).toBe(0);
    // Zero, not NaN: a card that printed "NaN%" would be a defect wearing the clothes of a reading.
    expect(share.passed).toBe(0);
  });
});

describe("efficiencyTrend", () => {
  it("calls fewer tokens an improvement, because fewer tokens is the goal", () => {
    expect(
      efficiencyTrend(
        efficiency({ median_total_tokens: 60_000, previous_median_total_tokens: 100_000 }),
      ),
    ).toBe("improved");
  });

  it("calls more tokens a worsening", () => {
    expect(
      efficiencyTrend(
        efficiency({ median_total_tokens: 140_000, previous_median_total_tokens: 100_000 }),
      ),
    ).toBe("worsened");
  });

  /**
   * A median over a few dozen runs wobbles. A page that announced a direction for every wobble
   * would be announcing something every single day, and a claim made daily stops being read.
   */
  it("treats a small move as level rather than as news", () => {
    expect(
      efficiencyTrend(
        efficiency({ median_total_tokens: 104_000, previous_median_total_tokens: 100_000 }),
      ),
    ).toBe("level");
  });

  it("claims no direction when there is nothing to compare against", () => {
    expect(efficiencyTrend(efficiency({ median_total_tokens: 100_000 }))).toBe("unknown");
    expect(efficiencyTrend(efficiency({ previous_median_total_tokens: 100_000 }))).toBe("unknown");
    // A previous window of zero would divide by nothing, and "infinitely worse" is not a reading.
    expect(
      efficiencyTrend(
        efficiency({ median_total_tokens: 100_000, previous_median_total_tokens: 0 }),
      ),
    ).toBe("unknown");
  });
});

describe("compactTokens", () => {
  it("shortens without lying about the order of magnitude", () => {
    expect(compactTokens(840)).toBe("840");
    expect(compactTokens(84_210)).toBe("84k");
    expect(compactTokens(2_400_000)).toBe("2.4M");
  });
});

describe("humanMinutes", () => {
  it("says a duration the way somebody would say it out loud", () => {
    expect(humanMinutes(42)).toBe("42 min");
    expect(humanMinutes(150)).toBe("2.5 h");
    expect(humanMinutes(2_880)).toBe("2.0 d");
  });
});
