import { describe, expect, it } from "vitest";

import { fitTransform } from "./fit";

const none = { top: 0, right: 0, bottom: 0, left: 0 };

describe("fitTransform", () => {
  it("centres the bounds in the viewport", () => {
    const t = fitTransform({ minX: -10, minY: -10, maxX: 10, maxY: 10 }, 800, 600, none, 0, 0.2, 100);
    expect(t.applyX(0)).toBeCloseTo(400);
    expect(t.applyY(0)).toBeCloseTo(300);
    expect(t.k).toBeCloseTo(30); // limited by the height: 600 / 20
  });

  it("clamps the scale so a tiny graph is not blown up", () => {
    const t = fitTransform({ minX: -1, minY: -1, maxX: 1, maxY: 1 }, 800, 600, none, 0, 0.2, 2);
    expect(t.k).toBe(2);
  });

  it("centres in the space the insets leave free", () => {
    const t = fitTransform({ minX: 0, minY: 0, maxX: 10, maxY: 10 }, 1000, 600, { ...none, right: 400 }, 0, 0.2, 2);
    expect(t.applyX(5)).toBeCloseTo(300);
    expect(t.applyY(5)).toBeCloseTo(300);
  });

  it("follows a new height", () => {
    const b = { minX: -50, minY: -50, maxX: 50, maxY: 50 };
    expect(fitTransform(b, 800, 320, none, 0, 0.2, 2).applyY(0)).toBeCloseTo(160);
    expect(fitTransform(b, 800, 900, none, 0, 0.2, 2).applyY(0)).toBeCloseTo(450);
  });
});
