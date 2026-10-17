// @vitest-environment node
import { describe, expect, it } from "vitest";

import { seriesClass, type ChartKey } from "./chart-series";

const KEYS: ChartKey[] = [
  { key: "alpha", label: "alpha" },
  { key: "beta", label: "beta" },
  { key: "judge", label: "Judge", className: "ui-chart-judge" },
];

describe("seriesClass", () => {
  it("uses a key's own class, else cycles the eight categorical ones", () => {
    expect(seriesClass(KEYS, "judge")).toBe("ui-chart-judge");
    expect(seriesClass(KEYS, "beta")).toBe("ui-chart-s2");
    const many = Array.from({ length: 10 }, (_, i) => ({ key: `k${i}`, label: `k${i}` }));
    expect(seriesClass(many, "k8")).toBe("ui-chart-s1");
    // A key the legend does not know is "other", never series 1's colour.
    expect(seriesClass(KEYS, "stranger")).toBe("ui-chart-other");
  });
});
