// @vitest-environment node
import { describe, expect, it } from "vitest";
import { distLine, humanSeconds, percentOr } from "./format";

describe("humanSeconds", () => {
  it("writes the largest sensible unit", () => {
    expect(humanSeconds(12)).toBe("12 s");
    expect(humanSeconds(240)).toBe("4 min");
    expect(humanSeconds(5400)).toBe("1 h 30");
    expect(humanSeconds(7200)).toBe("2 h");
    expect(humanSeconds(190000)).toBe("2.2 d");
  });
});

describe("percentOr", () => {
  it("never turns an unmeasured ratio into a zero", () => {
    expect(percentOr(null)).toBe("no data");
    expect(percentOr(0.934)).toBe("93%");
    expect(percentOr(0)).toBe("0%");
  });
});

describe("distLine", () => {
  it("leaves out a p95 that does not exist, and says no data when nothing was sampled", () => {
    expect(distLine({ p50: 12, p95: null, n: 3 })).toBe("p50 12 s · 3 measured");
    expect(distLine({ p50: 12, p95: 240, n: 9 })).toBe("p50 12 s · p95 4 min · 9 measured");
    expect(distLine({ p50: null, p95: null, n: 0 })).toBe("no data");
  });
});
