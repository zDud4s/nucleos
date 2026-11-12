// @vitest-environment node
import { describe, expect, it } from "vitest";
import {
  BAND_GAP, BAND_HEAD, CARD_H, GAP_X, GAP_Y, GROUP_H, NODE_H, NODE_W, TREE_GAP,
  layoutForest, pathToRoot, type LayoutOptions,
} from "./layout";
import { at, child, node } from "./test-fixtures";

const base: LayoutOptions = { size: "terminal", onlyActive: false, project: null, expanded: new Set() };
const pos = (out: ReturnType<typeof layoutForest>, id: string) => out.nodes.find((n) => n.id === id)!;

describe("layoutForest", () => {
  it("puts a lone root at the band's origin, below the band head", () => {
    const out = layoutForest([node("r")], base);
    expect(pos(out, "r")).toMatchObject({ x: 0, y: BAND_HEAD, w: NODE_W, h: NODE_H });
  });

  it("centers a parent over its children; a subtree is as wide as its children plus gaps", () => {
    const r = node("r");
    const out = layoutForest([r, child("a", r, { started_at: at(1) }), child("b", r, { started_at: at(2) })], base);
    const row = BAND_HEAD + NODE_H + GAP_Y;
    expect(pos(out, "a")).toMatchObject({ x: 0, y: row });
    expect(pos(out, "b")).toMatchObject({ x: NODE_W + GAP_X, y: row });
    expect(pos(out, "r").x).toBe((NODE_W + GAP_X) / 2);
  });

  it("widens a subtree by the sum of its children, not by its depth", () => {
    const r = node("r");
    const a = child("a", r, { started_at: at(1) });
    const b = child("b", r, { started_at: at(2) });
    const a1 = child("a1", a, { started_at: at(3) });
    const a2 = child("a2", a, { started_at: at(4) });
    const out = layoutForest([r, a, b, a1, a2], base);
    const aWidth = 2 * NODE_W + GAP_X;
    expect(pos(out, "a1").x).toBe(0);
    expect(pos(out, "a2").x).toBe(NODE_W + GAP_X);
    expect(pos(out, "a").x).toBe((aWidth - NODE_W) / 2);
    expect(pos(out, "b").x).toBe(aWidth + GAP_X);
  });

  it("lays trees of one project side by side, oldest first", () => {
    const out = layoutForest([node("late", { started_at: at(9) }), node("early", { started_at: at(1) })], base);
    expect(pos(out, "early").x).toBe(0);
    expect(pos(out, "late").x).toBe(NODE_W + TREE_GAP);
    expect(pos(out, "late").y).toBe(pos(out, "early").y);
  });

  it("stacks project bands vertically, alphabetically, each containing its nodes", () => {
    const out = layoutForest([node("z1", { project: "zeta" }), node("a1", { project: "alpha" })], base);
    expect(out.bands.map((b) => b.project)).toEqual(["alpha", "zeta"]);
    const [first, second] = out.bands;
    expect(second.y).toBe(first.y + first.h + BAND_GAP);
    for (const [band, id] of [[first, "a1"], [second, "z1"]] as const) {
      const n = pos(out, id);
      expect(n.x).toBeGreaterThanOrEqual(band.x);
      expect(n.x + n.w).toBeLessThanOrEqual(band.x + band.w);
      expect(n.y).toBeGreaterThanOrEqual(band.y);
      expect(n.y + n.h).toBeLessThanOrEqual(band.y + band.h);
    }
  });

  it("puts a root without a project in its own last band", () => {
    const out = layoutForest([node("n", { project: null }), node("a", { project: "alpha" })], base);
    expect(out.bands.map((b) => b.project)).toEqual(["alpha", "no project"]);
  });

  it("filters to one project", () => {
    const out = layoutForest([node("z1", { project: "zeta" }), node("a1", { project: "alpha" })], { ...base, project: "alpha" });
    expect(out.nodes.map((n) => n.id)).toEqual(["a1"]);
  });

  describe("done group (F9)", () => {
    const doneKids = (r: ReturnType<typeof node>, n: number) =>
      Array.from({ length: n }, (_, i) => child(`d${i}`, r, { state: "done", started_at: at(i + 1) }));

    it("keeps six done children as they are", () => {
      const r = node("r");
      expect(layoutForest([r, ...doneKids(r, 6)], base).nodes).toHaveLength(7);
    });

    it("folds seven done children into one group node counting them all", () => {
      const r = node("r");
      const out = layoutForest([r, ...doneKids(r, 7)], base);
      expect(out.nodes.map((n) => n.id)).toEqual(["r", "group:r"]);
      expect(pos(out, "group:r")).toMatchObject({ kind: "group", w: NODE_W, h: GROUP_H });
      expect(pos(out, "group:r").group).toMatchObject({ parentId: "r", count: 7, present: 7, omitted: 0, open: false });
      expect(out.edges.map((e) => e.id)).toEqual(["e:r>group:r"]);
    });

    it("counts done_omitted: three present and four omitted is a group of seven", () => {
      const r = node("r", { done_omitted: 4 });
      const out = layoutForest([r, ...doneKids(r, 3)], base);
      expect(pos(out, "group:r").group).toMatchObject({ count: 7, present: 3, omitted: 4 });
    });

    it("an open group lists only the present done nodes after itself", () => {
      const r = node("r", { done_omitted: 2 });
      const out = layoutForest([r, ...doneKids(r, 6)], { ...base, expanded: new Set(["r"]) });
      expect(out.nodes).toHaveLength(1 + 1 + 6);
      expect(pos(out, "group:r").group).toMatchObject({ open: true, omitted: 2 });
    });

    it("never folds a done node that still has live descendants", () => {
      const r = node("r");
      const kids = doneKids(r, 8);
      const live = child("live", kids[0], { state: "busy", started_at: at(20) });
      const out = layoutForest([r, ...kids, live], base);
      expect(out.nodes.map((n) => n.id).sort()).toEqual(["d0", "group:r", "live", "r"].sort());
      expect(pos(out, "group:r").group?.count).toBe(7);
    });
  });

  it("Active only drops finished nodes and trees with nothing alive", () => {
    const r = node("r");
    const out = layoutForest(
      [r, child("busy", r), child("old", r, { state: "done" }), node("dead", { state: "done" })],
      { ...base, onlyActive: true },
    );
    expect(out.nodes.map((n) => n.id).sort()).toEqual(["busy", "r"]);
  });

  it("card size shortens the rows", () => {
    const r = node("r");
    const out = layoutForest([r, child("a", r)], { ...base, size: "card" });
    expect(pos(out, "a").y).toBe(BAND_HEAD + CARD_H + GAP_Y);
    expect(pos(out, "a").h).toBe(CARD_H);
  });

  it("is deterministic: the same nodes in any order give the same layout", () => {
    const r = node("r");
    const nodes = [r, child("a", r, { started_at: at(1) }), child("b", r, { started_at: at(1) }), node("s", { project: "x" })];
    expect(JSON.stringify(layoutForest([...nodes].reverse(), base))).toBe(JSON.stringify(layoutForest(nodes, base)));
  });

  it("draws an edge from every placed parent to its child", () => {
    const r = node("r");
    const out = layoutForest([r, child("a", r)], base);
    expect(out.edges).toEqual([{ id: "e:r>a", source: "r", target: "a" }]);
  });

  it("treats a node whose parent is unknown as a root", () => {
    const out = layoutForest([node("orphan", { parent_id: "ghost" })], base);
    expect(out.nodes.map((n) => n.id)).toEqual(["orphan"]);
  });
});

describe("pathToRoot", () => {
  it("collects the node and its ancestors, through a group node too", () => {
    const parents: Record<string, string | null> = { c: "b", b: "a", a: null };
    const parentOf = (id: string) => parents[id] ?? null;
    expect([...pathToRoot(parentOf, "c")].sort()).toEqual(["a", "b", "c"]);
    expect([...pathToRoot(parentOf, "group:b")].sort()).toEqual(["a", "b", "group:b"]);
  });
  it("survives a cycle", () => {
    const parentOf = (id: string) => (id === "a" ? "b" : "a");
    expect(pathToRoot(parentOf, "a").size).toBe(2);
  });
});
