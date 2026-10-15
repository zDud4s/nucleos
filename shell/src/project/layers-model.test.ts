// §spec mapa-do-projeto
import { describe, expect, it } from "vitest";
import { buildBands, isolate, maxLayer } from "./layers-model";
import type { Layering, MapImport, MapModule } from "../data/project-map";

function mod(path: string): MapModule {
  return {
    path,
    reader: "rust",
    declares: false,
    cites: [],
    spec: null,
    tested: false,
  } as MapModule;
}

const modules: MapModule[] = [
  mod("core/src/low/a.rs"),
  mod("core/src/low/b.rs"),
  mod("core/src/mid/c.rs"),
  mod("core/src/top/d.rs"),
  mod("shell/src/ui/e.ts"),
];

const layering: Layering = {
  modules: {
    "core/src/low/a.rs": 0,
    "core/src/low/b.rs": 0,
    "core/src/mid/c.rs": 1,
    "core/src/top/d.rs": 2,
    "shell/src/ui/e.ts": 2,
  },
  overridden: [],
  violations: [
    // layer 0 importing layer 1, absent from the frozen baseline
    {
      from: "core/src/low/a.rs",
      to: "core/src/mid/c.rs",
      from_layer: 0,
      to_layer: 1,
      new: true,
    },
    // layer 1 importing layer 2, already in the baseline
    {
      from: "core/src/mid/c.rs",
      to: "core/src/top/d.rs",
      from_layer: 1,
      to_layer: 2,
      new: false,
    },
  ],
};

const imports: MapImport[] = [
  { from: "core/src/top/d.rs", to: "core/src/mid/c.rs" },
  { from: "shell/src/ui/e.ts", to: "core/src/mid/c.rs" },
  { from: "core/src/mid/c.rs", to: "core/src/low/a.rs" },
  { from: "core/src/low/a.rs", to: "core/src/mid/c.rs" },
  { from: "core/src/mid/c.rs", to: "core/src/top/d.rs" },
  { from: "core/src/low/b.rs", to: "core/src/low/a.rs" },
];

describe("layers-model", () => {
  it("bands run from the top layer down grouped by folder", () => {
    const bands = buildBands(modules, layering);
    expect(bands.map((band) => band.layer)).toEqual([2, 1, 0]);
    expect(maxLayer(layering)).toBe(2);

    const [top, mid, low] = bands;
    expect(top.blocks.map((block) => block.key)).toEqual(["core/top", "shell/ui"]);
    expect(top.files).toBe(2);
    expect(mid.blocks.map((block) => block.key)).toEqual(["core/mid"]);
    expect(low.blocks).toHaveLength(1);
    expect(low.blocks[0].key).toBe("core/low");
    expect(low.blocks[0].layer).toBe(0);
    expect(low.blocks[0].files).toEqual(["core/src/low/a.rs", "core/src/low/b.rs"]);
    expect(low.files).toBe(2);
  });

  it("counts upward imports per band and the new ones apart", () => {
    const [top, mid, low] = buildBands(modules, layering);
    // a violation counts on the band of its importer
    expect(top.upward).toBe(0);
    expect(top.upwardNew).toBe(0);
    expect(mid.upward).toBe(1);
    expect(mid.upwardNew).toBe(0);
    expect(low.upward).toBe(1);
    expect(low.upwardNew).toBe(1);
    expect(low.blocks[0].upward).toBe(1);
    expect(low.blocks[0].upwardNew).toBe(1);
    expect(mid.blocks[0].upward).toBe(1);
    expect(mid.blocks[0].upwardNew).toBe(0);
  });

  it("isolating a band aggregates its edges by neighbouring layer", () => {
    const bands = buildBands(modules, layering);
    const view = isolate(bands, imports, layering, 1);
    expect(view.band.layer).toBe(1);
    expect(view.band.blocks.map((block) => block.key)).toEqual(["core/mid"]);
    // into layer 1: two from layer 2 (d, e), one from layer 0 (a)
    expect(view.into.get(2)).toBe(2);
    expect(view.into.get(0)).toBe(1);
    // out of layer 1: one down to layer 0 (c -> a), one up to layer 2 (c -> d)
    expect(view.outOf.get(0)).toBe(1);
    expect(view.outOf.get(2)).toBe(1);
    // edges inside layer 0 never touch layer 1
    expect(view.into.has(1)).toBe(false);
    expect(view.outOf.has(1)).toBe(false);
  });
});
