// @vitest-environment node
import { describe, expect, it } from "vitest";
import type { GEdge, GModel, GNode } from "./graph-types";
import { colorToken, neighbours, nodeRadius } from "./graph-util";

function node(id: string, extra: Partial<GNode> = {}): GNode {
  return { id, kind: "note", ref: id, label: id, bucket: "in_force", missing: false, degree: 0, ...extra };
}
function edge(source: string, target: string): GEdge {
  return { id: `relates|${source}|${target}`, source, target, type: "relates" };
}

describe("neighbours", () => {
  // a - b - c - d, plus e alone
  const model: GModel = {
    nodes: ["a", "b", "c", "d", "e"].map((id) => node(id)),
    edges: [edge("a", "b"), edge("b", "c"), edge("c", "d")],
  };

  it("depth 1 is the node and its direct neighbours, both directions", () => {
    expect([...neighbours(model, "b", 1)].sort()).toEqual(["a", "b", "c"]);
  });
  it("depth 2 reaches one hop further", () => {
    expect([...neighbours(model, "a", 2)].sort()).toEqual(["a", "b", "c"]);
    expect([...neighbours(model, "b", 2)].sort()).toEqual(["a", "b", "c", "d"]);
  });
  it("an isolated node is only itself", () => {
    expect([...neighbours(model, "e", 2)]).toEqual(["e"]);
  });
});

describe("nodeRadius", () => {
  it("starts at 4 and grows with degree", () => {
    expect(nodeRadius(0)).toBe(4);
    expect(nodeRadius(4)).toBe(8);
    expect(nodeRadius(9)).toBeGreaterThan(nodeRadius(4));
  });
  it("is capped at 16", () => {
    expect(nodeRadius(10_000)).toBe(16);
  });
  it("treats a negative degree as zero", () => {
    expect(nodeRadius(-3)).toBe(4);
  });
});

describe("colorToken", () => {
  it("names a knowledge row by its layer", () => {
    expect(colorToken(node("k:1", { kind: "knowledge", layer: "procedural" }))).toBe("--brain-k-procedural");
    expect(colorToken(node("k:2", { kind: "knowledge", layer: "episodic" }))).toBe("--brain-k-episodic");
  });
  it("falls back to semantic for a knowledge row with no layer", () => {
    expect(colorToken(node("k:3", { kind: "knowledge" }))).toBe("--brain-k-semantic");
  });
  it.each(["note", "project", "contact", "mail", "file"] as const)("names a %s by its kind", (kind) => {
    expect(colorToken(node("x", { kind }))).toBe(`--brain-${kind}`);
  });
});
