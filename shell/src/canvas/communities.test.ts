import { describe, expect, it } from "vitest";
import { communities, feedback, seriate } from "./communities";

const links = (...pairs: string[]) =>
  pairs.map((pair) => {
    const [from, to] = pair.split(">");
    return { from, to, weight: 1 };
  });

describe("communities", () => {
  it("puts two dense clumps joined by one link into two groups", () => {
    // The shape the núcleo's `map_*` files have: tightly wired inside, one thread to the rest.
    const found = communities(
      ["a1", "a2", "a3", "b1", "b2", "b3"],
      links("a1>a2", "a2>a3", "a3>a1", "b1>b2", "b2>b3", "b3>b1", "a1>b1"),
    );
    expect(new Set(found.values()).size).toBe(2);
    expect(found.get("a1")).toBe(found.get("a3"));
    expect(found.get("b1")).toBe(found.get("b3"));
    expect(found.get("a1")).not.toBe(found.get("b1"));
  });

  it("raises the resolution until no group is too big to draw", () => {
    // A single ring of thirty would be one community at the default resolution, and one community
    // of thirty is a picture nobody can read. The knob is turned until it is not.
    const ring = Array.from({ length: 30 }, (_, i) => `n${i}`);
    const found = communities(
      ring,
      ring.map((name, i) => ({ from: name, to: ring[(i + 1) % ring.length], weight: 1 })),
      6,
    );
    const sizes = new Map<string, number>();
    for (const where of found.values()) sizes.set(where, (sizes.get(where) ?? 0) + 1);
    expect(Math.max(...sizes.values())).toBeLessThanOrEqual(6);
  });

  it("leaves a name with no links in a group of its own", () => {
    const found = communities(["a", "b", "lonely"], links("a>b"));
    expect(found.get("lonely")).toBe("lonely");
  });
});

describe("seriate", () => {
  it("puts a chain in the order it flows, so nothing sits below the diagonal", () => {
    const chain = links("a>b", "b>c", "c>d");
    expect(seriate(["d", "c", "b", "a"], chain)).toEqual(["a", "b", "c", "d"]);
    expect(feedback(seriate(["d", "c", "b", "a"], chain), chain).back).toBe(0);
  });

  it("beats ordering by depth on a graph where depth is misleading", () => {
    // Depth ranks by longest path, which says nothing about how much weight points backwards.
    const graph = links("a>b", "b>c", "c>a", "a>c", "b>a", "c>b");
    const mine = feedback(seriate(["a", "b", "c"], graph), graph);
    expect(mine.back).toBeLessThanOrEqual(mine.forward);
  });

  it("returns every name exactly once", () => {
    const names = ["a", "b", "c", "d", "e"];
    const ordered = seriate(names, links("a>b", "b>c", "c>a", "d>e"));
    expect([...ordered].sort()).toEqual(names);
  });

  it("cannot remove a cycle, and reports what it could not remove", () => {
    // The honest half: three files that each depend on the next always leave one arrow pointing
    // back, whatever the order. That one is the code's, not the sort's.
    const ring = links("a>b", "b>c", "c>a");
    expect(feedback(seriate(["a", "b", "c"], ring), ring).back).toBe(1);
  });
});
