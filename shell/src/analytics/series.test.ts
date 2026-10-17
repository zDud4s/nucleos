// @vitest-environment node
import { describe, expect, it } from "vitest";
import type { CostBlock, QuotaHistory, ThroughputBlock } from "../data/analytics";
import {
  ESTIMATED_KEY,
  GATE_KEYS,
  costBars,
  costKeys,
  gateBars,
  passRate,
  quotaSeries,
  totalTokens,
  versus,
} from "./series";

const zeroCost = { measured_usd: 0, estimated_usd: 0, judge_usd: 0, runs: 0, estimated_runs: 0 };

function cost(overrides: Partial<CostBlock> = {}): CostBlock {
  return {
    total: zeroCost,
    previous: zeroCost,
    series: [],
    by_label: "project",
    // As the daemon writes them (core plan, "JSON shapes the shell consumes"): labels come from
    // `keys[].label`, reserved keys included, and `judge` is always last.
    keys: [
      { key: "nucleos", label: "nucleos" },
      { key: "__none__", label: "No project" },
      { key: "__other__", label: "Other projects" },
      { key: "judge", label: "Judge" },
    ],
    ...overrides,
  };
}

describe("costKeys", () => {
  it("takes every label from the daemon, and gives the judge and the folded projects tones of their own", () => {
    expect(costKeys(cost())).toEqual([
      { key: "nucleos", label: "nucleos" },
      { key: "__none__", label: "No project" },
      { key: "__other__", label: "Other projects", className: "ui-chart-other" },
      { key: "judge", label: "Judge", className: "ui-chart-judge" },
    ]);
  });

  it("adds the estimated part only when something was estimated, and last", () => {
    const keys = costKeys(cost({ total: { ...zeroCost, estimated_usd: 1.2, estimated_runs: 3 } }));
    expect(keys[keys.length - 1]).toEqual({
      key: ESTIMATED_KEY,
      label: "Estimated from run time",
      className: "ui-chart-estimated",
    });
  });
});

describe("costBars", () => {
  it("stacks measured cost per key and keeps the estimate as a part of its own", () => {
    // One part per `keys[]` entry, in order, zeros included — the daemon's contract.
    const bars = costBars(
      cost({
        series: [
          {
            day: "2026-10-17",
            partial: true,
            by: [
              { key: "nucleos", measured_usd: 1.1, estimated_usd: 0.4 },
              { key: "__none__", measured_usd: 0, estimated_usd: 0.1 },
              { key: "__other__", measured_usd: 0, estimated_usd: 0 },
              { key: "judge", measured_usd: 0.35, estimated_usd: 0 },
            ],
          },
        ],
      }),
    );
    expect(bars).toEqual([
      {
        day: "2026-10-17",
        partial: true,
        parts: [
          { key: "nucleos", value: 1.1 },
          { key: "__none__", value: 0 },
          { key: "__other__", value: 0 },
          { key: "judge", value: 0.35 },
          { key: ESTIMATED_KEY, value: 0.5 },
        ],
      },
    ]);
  });
});

describe("gate", () => {
  const throughput: ThroughputBlock = {
    total: { runs_completed: 52, passed: 40, failed: 5, errored: 2, no_gate: 4, other: 1 },
    previous: { runs_completed: 0, passed: 0, failed: 0, errored: 0, no_gate: 0, other: 0 },
    series: [{ day: "2026-10-17", partial: false, passed: 1, failed: 2, errored: 3, no_gate: 4, other: 5 }],
    by_mode: [],
    in_flight: 0,
  };

  it("keeps the five states five, each in its own tone", () => {
    expect(GATE_KEYS.map((key) => key.key)).toEqual(["passed", "failed", "errored", "no_gate", "other"]);
    expect(new Set(GATE_KEYS.map((key) => key.className)).size).toBe(5);
    expect(gateBars(throughput)[0].parts.map((part) => part.value)).toEqual([1, 2, 3, 4, 5]);
  });

  it("leaves ungated runs out of the pass rate, and has no rate when nothing was judged", () => {
    expect(passRate(throughput.total)).toBeCloseTo(40 / 47);
    expect(passRate({ runs_completed: 4, passed: 0, failed: 0, errored: 0, no_gate: 4, other: 0 })).toBeNull();
  });
});

describe("versus", () => {
  it("says the direction in words, with a tenth either way counted as level", () => {
    expect(versus(12, 6, 30)).toBe("more than the previous 30 days");
    expect(versus(5, 10, 7)).toBe("less than the previous 7 days");
    expect(versus(10.5, 10, 30)).toBe("level with the previous 30 days");
    expect(versus(0, 0, 90)).toBe("level with the previous 90 days");
    expect(versus(3, 0, 30)).toBe("more than the previous 30 days");
  });

  it("says nothing when either side was not measured", () => {
    expect(versus(null, 3, 30)).toBe("");
    expect(versus(3, null, 30)).toBe("");
  });
});

describe("totalTokens", () => {
  it("adds the four kinds", () => {
    expect(
      totalTokens({
        input: 1,
        cache_read: 2,
        cache_creation: 3,
        output: 4,
        measured_sessions: 1,
        unmeasured_runs: 0,
        cache_hit_ratio: null,
      }),
    ).toBe(10);
  });
});

describe("quotaSeries", () => {
  it("draws one series per provider and window, as fractions over epoch time", () => {
    const history: QuotaHistory = {
      days: 30,
      timezone: "UTC",
      since: "2026-10-12T10:00:00Z",
      series: [
        {
          provider: "anthropic",
          window: "5h",
          fidelity: "derived",
          points: [{ at: "2026-10-12T10:00:00Z", used_fraction: 0.41, resets_at: null, stale: true }],
        },
      ],
    };
    expect(quotaSeries(history)).toEqual([
      {
        key: "anthropic:5h",
        label: "anthropic 5h (derived)",
        points: [{ at: Date.parse("2026-10-12T10:00:00Z"), value: 0.41, stale: true }],
      },
    ]);
  });
});
