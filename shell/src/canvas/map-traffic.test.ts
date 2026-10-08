// @vitest-environment node
// §spec mapa-do-projeto
import { describe, expect, it } from "vitest";
import { buildCommunities, neighbourCount, sliceAround, trafficFor } from "./map-graphs";
import type { MapImport, MapModule } from "../data/project-map";

const mod = (path: string): MapModule => ({
  path,
  reader: "rust",
  declares: false,
  cites: [],
  spec: null,
  tested: false,
});
const link = (from: string, to: string): MapImport => ({ from, to });

/** Two clumps joined by two threads, one each way, so direction has something to say. */
const twoGroups = (() => {
  const names = ["a1", "a2", "a3", "b1", "b2", "b3"].map((n) => `core/src/${n}.rs`);
  const imports = [
    link("core/src/a1.rs", "core/src/a2.rs"),
    link("core/src/a2.rs", "core/src/a3.rs"),
    link("core/src/a3.rs", "core/src/a1.rs"),
    link("core/src/b1.rs", "core/src/b2.rs"),
    link("core/src/b2.rs", "core/src/b3.rs"),
    link("core/src/b3.rs", "core/src/b1.rs"),
    link("core/src/a1.rs", "core/src/b1.rs"),
  ];
  return { modules: names.map(mod), imports };
})();

describe("what a community is called", () => {
  it("carries its side when the project has more than one", () => {
    // `transcription`, `transcription (2)`: nothing said which was the core's and which the
    // shell's, so every numbered row had to be opened to find out.
    const modules = ["core/src/job.rs", "core/src/run.rs", "shell/src/job.ts", "shell/src/run.ts"].map(mod);
    const matrix = buildCommunities(modules, [
      link("core/src/run.rs", "core/src/job.rs"),
      link("shell/src/run.ts", "shell/src/job.ts"),
    ]);
    expect([...matrix.order].sort()).toEqual(["core·job", "shell·job"]);
  });

  it("carries no side in a project that has only one", () => {
    const matrix = buildCommunities(twoGroups.modules, twoGroups.imports);
    expect(matrix.order.every((title) => !title.includes("·"))).toBe(true);
  });
});

describe("what a community touches", () => {
  it("keeps the two directions apart, because they are opposite facts", () => {
    const matrix = buildCommunities(twoGroups.modules, twoGroups.imports);
    const [first, second] = matrix.order;
    const there = trafficFor(matrix, first);
    const back = trafficFor(matrix, second);
    // One thread crosses. Whichever end it leaves from, it is `uses` there and
    // `usedBy` at the other — and never both at either.
    const crossings = there.uses.length + there.usedBy.length;
    expect(crossings).toBe(1);
    expect(back.uses.length + back.usedBy.length).toBe(1);
    expect(there.uses.length).toBe(back.usedBy.length === 1 ? 1 : 0);
  });

  it("carries the weight, so a single import does not look like thirty", () => {
    const matrix = buildCommunities(twoGroups.modules, twoGroups.imports);
    const said = matrix.order.flatMap((title) => {
      const { uses, usedBy } = trafficFor(matrix, title);
      return [...uses, ...usedBy];
    });
    expect(said.every((one) => one.weight >= 1)).toBe(true);
  });

  it("says nothing about a community that touches none", () => {
    const alone = [mod("core/src/x.rs"), mod("core/src/y.rs")];
    const matrix = buildCommunities(alone, [link("core/src/x.rs", "core/src/y.rs")]);
    const { uses, usedBy } = trafficFor(matrix, matrix.order[0]);
    expect(uses).toEqual([]);
    expect(usedBy).toEqual([]);
  });
});

describe("one file and what touches it", () => {
  // A hub with five leaves, and a sixth file nothing here reaches.
  const hub = "core/src/hub.rs";
  const leaves = ["l1", "l2", "l3", "l4", "l5"].map((n) => `core/src/${n}.rs`);
  const far = "core/src/far.rs";
  const members = [hub, ...leaves, far];
  const imports = [
    ...leaves.map((leaf) => link(leaf, hub)),
    link(far, leaves[0]),
  ];

  it("draws the centre and its direct neighbours, and stops there", () => {
    const slice = sliceAround(members, imports, hub);
    expect(slice.members).toHaveLength(6);
    expect(slice.members).toContain(hub);
    // `far` reaches the hub in two steps. Two steps out is the density that
    // refused in the first place, arriving one ring later.
    expect(slice.members).not.toContain(far);
  });

  it("keeps both directions, because a neighbour is one either way", () => {
    const slice = sliceAround(members, imports, leaves[0]);
    expect(slice.members).toContain(hub); // this one imports it
    expect(slice.members).toContain(far); // and this one imports this one
  });

  it("is a file on its own when nothing touches it", () => {
    const slice = sliceAround(members, imports, "core/src/nobody.rs");
    expect(slice.members).toEqual(["core/src/nobody.rs"]);
    expect(neighbourCount(members, imports, "core/src/nobody.rs")).toBe(0);
  });

  it("draws where the whole community would not", () => {
    // Every file importing every other: the density that refuses. One
    // neighbourhood inside it is small enough to be a picture.
    const dense = Array.from({ length: 50 }, (_, n) => `core/src/d${n}.rs`);
    const every: MapImport[] = [];
    for (const from of dense) for (const to of dense) if (from !== to) every.push(link(from, to));
    // The slice of a clique is the clique, so this one still refuses — and says
    // so rather than drawing it. The guarantee is honesty, not a picture.
    const slice = sliceAround(dense, every, dense[0]);
    expect(slice.refused.length).toBeGreaterThan(0);

    // A chain is the ordinary case, and there a neighbourhood is three boxes.
    const chain = dense.slice(1).map((to, at) => link(dense[at], to));
    const small = sliceAround(dense, chain, dense[25]);
    expect(small.members).toHaveLength(3);
    expect(small.refused).toEqual([]);
  });

  it("counts the neighbours without the centre, which is what a picker shows", () => {
    expect(neighbourCount(members, imports, hub)).toBe(5);
  });
});
