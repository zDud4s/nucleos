// @vitest-environment node
import { describe, expect, it } from "vitest";
import { hitTest, labelVisible, toWorld, type Placed, type Transform } from "./hit-test";

const IDENTITY: Transform = { k: 1, x: 0, y: 0 };

describe("toWorld", () => {
  it("undoes the translate, then the scale", () => {
    expect(toWorld({ x: 110, y: 70 }, { k: 2, x: 10, y: 30 })).toEqual({ x: 50, y: 20 });
  });

  it("is the identity under the identity transform", () => {
    expect(toWorld({ x: 3, y: -4 }, IDENTITY)).toEqual({ x: 3, y: -4 });
  });
});

describe("hitTest", () => {
  const one: Placed[] = [{ id: "a", x: 100, y: 100, r: 10 }];

  it("hits a point inside the circle", () => {
    expect(hitTest(one, { x: 105, y: 100 }, IDENTITY)).toBe("a");
  });

  it("misses a point outside the radius plus slop", () => {
    expect(hitTest(one, { x: 115, y: 100 }, IDENTITY)).toBeNull();
  });

  it("hits within the slop just past the edge", () => {
    expect(hitTest(one, { x: 113, y: 100 }, IDENTITY)).toBe("a");
  });

  it("shrinks the slop in world units as the zoom grows", () => {
    // At k=4 the 4px slop is 1 world unit: 12 world units out (r=10) misses, 10.5 hits.
    const zoomed: Transform = { k: 4, x: 0, y: 0 };
    expect(hitTest(one, { x: 112 * 4, y: 100 * 4 }, zoomed)).toBeNull();
    expect(hitTest(one, { x: 110.5 * 4, y: 100 * 4 }, zoomed)).toBe("a");
  });

  it("follows the transform's translate", () => {
    const panned: Transform = { k: 1, x: 300, y: -50 };
    expect(hitTest(one, { x: 400, y: 50 }, panned)).toBe("a");
    expect(hitTest(one, { x: 100, y: 100 }, panned)).toBeNull();
  });

  it("takes the nearest of two overlapping nodes", () => {
    const two: Placed[] = [
      { id: "a", x: 100, y: 100, r: 10 },
      { id: "b", x: 110, y: 100, r: 10 },
    ];
    expect(hitTest(two, { x: 102, y: 100 }, IDENTITY)).toBe("a");
    expect(hitTest(two, { x: 108, y: 100 }, IDENTITY)).toBe("b");
  });

  it("breaks a tie for the last in the array, the one drawn on top", () => {
    const stacked: Placed[] = [
      { id: "under", x: 50, y: 50, r: 8 },
      { id: "over", x: 50, y: 50, r: 8 },
    ];
    expect(hitTest(stacked, { x: 50, y: 50 }, IDENTITY)).toBe("over");
  });

  it("answers null for an empty graph", () => {
    expect(hitTest([], { x: 0, y: 0 }, IDENTITY)).toBeNull();
  });
});

describe("labelVisible", () => {
  it.each([
    // k, hovered, focused, compact, visible
    [0.5, false, false, false, false],
    [1.39, false, false, false, false],
    [1.4, false, false, false, true],
    [3, false, false, false, true],
    [0.5, true, false, false, true],
    [0.5, false, true, false, true],
    [0.5, false, false, true, true],
  ])("k=%s hovered=%s focused=%s compact=%s -> %s", (k, hovered, focused, compact, visible) => {
    expect(labelVisible(k, hovered, focused, compact)).toBe(visible);
  });
});
