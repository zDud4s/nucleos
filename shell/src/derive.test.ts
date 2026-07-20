import { describe, expect, it } from "vitest";

import type { AutopilotMode, ClassTally, ProjectSummary } from "./api";
import {
  agreementRate,
  groupScoreboardByMode,
  killSwitchLabel,
  modeBadge,
  totalPending,
} from "./derive";

describe("pure UI derivations", () => {
  it.each([
    ["off", { label: "Off", tone: "neutral" }],
    ["shadow", { label: "Shadow", tone: "info" }],
    ["active", { label: "Active", tone: "success" }],
  ] satisfies [AutopilotMode, { label: string; tone: string }][]) (
    "derives the %s mode badge",
    (mode, expected) => {
      expect(modeBadge(mode)).toEqual(expected);
    },
  );

  it("sums pending work across projects, including an empty list", () => {
    const cases = [
      { projects: [] satisfies ProjectSummary[], expected: 0 },
      {
        projects: [
          { project_id: "alpha", mode: "off", pending: 2 },
          { project_id: "beta", mode: "shadow", pending: 0 },
          { project_id: "gamma", mode: "active", pending: 5 },
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
});
