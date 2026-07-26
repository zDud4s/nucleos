import { describe, expect, it } from "vitest";

import type { Budget, ClassTally, ProjectSummary } from "./api";
import {
  agreementRate,
  autopilotState,
  budgetStatusLabel,
  formatUsd,
  groupScoreboardByMode,
  killSwitchLabel,
  periodLabel,
  promotionReadiness,
  readinessCriterionLabel,
  readinessGap,
  relativeTime,
  scoreboardReadiness,
  totalPending,
} from "./derive";

describe("pure UI derivations", () => {
  it("sums pending work across projects, including an empty list", () => {
    const cases = [
      { projects: [] satisfies ProjectSummary[], expected: 0 },
      {
        projects: [
          { project_id: "alpha", mode: "off", project_root: null, pending: 2 },
          { project_id: "beta", mode: "shadow", project_root: null, pending: 0 },
          { project_id: "gamma", mode: "active", project_root: null, pending: 5 },
        ] satisfies ProjectSummary[],
        expected: 7,
      },
    ];

    for (const { projects, expected } of cases) {
      expect(totalPending(projects)).toBe(expected);
    }
  });

  it("derives agreement rates, including the zero-reviewed edge case", () => {
    const cases = [
      {
        tally: {
          mode: "shadow",
          action_class: "filesystem.write",
          total: 0,
          would_allow: 0,
          would_pend: 0,
          would_deny: 0,
          reviewed: 0,
          agree: 0,
          disagree: 0,
        } satisfies ClassTally,
        expected: null,
      },
      {
        tally: {
          mode: "active",
          action_class: "shell.execute",
          total: 4,
          would_allow: 3,
          would_pend: 1,
          would_deny: 0,
          reviewed: 4,
          agree: 3,
          disagree: 1,
        } satisfies ClassTally,
        expected: 0.75,
      },
    ];

    for (const { tally, expected } of cases) {
      expect(agreementRate(tally)).toBe(expected);
    }
  });

  it("groups an empty scoreboard into an empty record", () => {
    expect(groupScoreboardByMode([] satisfies ClassTally[])).toEqual({});
  });

  it("groups scoreboard tallies by mode while preserving group order", () => {
    const tallies = [
      {
        mode: "shadow",
        action_class: "filesystem.write",
        total: 3,
        would_allow: 1,
        would_pend: 2,
        would_deny: 0,
        reviewed: 2,
        agree: 2,
        disagree: 0,
      },
      {
        mode: "active",
        action_class: "shell.execute",
        total: 2,
        would_allow: 2,
        would_pend: 0,
        would_deny: 0,
        reviewed: 1,
        agree: 1,
        disagree: 0,
      },
      {
        mode: "shadow",
        action_class: "network.request",
        total: 4,
        would_allow: 2,
        would_pend: 1,
        would_deny: 1,
        reviewed: 3,
        agree: 2,
        disagree: 1,
      },
    ] satisfies ClassTally[];

    expect(groupScoreboardByMode(tallies)).toEqual({
      shadow: [tallies[0], tallies[2]],
      active: [tallies[1]],
    });
  });

  it("derives the kill-switch labels", () => {
    const cases = [
      [true, "Kill switch engaged — autopilot paused"],
      [false, "Kill switch off"],
    ] as const;

    for (const [engaged, expected] of cases) {
      expect(killSwitchLabel(engaged)).toBe(expected);
    }
  });

  it("formats USD amounts to two decimals", () => {
    expect(formatUsd(1.5)).toBe("$1.50");
    expect(formatUsd(0)).toBe("$0.00");
    expect(formatUsd(12.345)).toBe("$12.35");
  });

  it("labels budget periods", () => {
    expect(periodLabel("daily")).toBe("today");
    expect(periodLabel("weekly")).toBe("this week");
    expect(periodLabel("monthly")).toBe("this month");
  });

  it("derives budget status labels for unset, active, and paused budgets", () => {
    const base = {
      limit_usd: null,
      period: "monthly",
      hourly_limit_usd: null,
      per_run_reserve_usd: 0.5,
      time_cost_per_hour_usd: 3,
      window_spend_usd: 0,
      hourly_spend_usd: 0,
      paused: false,
      reason: null,
    } satisfies Budget;

    expect(budgetStatusLabel(base)).toBe("No spending limit set");
    expect(
      budgetStatusLabel({ ...base, limit_usd: 10, window_spend_usd: 1.5 }),
    ).toBe("$1.50 of $10.00 this month");
    expect(
      budgetStatusLabel({ ...base, limit_usd: 10, window_spend_usd: 12, paused: true }),
    ).toBe("Paused — $12.00 of $10.00 this month");
  });

  it("derives promotion readiness per action class", () => {
    const base = {
      mode: "shadow",
      action_class: "filesystem.write",
      total: 0,
      would_allow: 0,
      would_pend: 0,
      would_deny: 0,
      reviewed: 0,
      agree: 0,
      disagree: 0,
    } satisfies ClassTally;

    expect(promotionReadiness(base)).toEqual({ ready: false, rate: null, samples: 0 });
    expect(
      promotionReadiness({ ...base, reviewed: 10, agree: 10 }),
    ).toEqual({ ready: true, rate: 1, samples: 10 });
    expect(
      promotionReadiness({ ...base, reviewed: 10, agree: 9 }),
    ).toEqual({ ready: false, rate: 0.9, samples: 10 });
    expect(
      promotionReadiness({ ...base, reviewed: 5, agree: 5 }),
    ).toEqual({ ready: false, rate: 1, samples: 5 });
  });

  it("explains the gap to readiness, or null when ready", () => {
    const base = {
      mode: "shadow",
      action_class: "filesystem.write",
      total: 0,
      would_allow: 0,
      would_pend: 0,
      would_deny: 0,
      reviewed: 0,
      agree: 0,
      disagree: 0,
    } satisfies ClassTally;

    expect(readinessGap({ ...base, reviewed: 10, agree: 10 })).toBeNull();
    expect(readinessGap(base)).toBe("10 more reviews");
    expect(readinessGap({ ...base, reviewed: 4, agree: 4 })).toBe("6 more reviews");
    expect(readinessGap({ ...base, reviewed: 9, agree: 9 })).toBe("1 more review");
    expect(readinessGap({ ...base, reviewed: 10, agree: 9 })).toBe("90% agreement");
    expect(readinessGap({ ...base, reviewed: 20, agree: 18 })).toBe("90% agreement");
  });

  it("counts ready classes across a scoreboard group", () => {
    const base = {
      mode: "shadow",
      action_class: "a",
      total: 0,
      would_allow: 0,
      would_pend: 0,
      would_deny: 0,
      reviewed: 0,
      agree: 0,
      disagree: 0,
    } satisfies ClassTally;

    expect(scoreboardReadiness([])).toEqual({ ready: 0, total: 0 });
    expect(
      scoreboardReadiness([
        { ...base, action_class: "a", reviewed: 10, agree: 10 },
        { ...base, action_class: "b", reviewed: 3, agree: 3 },
      ]),
    ).toEqual({ ready: 1, total: 2 });
  });

  it("states the promotion criterion", () => {
    expect(readinessCriterionLabel()).toBe("Ready at 10+ reviews, ≥95% agreement");
  });

  it("derives the headline autopilot state in priority order", () => {
    const calm = {
      killEngaged: false,
      budgetPaused: false,
      isFirstProject: false,
      proposalCount: 0,
      pending: 0,
    };

    expect(autopilotState(calm)).toBe("quiet");
    expect(autopilotState({ ...calm, pending: 2 })).toBe("pending");
    expect(autopilotState({ ...calm, proposalCount: 4 })).toBe("swamped");
    // The queue is only "swamped" strictly beyond the threshold.
    expect(autopilotState({ ...calm, proposalCount: 3 })).toBe("quiet");
    expect(autopilotState({ ...calm, isFirstProject: true })).toBe("first");
    expect(autopilotState({ ...calm, budgetPaused: true })).toBe("budget");
    expect(autopilotState({ ...calm, killEngaged: true })).toBe("kill");
    // A null kill switch (not yet loaded) counts as not engaged.
    expect(autopilotState({ ...calm, killEngaged: null, pending: 1 })).toBe("pending");

    // Priority: each higher state wins over every lower one at once.
    expect(
      autopilotState({
        killEngaged: true,
        budgetPaused: true,
        isFirstProject: true,
        proposalCount: 9,
        pending: 9,
      }),
    ).toBe("kill");
    expect(
      autopilotState({
        ...calm,
        budgetPaused: true,
        isFirstProject: true,
        proposalCount: 9,
        pending: 9,
      }),
    ).toBe("budget");
    expect(autopilotState({ ...calm, proposalCount: 4, pending: 9 })).toBe("swamped");
  });

  it("formats human relative times, with fallbacks", () => {
    const now = Date.parse("2026-07-26T12:00:00Z");
    expect(relativeTime("2026-07-26T12:00:00Z", now)).toBe("just now");
    expect(relativeTime("2026-07-26T11:59:30Z", now)).toBe("just now");
    expect(relativeTime("2026-07-26T12:00:30Z", now)).toBe("just now");
    expect(relativeTime("2026-07-26T11:48:00Z", now)).toBe("12 min ago");
    expect(relativeTime("2026-07-26T09:00:00Z", now)).toBe("3 h ago");
    expect(relativeTime("2026-07-24T12:00:00Z", now)).toBe("2 d ago");
    expect(relativeTime("2026-07-12T12:00:00Z", now)).toBe("2 w ago");
    expect(relativeTime("2026-05-01T00:00:00Z", now)).toBe("2026-05-01");
    expect(relativeTime("not-a-date", now)).toBe("not-a-date");
  });
});
