// @vitest-environment node
// §spec mapa-do-projeto
import { describe, expect, it } from "vitest";

import { buildFileItems, fileFacts, itemLinks, labelFor } from "./map-items";
import type { FileItem, FileItems } from "../data/project-map";

const item = (id: string, over: Partial<FileItem> = {}): FileItem => ({
  id,
  name: id.includes("::") ? id.split("::")[1] : id,
  container: id.includes("::") ? id.split("::")[0] : null,
  kind: "function",
  exported: false,
  documented: false,
  line: 1,
  ...over,
});

const file = (items: FileItem[], references: { from: string; to: string }[] = []): FileItems => ({
  path: "core/src/a.rs",
  reader: "rust",
  items,
  references,
  missed: [],
});

describe("labelFor", () => {
  it("uses the bare name, which is what the reader came for", () => {
    const label = labelFor([item("Door::open"), item("shut")]);
    expect(label("Door::open")).toBe("open");
    expect(label("shut")).toBe("shut");
  });

  it("falls back to the qualified id only where the short name would lie", () => {
    // A file with `A::new` and `B::new` has two different functions called `new`. Two boxes
    // labelled the same is the quiet kind of wrong this map exists to catch.
    const label = labelFor([item("A::new"), item("B::new"), item("alone")]);
    expect(label("A::new")).toBe("A::new");
    expect(label("B::new")).toBe("B::new");
    expect(label("alone")).toBe("alone");
  });
});

describe("itemLinks", () => {
  it("drops an edge whose end is not a box rather than drawing it to nowhere", () => {
    const links = itemLinks(
      [item("a"), item("b")],
      [
        { from: "a", to: "b" },
        { from: "a", to: "gone" },
      ],
    );
    expect(links).toEqual([{ from: "a", to: "b", weight: 1 }]);
  });

  it("never draws a box to itself, because recursion is a property of the box", () => {
    expect(itemLinks([item("walk")], [{ from: "walk", to: "walk" }])).toEqual([]);
  });
});

describe("fileFacts", () => {
  it("counts what the file offers, what it explains, and what nothing reaches", () => {
    const found = file(
      [
        item("open", { exported: true, documented: true }),
        item("shut", { documented: true }),
        item("stranded"),
      ],
      [{ from: "open", to: "shut" }],
    );
    expect(fileFacts(found)).toEqual({
      items: 3,
      exported: 1,
      documented: 2,
      // `shut` is reached, `open` is reachable from outside; only `stranded` is neither.
      unreachable: 1,
    });
  });

  it("does not call an exported declaration unreachable just because this file ignores it", () => {
    // A file's whole point may be to offer something nothing in it uses. Counting that as dead
    // would report the public surface of every leaf module as debt.
    const found = file([item("offered", { exported: true })]);
    expect(fileFacts(found).unreachable).toBe(0);
  });
});

describe("buildFileItems", () => {
  it("lays the declarations out with a caller above what it calls", () => {
    const found = file(
      [item("caller"), item("helper")],
      [{ from: "caller", to: "helper" }],
    );
    const drawing = buildFileItems(found);
    expect(drawing.refused).toEqual([]);
    const at = new Map(drawing.drawn.nodes.map((node) => [node.id, node]));
    expect(at.get("caller")!.y).toBeLessThan(at.get("helper")!.y);
  });

  it("labels a box with the name and keys it by the id", () => {
    const drawing = buildFileItems(file([item("Door::open")]));
    const [node] = drawing.drawn.nodes;
    expect(node.id).toBe("Door::open");
    expect(node.lines.join("")).toBe("open");
  });

  it("refuses a file with more declarations than a picture holds, and says the number", () => {
    // 45 boxes is past the measured limit. `http.rs` declares 385, and a drawing of it would look
    // like an answer while being unreadable — the one failure this whole map exists to refuse.
    const many = Array.from({ length: 45 }, (_, i) => item(`f${i}`));
    const drawing = buildFileItems(file(many));
    expect(drawing.refused.length).toBeGreaterThan(0);
    expect(drawing.refused.join(" ")).toMatch(/45 boxes/);
  });

  it("draws a file whose declarations use nothing of each other", () => {
    // A file of types has no edges and is still the file's contents. Dropping the boxes with no
    // line on them would leave a types-only module looking empty.
    const drawing = buildFileItems(file([item("A"), item("B"), item("C")]));
    expect(drawing.refused).toEqual([]);
    expect(drawing.drawn.nodes.length).toBe(3);
  });
});
