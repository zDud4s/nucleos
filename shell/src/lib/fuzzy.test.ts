import { describe, expect, it } from "vitest";
import { fuzzyFilter, fuzzyScore } from "./fuzzy";

describe("fuzzy", () => {
  it("matches subsequences and rejects non-matches", () => {
    expect(fuzzyScore("Autopilot", "atp")).not.toBeNull();
    expect(fuzzyScore("Autopilot", "xyz")).toBeNull();
    expect(fuzzyScore("Anything", "")).toBe(0);
  });
  it("ranks substring/prefix above scattered", () => {
    const out = fuzzyFilter(["Fleet", "Analytics", "Files"], "fi", (s) => s);
    expect(out[0]).toBe("Files");
  });
  it("keeps the same array for an empty query and original order on ties", () => {
    const a = ["b", "a"];
    expect(fuzzyFilter(a, " ", (s) => s)).toBe(a);
    expect(fuzzyFilter(["ab1", "ab2"], "ab", (s) => s)).toEqual(["ab1", "ab2"]);
  });
});
