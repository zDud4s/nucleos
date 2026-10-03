import { describe, expect, it } from "vitest";
import { layout, toGraph, type BrainFilters } from "./brain-graph";
import type { Known } from "./knowledge";
import type { NoteLink, NotesGraph, OwnerNote } from "./owner-notes";

const FILTERS: BrainFilters = {
  knowledge: "linked",
  linkTypes: new Set(["relates", "supports", "contradicts", "details", "supersedes"]),
  kinds: new Set(["note", "knowledge", "project", "contact", "mail", "file"]),
  showArchived: false,
};

function note(id: number): OwnerNote {
  return {
    id,
    text: `note ${id}`,
    origin: "shell",
    state: "active",
    created_at: "2026-10-01T00:00:00Z",
    updated_at: "2026-10-01T00:00:00Z",
  };
}

function link(id: number, noteId: number, over: Partial<NoteLink> = {}): NoteLink {
  return {
    id,
    note_id: noteId,
    link_type: "relates",
    target_kind: "note",
    target_ref: "2",
    created_at: "2026-10-01T00:00:00Z",
    ...over,
  };
}

function known(id: number): Known {
  return { id, title: `row ${id}` } as Known;
}

function graph(links: NoteLink[]): NotesGraph {
  return { notes: [note(1), note(2)], links, targets: [] };
}

describe("toGraph", () => {
  it("an entity appears only when a note links it", () => {
    const without = toGraph(graph([link(1, 1)]), [], FILTERS);
    expect(without.nodes.some((n) => n.type === "entity")).toBe(false);

    const withLink = toGraph(
      {
        ...graph([link(1, 1, { target_kind: "project", target_ref: "alpha" })]),
        targets: [{ kind: "project", ref: "alpha", label: "alpha", missing: true }],
      },
      [],
      FILTERS,
    );
    const entity = withLink.nodes.find((n) => n.id === "project:alpha");
    expect(entity?.type).toBe("entity");
    expect(entity?.data.missing).toBe(true);
    expect(withLink.meta.entities).toBe(1);
  });

  it("knowledge filter linked keeps only linked rows", () => {
    const g = graph([link(1, 1, { target_kind: "knowledge", target_ref: "10" })]);
    const rows = [known(10), known(11)];

    const linked = toGraph(g, rows, FILTERS);
    expect(linked.nodes.filter((n) => n.type === "knowledge").map((n) => n.id)).toEqual(["k:10"]);

    expect(toGraph(g, rows, { ...FILTERS, knowledge: "all" }).meta.knowledge).toBe(2);

    const none = toGraph(g, rows, { ...FILTERS, knowledge: "none" });
    expect(none.meta.knowledge).toBe(0);
    expect(none.edges).toHaveLength(0);
  });

  it("duplicate links collapse to one edge", () => {
    const out = toGraph(graph([link(1, 1), link(2, 1)]), [], FILTERS);
    expect(out.edges).toHaveLength(1);
    expect(out.edges[0].id).toBe("relates|n:1|n:2");
  });
});

describe("layout", () => {
  it("gives every node a finite position", () => {
    const g = toGraph(
      graph([link(1, 1), link(2, 2, { target_kind: "project", target_ref: "alpha" })]),
      [],
      FILTERS,
    );
    const placed = layout(g);
    expect(placed).toHaveLength(g.nodes.length);
    for (const node of placed) {
      expect(Number.isFinite(node.position.x)).toBe(true);
      expect(Number.isFinite(node.position.y)).toBe(true);
    }
  });
});
