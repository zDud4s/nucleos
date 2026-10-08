import { describe, expect, it } from "vitest";
import { formatItem, itemOfNode, nodeIdOf, parseItem, parseView } from "./item-ref";

describe("parseItem", () => {
  it.each([
    ["note:45", { kind: "note", id: 45 }],
    ["knowledge:123", { kind: "knowledge", id: 123 }],
    ["note:1", { kind: "note", id: 1 }],
  ])("reads %s", (raw, expected) => {
    expect(parseItem(raw)).toEqual(expected);
  });

  it.each([
    null,
    undefined,
    42,
    "",
    "note",
    "note:",
    "note:0",
    "note:-3",
    "note:1.5",
    "note:abc",
    "note:01x",
    "knowledge:",
    "project:3",
    "Note:3",
    "note:3:4",
    " note:3",
  ])("refuses %j", (raw) => {
    expect(parseItem(raw)).toBeNull();
  });
});

describe("formatItem / nodeIdOf / itemOfNode", () => {
  it("round-trips through the query string form", () => {
    for (const ref of [
      { kind: "note", id: 45 },
      { kind: "knowledge", id: 123 },
    ] as const) {
      expect(parseItem(formatItem(ref))).toEqual(ref);
    }
    expect(formatItem({ kind: "note", id: 45 })).toBe("note:45");
    expect(formatItem({ kind: "knowledge", id: 123 })).toBe("knowledge:123");
  });

  it("maps to graph node ids and back", () => {
    expect(nodeIdOf({ kind: "note", id: 45 })).toBe("n:45");
    expect(nodeIdOf({ kind: "knowledge", id: 123 })).toBe("k:123");
    expect(nodeIdOf({ kind: "capture", id: 7 })).toBeNull();
    expect(parseItem("capture:7")).toEqual({ kind: "capture", id: 7 });
    expect(formatItem({ kind: "capture", id: 7 })).toBe("capture:7");
    expect(itemOfNode("n:45")).toEqual({ kind: "note", id: 45 });
    expect(itemOfNode("k:123")).toEqual({ kind: "knowledge", id: 123 });
  });

  it("gives no item for entity nodes or malformed ids", () => {
    for (const id of ["project:p1", "file:src/a.ts", "n:", "n:0", "n:x", "k:-1", "", "45"]) {
      expect(itemOfNode(id)).toBeNull();
    }
  });
});

describe("parseView", () => {
  it("defaults to the list", () => {
    expect(parseView(undefined)).toBe("list");
    expect(parseView("nonsense")).toBe("list");
    expect(parseView(7)).toBe("list");
  });

  it("accepts the two views", () => {
    expect(parseView("list")).toBe("list");
    expect(parseView("graph")).toBe("graph");
  });
});
