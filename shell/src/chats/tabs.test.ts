// @vitest-environment node
import { describe, expect, it } from "vitest";
import { closeTab, openTab, pruneTabs, readTabs } from "./tabs";
import { CHAT_HUES, chatHue } from "./sessions";

describe("tabs", () => {
  it("opens, closes and picks the neighbour, and survives a bad store", () => {
    expect(openTab(["a"], "b")).toEqual(["a", "b"]);
    expect(openTab(["a", "b"], "a")).toEqual(["a", "b"]);

    // Closing the current tab picks the right neighbour, else the left, else nothing.
    expect(closeTab(["a", "b", "c"], "b", "b")).toEqual({ tabs: ["a", "c"], next: "c" });
    expect(closeTab(["a", "b", "c"], "c", "c")).toEqual({ tabs: ["a", "b"], next: "b" });
    expect(closeTab(["a"], "a", "a")).toEqual({ tabs: [], next: null });
    // Closing another tab leaves the current one alone.
    expect(closeTab(["a", "b", "c"], "a", "c")).toEqual({ tabs: ["b", "c"], next: "c" });
    // Closing an id that is not open changes nothing.
    expect(closeTab(["a", "b"], "z", "a")).toEqual({ tabs: ["a", "b"], next: "a" });

    expect(pruneTabs(["a", "b", "c"], ["a", "c", "x"])).toEqual(["a", "c"]);

    expect(readTabs(null)).toEqual([]);
    expect(readTabs("{not json")).toEqual([]);
    expect(readTabs('{"a":1}')).toEqual([]);
    expect(readTabs('["a", 3, "b"]')).toEqual([]);
    expect(readTabs('["a","b","a"]')).toEqual(["a", "b"]);
  });

  it("gives each chat a stable colour from the palette", () => {
    expect(chatHue("c-1")).toBe(chatHue("c-1"));
    expect(CHAT_HUES).toContain(chatHue("c-1"));
    // Not all one colour: a handful of ids spreads over more than one hue.
    const hues = new Set(["c-1", "c-2", "c-3", "c-4", "c-5", "c-6"].map(chatHue));
    expect(hues.size).toBeGreaterThan(2);
  });
});
