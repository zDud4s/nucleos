import { describe, expect, it } from "vitest";
import { devtimeOverview } from "../test/harness";
import {
  COST_KEYS, KIND_KEYS, WASTE_KEYS, activeByKind, activeByWaste, costSeries, leverShares, precisionText,
  qualityLine, ruleShares, versusPoints,
} from "./devtime-series";

const MIN = 60_000;

describe("devtime adapters", () => {
  it("stacks active time by waste or by kind, keeping today partial", () => {
    const report = devtimeOverview({ days: 7 });
    report.days_series[6] = { ...report.days_series[6], active_ms: 10 * MIN,
      by_waste: { useful: 6 * MIN, rework: 3 * MIN, avoidable: MIN },
      by_kind: { model: 4 * MIN, tool: 6 * MIN, subagent: 0, wait_background: 0, wait_machine: 0 } };
    const byWaste = activeByWaste(report.days_series);
    expect(byWaste).toHaveLength(7);
    expect(byWaste[6]).toEqual({ day: report.days_series[6].day, partial: true,
      parts: [{ key: "useful", value: 6 * MIN }, { key: "rework", value: 3 * MIN }, { key: "avoidable", value: MIN }] });
    expect(activeByKind(report.days_series)[6].parts.find((p) => p.key === "tool")?.value).toBe(6 * MIN);
    expect(WASTE_KEYS.map((k) => k.className)).toEqual(["ui-chart-useful", "ui-chart-rework", "ui-chart-avoidable"]);
    expect(KIND_KEYS).toHaveLength(5);
  });

  it("draws the daily cost, leaves an unpriced day empty rather than zero, and draws nothing when no day is priced", () => {
    const report = devtimeOverview({ days: 7 });
    expect(costSeries(report.days_series)).toEqual([]); // every day null: no sparkline at all
    report.days_series[4] = { ...report.days_series[4], cost_usd: 1.5 };
    report.days_series[6] = { ...report.days_series[6], cost_usd: 0.25 };
    const cost = costSeries(report.days_series);
    expect(cost).toHaveLength(7);
    expect(cost[4]).toEqual({ day: report.days_series[4].day, partial: false, parts: [{ key: "cost", value: 1.5 }] });
    expect(cost[5].parts).toEqual([]); // null day: no part (the chart reads it as baseline)
    expect(cost[6]).toMatchObject({ partial: true, parts: [{ key: "cost", value: 0.25 }] });
    expect(COST_KEYS).toEqual([{ key: "cost", label: "Estimated cost", className: "ui-chart-cost" }]);
  });

  it("keeps the top eight rules and levers, folds the rest into Other, and names the reserved keys", () => {
    const rows = Array.from({ length: 10 }, (_, i) => ({ rule_id: `r${i}`, level: "base", waste: "rework", lever: "x", ms: 100 - i, findings: 1 }));
    rows[9] = { ...rows[9], rule_id: "__none__" };
    const shares = ruleShares(rows);
    expect(shares).toHaveLength(9);
    expect(shares[8]).toMatchObject({ key: "__other__", label: "Other", value: 92 + 91 });
    expect(leverShares([{ lever: "__none__", ms: 5 }])[0].label).toBe("No lever");
  });

  it("speaks a ratio's change in points, and says nothing when either side is absent", () => {
    expect(versusPoints(0.12, 0.16, 30)).toBe("4 points less than the previous 30 days");
    expect(versusPoints(0.2, 0.15, 7)).toBe("5 points more than the previous 7 days");
    expect(versusPoints(0.15, 0.152, 7)).toBe("level with the previous 7 days");
    expect(versusPoints(null, 0.1, 7)).toBe("");
  });

  it("says the precision with its n, or few cases, and that it is global in a project's view", () => {
    const rule = { rule_id: "A1", level: "base", n_cases: 12, n_not_rework: 2, precision: 0.83, prior: 0.7, eligible: true, low_precision: false };
    expect(precisionText(rule, false)).toBe("83% in 12 cases");
    expect(precisionText({ ...rule, precision: null, n_cases: 3 }, true)).toBe("few cases (3) · global, all projects");
  });

  it("writes the quality strip in last-cycle words, amber when the last read is old or ingest is off", () => {
    const now = Date.parse("2026-10-17T12:00:00Z");
    const fresh = qualityLine({ ingest_enabled: true, last_cycle_at: "2026-10-17T11:58:00Z", unmapped_sessions_last_cycle: 3,
      skipped_daemon_sessions_last_cycle: 12, sessions: 148, dirty_sessions: 4, unruled_sessions: 0 }, now);
    expect(fresh.amber).toBe(false);
    expect(fresh.clauses).toEqual([
      "Last read 2 min ago",
      "3 sessions without a project in the last cycle",
      "12 daemon sessions skipped in the last cycle",
      "4 dirty",
    ]);
    expect(qualityLine({ ...fresh.quality, last_cycle_at: "2026-10-17T11:50:00Z" }, now).amber).toBe(true);
    expect(qualityLine({ ...fresh.quality, ingest_enabled: false }, now).amber).toBe(true);
  });
});
