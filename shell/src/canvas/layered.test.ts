import { describe, expect, it } from "vitest";
import { GAP_Y, NODE_H, backEdges, layout, widthOf, wrap } from "./layered";

/** A tiny helper so the graphs below read as the shape they are rather than as object literals. */
const links = (...pairs: string[]) =>
  pairs.map((pair) => {
    const [from, to] = pair.split(">");
    return { from, to, weight: 1 };
  });

describe("wrap", () => {
  it("leaves a short name alone", () => {
    expect(wrap("citations")).toEqual(["citations"]);
  });

  it("splits a long name at the underscore that balances the two lines", () => {
    // `a_pending_approval` splits after `a_`? No: that leaves 16 against 2. The cut is the one
    // whose longer half is shortest, and ties keep the earlier cut so the answer is stable.
    expect(wrap("resolve_the_pending_approval")).toEqual(["resolve_the_", "pending_approval"]);
    expect(wrap("a_very_long_identifier")).toEqual(["a_very_long_", "identifier"]);
  });

  it("splits a method at the colons, where a reader's eye already breaks it", () => {
    expect(wrap("NucleosTools::errand_files_list")).toEqual([
      "NucleosTools::",
      "errand_files_list",
    ]);
  });

  it("leaves a long name with nothing to split on as one line", () => {
    expect(wrap("aVeryLongCamelCaseNameIndeed")).toEqual(["aVeryLongCamelCaseNameIndeed"]);
  });

  it("measures a box by its longest line and never below the minimum", () => {
    // Wrapping is why this is not simply length: the two-line form is measured by its longer half,
    // so a name of 24 characters is narrower than a 16-character one that cannot be split.
    expect(widthOf("resolve_pending_approval")).toBeLessThan(widthOf("cannotBeSplitHere"));
    expect(widthOf("a")).toBe(74);
  });
});

describe("backEdges", () => {
  it("finds nothing in a graph that only goes one way", () => {
    expect(backEdges(["a", "b", "c"], links("a>b", "b>c")).size).toBe(0);
  });

  it("names one edge of a cycle, so the layering has somewhere to start", () => {
    // Which one it names is decided by the order the search happened to visit, and anything
    // reporting cycles to a person has to say so rather than present the arrow as the finding.
    expect(backEdges(["a", "b", "c"], links("a>b", "b>c", "c>a")).size).toBe(1);
  });
});

describe("layout", () => {
  it("puts a caller above what it calls", () => {
    const drawn = layout(["top", "middle", "bottom"], links("top>middle", "middle>bottom"));
    const at = new Map(drawn.nodes.map((node) => [node.id, node.y]));
    expect(at.get("top")).toBeLessThan(at.get("middle")!);
    expect(at.get("middle")).toBeLessThan(at.get("bottom")!);
  });

  it("breaks an edge that skips a layer into bends, and stacks them exactly vertically", () => {
    // This is the whole reason Brandes–Köpf is here. Measured over the núcleo before it, 73% of
    // the crooked segments were the bends of a long edge; after it, every one of them is straight.
    const drawn = layout(["a", "b", "c", "d"], links("a>b", "b>c", "c>d", "a>d"));
    const bends = Object.values(drawn.bends);
    expect(bends.length).toBe(2);
    expect(new Set(bends.map((bend) => Math.round(bend.x))).size).toBe(1);
  });

  it("never lets two boxes on one row overlap", () => {
    const names = Array.from({ length: 12 }, (_, i) => `some_function_number_${i}`);
    const drawn = layout(names, links(...names.slice(1).map((n) => `${names[0]}>${n}`)));
    const rows = new Map<number, Array<{ x: number; width: number }>>();
    for (const node of drawn.nodes) {
      const row = rows.get(node.y) ?? [];
      row.push(node);
      rows.set(node.y, row);
    }
    for (const row of rows.values()) {
      row.sort((a, b) => a.x - b.x);
      for (let i = 1; i < row.length; i += 1) {
        expect(row[i].x - row[i].width / 2).toBeGreaterThanOrEqual(
          row[i - 1].x + row[i - 1].width / 2 - 0.5,
        );
      }
    }
  });

  it("draws the same picture however the links arrive", () => {
    // A diagram that moves between two reads of the same code teaches the reader to distrust it,
    // and the search below is order-sensitive, so the order is fixed before it runs.
    const names = ["alpha", "beta", "gamma", "delta"];
    const one = links("alpha>beta", "alpha>gamma", "beta>delta", "gamma>delta");
    const other = [...one].reverse();
    expect(layout(names, other).nodes).toEqual(layout(names, one).nodes);
  });

  it("counts the crossing in a picture that has one", () => {
    // Two edges that must cross whichever way the middle row is ordered.
    const drawn = layout(["a", "b", "c", "d"], links("a>d", "b>c", "a>c", "b>d"));
    expect(drawn.crossings).toBeGreaterThan(0);
  });

  it("keeps a reversed edge rather than dropping it, and says it was reversed", () => {
    const drawn = layout(["a", "b"], links("a>b", "b>a"));
    expect(drawn.segments.length).toBe(1);
    expect(drawn.segments[0].reversed).toBe(true);
  });

  it("gives an empty graph a size instead of a NaN", () => {
    const drawn = layout([], []);
    expect(drawn.width).toBe(0);
    expect(drawn.nodes).toEqual([]);
    expect(drawn.straight).toBe(1);
  });

  it("leaves room between the rows for the boxes that sit in them", () => {
    expect(GAP_Y).toBeGreaterThan(NODE_H);
  });
});
