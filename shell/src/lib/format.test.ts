// @vitest-environment node
import { describe, expect, it } from "vitest";
import { percentText } from "./format";

describe("percentText", () => {
  it("rounds a fraction to a whole percentage", () => {
    expect(percentText(0.424)).toBe("42%");
    expect(percentText(0.425)).toBe("43%");
    expect(percentText(0)).toBe("0%");
    expect(percentText(1)).toBe("100%");
    expect(percentText(1.3)).toBe("130%");
  });

  it("says the fallback for what is not a number, and never zero", () => {
    expect(percentText(undefined)).toBe("—");
    expect(percentText(null)).toBe("—");
    expect(percentText(Number.NaN)).toBe("—");
    expect(percentText(Number.POSITIVE_INFINITY, "n/a")).toBe("n/a");
  });
});
