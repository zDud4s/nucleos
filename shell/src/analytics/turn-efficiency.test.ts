import { describe, expect, it } from "vitest";
import type { TurnClassRow } from "../data/devtime";
import { devtimeTurnClasses, devtimeUnexplained } from "../test/harness";
import { classLabel, criteriaText, explainedText, hasTurnClasses, perCallText, slowTurns } from "./turn-efficiency";

const row = (overrides: Partial<TurnClassRow>): TurnClassRow => ({
  ...devtimeTurnClasses().classes[1],
  ...overrides,
});

describe("classLabel", () => {
  it("names a class by its call range", () => {
    expect(classLabel(row({ min_calls: 0, max_calls: 0 }))).toBe("No tool calls");
    expect(classLabel(row({ min_calls: 1, max_calls: 5 }))).toBe("1–5 calls");
    expect(classLabel(row({ min_calls: 1, max_calls: 1 }))).toBe("1 call");
    expect(classLabel(row({ min_calls: 21, max_calls: null }))).toBe("21+ calls");
  });

  it("falls back to the raw token when the range is unknown", () => {
    expect(classLabel(row({ turn_class: "c9", min_calls: null, max_calls: null }))).toBe("c9");
  });
});

describe("perCallText", () => {
  it("reads an absent cost as unknown and a short one in seconds", () => {
    expect(perCallText(null)).toBe("—");
    expect(perCallText(56_250)).toBe("56 s");
  });

  it("switches to minutes from a minute up", () => {
    expect(perCallText(59_400)).toBe("59 s");
    expect(perCallText(59_600)).toBe("1 min");
    expect(perCallText(150_000)).toBe("3 min");
  });
});

describe("explainedText", () => {
  it("writes the explained ratio as a percentage, unknown when null", () => {
    expect(explainedText(row({ explained_ratio: 0.1111 }))).toBe("11%");
    expect(explainedText(row({ explained_ratio: null }))).toBe("—");
  });
});

describe("criteriaText", () => {
  it("states the picker's thresholds and the medians basis", () => {
    expect(criteriaText(devtimeUnexplained().criteria)).toBe(
      "At least 3× the median of turns of its size, 5 min or longer, at most 20% explained by a rule · medians since 2026-09-17",
    );
  });
});

describe("slowTurns", () => {
  it("keeps the daemon's order and caps the list", () => {
    const items = devtimeUnexplained().items;
    expect(slowTurns(items)).toEqual(items);
    expect(slowTurns(items, 2)).toEqual(items.slice(0, 2));
    const many = Array.from({ length: 14 }, (_, index) => ({ ...items[0], active_ms: 2_000_000 - index }));
    expect(slowTurns(many)).toHaveLength(10);
    expect(slowTurns(many)[9]).toBe(many[9]);
  });
});

describe("hasTurnClasses", () => {
  it("is false for an older daemon and for an empty window, true otherwise", () => {
    expect(hasTurnClasses(undefined)).toBe(false);
    expect(hasTurnClasses({ turns: 0, classes: [] })).toBe(false);
    expect(hasTurnClasses(devtimeTurnClasses())).toBe(true);
  });
});
