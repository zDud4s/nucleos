// @vitest-environment node
import { describe, expect, it } from "vitest";
import { DEFAULT_DAYS, WINDOWS, analyticsPath, parseDays, quotaPath } from "./analytics";
import { keys } from "./keys";

describe("parseDays", () => {
  it("accepts exactly the three windows the núcleo serves, as numbers or as search strings", () => {
    expect(WINDOWS).toEqual([7, 30, 90]);
    expect(parseDays(7)).toBe(7);
    expect(parseDays("30")).toBe(30);
    expect(parseDays("90")).toBe(90);
  });

  it("refuses anything else rather than clamping it, as the route does (spec §4.1)", () => {
    for (const raw of [14, "14", "abc", "", undefined, null, 30.5, "7d"]) {
      expect(parseDays(raw), String(raw)).toBeUndefined();
    }
    expect(DEFAULT_DAYS).toBe(30);
  });
});

describe("analytics paths and keys", () => {
  it("asks the global route, or the project's, always with the window", () => {
    expect(analyticsPath(undefined, 30)).toBe("/analytics?days=30");
    expect(analyticsPath("a b", 7)).toBe("/projects/a%20b/analytics?days=7");
    expect(quotaPath(90)).toBe("/analytics/quota?days=90");
  });

  it("keys the project's analytics under the project, so removing it evicts them", () => {
    expect(keys.projects.analytics("nucleos", 30)).toEqual(["projects", "nucleos", "analytics", 30]);
    expect(keys.analytics.global(7)).toEqual(["analytics", "global", 7]);
    expect(keys.analytics.quota(7)).toEqual(["analytics", "quota", 7]);
  });
});
