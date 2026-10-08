// @vitest-environment node
import { describe, expect, it } from "vitest";
import type { Known } from "../data/knowledge";
import type { NoteLink, NotesGraph, OwnerNote } from "../data/owner-notes";
import type { GEdgeType, GFilters, GNodeKind } from "./graph-types";
import { neighbours } from "./graph-util";
import { backlinks, buildModel, localModel } from "./graph-model";

const ALL_KINDS: GNodeKind[] = ["note", "knowledge", "project", "contact", "mail", "file"];
const ALL_EDGES: GEdgeType[] = ["relates", "supports", "contradicts", "details", "supersedes", "supersedes_k", "scope"];

function filters(over: Partial<GFilters> = {}): GFilters {
  return {
    state: "all",
    nodeKinds: new Set(ALL_KINDS),
    edgeTypes: new Set(ALL_EDGES),
    showArchived: true,
    showOrphans: true,
    ...over,
  };
}

function note(id: number, over: Partial<OwnerNote> = {}): OwnerNote {
  return { id, text: `note ${id}`, origin: "shell", state: "active", created_at: "t", updated_at: "t", ...over };
}
let linkSeq = 0;
function link(
  note_id: number,
  target_kind: NoteLink["target_kind"],
  target_ref: string,
  over: Partial<NoteLink> = {},
): NoteLink {
  return { id: ++linkSeq, note_id, link_type: "relates", target_kind, target_ref, created_at: "2026-01-01T00:00:00Z", ...over };
}
function known(id: number, over: Partial<Known> = {}): Known {
  return {
    id, layer: "semantic", scope_kind: "machine", scope_id: null, source: "owner", generator: null,
    kind: "memory", title: `row ${id}`, body: "", status: "active", proposal_id: null, supersedes: null,
    origin_run_id: null, evidence: null, observations: null, fingerprint: null, expires_after_runs: null,
    last_confirmed_at: null, shown_count: 0, outcome_count: 0, green_count: 0, last_shown_at: null,
    created_at: "t", activated_at: null, ended_at: null, ...over,
  };
}
function graph(notes: OwnerNote[], links: NoteLink[] = [], targets: NotesGraph["targets"] = []): NotesGraph {
  return { notes, links, targets };
}
const ids = (m: { nodes: { id: string }[] }) => m.nodes.map((n) => n.id);

describe("buildModel nodes", () => {
  it("makes n: nodes for notes and k: nodes for every knowledge row", () => {
    const m = buildModel(graph([note(1)]), [known(7)], [], filters());
    expect(ids(m)).toEqual(["k:7", "n:1"]);
    expect(m.nodes.find((n) => n.id === "k:7")).toMatchObject({
      kind: "knowledge", ref: "7", label: "row 7", layer: "semantic", bucket: "in_force", missing: false,
    });
    expect(m.nodes.find((n) => n.id === "n:1")).toMatchObject({ kind: "note", ref: "1", bucket: "in_force" });
  });

  it("drops archived notes unless showArchived", () => {
    const g = graph([note(1), note(2, { state: "archived" })]);
    expect(ids(buildModel(g, [], [], filters({ showArchived: false })))).toEqual(["n:1"]);
    const shown = buildModel(g, [], [], filters({ showArchived: true }));
    expect(shown.nodes.find((n) => n.id === "n:2")!.bucket).toBe("out");
  });

  it("is deterministic: nodes and edges sorted by id", () => {
    const g = graph([note(3), note(1)], [link(3, "knowledge", "2"), link(1, "knowledge", "1")]);
    const m = buildModel(g, [known(2), known(1)], [], filters());
    expect(ids(m)).toEqual(["k:1", "k:2", "n:1", "n:3"]);
    expect(m.edges.map((e) => e.id)).toEqual([...m.edges.map((e) => e.id)].sort());
  });
});

describe("buildModel note links", () => {
  it("links note to note and note to knowledge", () => {
    const g = graph([note(1), note(2)], [link(1, "note", "2"), link(1, "knowledge", "7", { link_type: "supports" })]);
    const m = buildModel(g, [known(7)], [], filters());
    expect(m.edges.map((e) => e.id)).toEqual(["relates|n:1|n:2", "supports|n:1|k:7"]);
  });

  it("keeps a stub for a note target that is not a kept node, from graph.targets", () => {
    const g = graph(
      [note(1)],
      [link(1, "note", "9"), link(1, "note", "8")],
      [{ kind: "note", ref: "9", label: "Hidden one", missing: false }],
    );
    const m = buildModel(g, [], [], filters());
    expect(m.nodes.find((n) => n.id === "n:9")).toMatchObject({ label: "Hidden one", missing: false });
    // 8 is absent from targets: gone.
    expect(m.nodes.find((n) => n.id === "n:8")).toMatchObject({ label: "8", missing: true });
  });

  it("makes a gone knowledge target a missing stub", () => {
    const m = buildModel(graph([note(1)], [link(1, "knowledge", "99")]), [], [], filters());
    expect(m.nodes.find((n) => n.id === "k:99")).toMatchObject({
      kind: "knowledge", missing: true, label: "knowledge — gone",
    });
  });

  it("makes entity nodes from targets, defaulting label to the ref and missing to false", () => {
    const g = graph(
      [note(1)],
      [link(1, "contact", "c1"), link(1, "mail", "m1")],
      [
        { kind: "contact", ref: "c1", label: "Ana", missing: false },
        { kind: "mail", ref: "m1", label: null, missing: true },
      ],
    );
    const m = buildModel(g, [], [], filters());
    expect(m.nodes.find((n) => n.id === "contact:c1")).toMatchObject({ kind: "contact", ref: "c1", label: "Ana", missing: false });
    expect(m.nodes.find((n) => n.id === "mail:m1")).toMatchObject({ label: "m1", missing: true });
    const bare = buildModel(graph([note(1)], [link(1, "file", "f")]), [], [], filters());
    expect(bare.nodes.find((n) => n.id === "file:f")).toMatchObject({ label: "f", missing: false });
  });

  it("ignores links from notes that were not kept", () => {
    const g = graph([note(1, { state: "archived" })], [link(1, "contact", "c1")]);
    const m = buildModel(g, [], [], filters({ showArchived: false }));
    expect(m.nodes).toEqual([]);
    expect(m.edges).toEqual([]);
  });

  it("honours edgeTypes and nodeKinds", () => {
    const g = graph([note(1)], [link(1, "contact", "c1"), link(1, "knowledge", "7", { link_type: "contradicts" })]);
    const noContradicts = buildModel(g, [known(7)], [], filters({ edgeTypes: new Set<GEdgeType>(["relates"]) }));
    expect(noContradicts.edges.map((e) => e.target)).toEqual(["contact:c1"]);
    const noContacts = buildModel(g, [known(7)], [], filters({ nodeKinds: new Set<GNodeKind>(["note", "knowledge"]) }));
    expect(ids(noContacts)).toEqual(["k:7", "n:1"]);
    expect(noContacts.edges.map((e) => e.target)).toEqual(["k:7"]);
  });

  it("dedupes identical links", () => {
    const g = graph([note(1)], [link(1, "contact", "c1"), link(1, "contact", "c1")]);
    expect(buildModel(g, [], [], filters()).edges).toHaveLength(1);
  });
});

describe("buildModel supersedes_k and scope", () => {
  it("draws k:<id> -> k:<supersedes> when both exist", () => {
    const m = buildModel(graph([]), [known(1), known(2, { supersedes: 1 })], [], filters());
    expect(m.edges).toEqual([{ id: "supersedes_k|k:2|k:1", source: "k:2", target: "k:1", type: "supersedes_k" }]);
  });

  it("skips supersedes_k when the old row is not a node", () => {
    const m = buildModel(graph([]), [known(2, { supersedes: 1 })], [], filters());
    expect(m.edges).toEqual([]);
  });

  it("scopes a project row to a project node", () => {
    const rows = [known(1, { scope_kind: "project", scope_id: "p1" })];
    const m = buildModel(graph([]), rows, [{ project_id: "p1" }], filters());
    expect(m.edges.map((e) => e.id)).toEqual(["scope|k:1|project:p1"]);
    expect(m.nodes.find((n) => n.id === "project:p1")).toMatchObject({
      kind: "project", label: "p1", missing: false, degree: 1,
    });
  });

  it("marks a scope project missing when the roster does not have it", () => {
    const rows = [known(1, { scope_kind: "project", scope_id: "ghost" })];
    const m = buildModel(graph([]), rows, [{ project_id: "p1" }], filters());
    expect(m.nodes.find((n) => n.id === "project:ghost")!.missing).toBe(true);
  });

  it("calls no scope project missing while the roster is unknown", () => {
    const rows = [known(1, { scope_kind: "project", scope_id: "ghost" })];
    const m = buildModel(graph([]), rows, undefined, filters());
    expect(m.nodes.find((n) => n.id === "project:ghost")!.missing).toBe(false);
  });

  it("leaves a machine-scope row with no links as a hidden orphan, but a project row is not", () => {
    const rows = [known(1), known(2, { scope_kind: "project", scope_id: "p1" })];
    const hidden = buildModel(graph([]), rows, [{ project_id: "p1" }], filters({ showOrphans: false }));
    expect(ids(hidden)).toEqual(["k:2", "project:p1"]);
    expect(ids(buildModel(graph([]), rows, [{ project_id: "p1" }], filters({ showOrphans: true })))).toContain("k:1");
  });

  it("does not create a project node when its only row is filtered out", () => {
    const rows = [known(1, { scope_kind: "project", scope_id: "p1", status: "rejected" })];
    const m = buildModel(graph([]), rows, [{ project_id: "p1" }], filters({ state: "in_force", showOrphans: true }));
    expect(m.nodes).toEqual([]);
  });

  it("shares one node between a note link to project:p1 and a row scoped to p1", () => {
    // ASSUMPTION: a note link's target_ref for `project` is the project_id (the backend has no SQL
    // table for project targets; refs are free text resolved against the roster).
    const g = graph([note(1)], [link(1, "project", "p1")]);
    const rows = [known(5, { scope_kind: "project", scope_id: "p1" })];
    const m = buildModel(g, rows, [{ project_id: "p1" }], filters());
    expect(m.nodes.filter((n) => n.id === "project:p1")).toHaveLength(1);
    expect(m.nodes.find((n) => n.id === "project:p1")!.degree).toBe(2);
    expect(m.edges.map((e) => e.id).sort()).toEqual(["relates|n:1|project:p1", "scope|k:5|project:p1"]);
  });
});

describe("buildModel state filter and degree", () => {
  const rows = [
    known(1, { status: "active" }),
    known(2, { status: "proposed" }),
    known(3, { status: "rejected" }),
    known(4, { status: "mystery" as Known["status"] }),
  ];
  it("in_force keeps proposed too, in the graph", () => {
    expect(ids(buildModel(graph([]), rows, [], filters({ state: "in_force" })))).toEqual(["k:1", "k:2"]);
  });
  it("out keeps only the ended ones; all keeps everything", () => {
    expect(ids(buildModel(graph([]), rows, [], filters({ state: "out" })))).toEqual(["k:3"]);
    expect(ids(buildModel(graph([]), rows, [], filters({ state: "all" })))).toHaveLength(4);
  });
  it("drops edges whose end was filtered, and the degree follows", () => {
    const g = graph([note(1)], [link(1, "knowledge", "3"), link(1, "knowledge", "1")]);
    const m = buildModel(g, rows, [], filters({ state: "in_force", showOrphans: true }));
    expect(m.edges.map((e) => e.target)).toEqual(["k:1"]);
    expect(m.nodes.find((n) => n.id === "n:1")!.degree).toBe(1);
  });
  it("hides orphans after filtering unless showOrphans", () => {
    const g = graph([note(1), note(2)], [link(1, "knowledge", "1")]);
    const m = buildModel(g, [known(1)], [], filters({ showOrphans: false }));
    expect(ids(m)).toEqual(["k:1", "n:1"]);
  });
});

describe("localModel", () => {
  const g = graph(
    [note(1), note(2), note(3), note(4)],
    [link(1, "note", "2"), link(2, "note", "3"), link(3, "note", "4")],
  );
  const model = buildModel(g, [], [], filters());

  it("depth 1 / 2 pick the neighbourhood and keep only induced edges", () => {
    const one = localModel(model, "n:2", 1);
    expect(ids(one)).toEqual(["n:1", "n:2", "n:3"]);
    expect(one.edges.map((e) => e.id)).toEqual(["relates|n:1|n:2", "relates|n:2|n:3"]);
    const two = localModel(model, "n:1", 2);
    expect(ids(two)).toEqual(["n:1", "n:2", "n:3"]);
    expect(two.edges).toHaveLength(2);
    expect(new Set(ids(two))).toEqual(neighbours(model, "n:1", 2));
  });

  it("recomputes degree within the subgraph", () => {
    const one = localModel(model, "n:1", 1);
    expect(one.nodes.find((n) => n.id === "n:2")!.degree).toBe(1);
  });
});

describe("backlinks", () => {
  const links = [
    link(1, "note", "5", { created_at: "2026-01-01T00:00:00Z" }),
    link(2, "note", "5", { created_at: "2026-03-01T00:00:00Z" }),
    link(3, "knowledge", "5", { created_at: "2026-02-01T00:00:00Z" }),
    link(4, "knowledge", "50", { created_at: "2026-02-01T00:00:00Z" }),
  ];
  const g = graph([], links);
  it("lists links to a note, newest first", () => {
    expect(backlinks(g, "note", "5").map((l) => l.note_id)).toEqual([2, 1]);
  });
  it("lists links to a knowledge row, matching the ref exactly", () => {
    expect(backlinks(g, "knowledge", "5").map((l) => l.note_id)).toEqual([3]);
  });
});
