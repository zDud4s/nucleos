import type { Known } from "../data/knowledge";
import type { NoteLink, NotesGraph } from "../data/owner-notes";
import type { GEdge, GEdgeType, GFilters, GModel, GNode, GNodeKind } from "./graph-types";
import { neighbours } from "./graph-util";
import { knownBucket, noteBucket, passesState } from "./item-state";

/**
 * The Brain graph model: notes, knowledge rows and the things they point at, as one
 * filtered node/edge set. Pure and deterministic (nodes and edges sorted by id), so the
 * simulation and the tests see the same input for the same data.
 */

function targetId(kind: string, ref: string): string {
  if (kind === "note") return `n:${ref}`;
  if (kind === "knowledge") return `k:${ref}`;
  return `${kind}:${ref}`;
}

/** First line of a note, trimmed: a note has no title of its own. */
function noteLabel(text: string): string {
  const first = text.trim().split("\n")[0] ?? "";
  return first.length > 60 ? `${first.slice(0, 59)}…` : first || "(empty note)";
}

const byId = (a: { id: string }, b: { id: string }) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);

export function buildModel(
  graph: NotesGraph,
  knowledge: Known[],
  /** `undefined` while the roster is unknown (loading or failed): no project is called missing then. */
  projects: { project_id: string }[] | undefined,
  filters: GFilters,
): GModel {
  const nodes = new Map<string, GNode>();
  const edges = new Map<string, GEdge>();
  const targetInfo = new Map(graph.targets.map((t) => [`${t.kind}:${t.ref}`, t]));
  const knownIds = new Set(knowledge.map((row) => String(row.id)));
  const projectIds = projects === undefined ? null : new Set(projects.map((p) => p.project_id));

  const keepsKind = (kind: GNodeKind) => filters.nodeKinds.has(kind);
  /** Adds the node unless its kind or (for notes and knowledge) its Estado is filtered out. */
  const add = (node: Omit<GNode, "degree">): GNode | null => {
    if (!keepsKind(node.kind)) return null;
    if ((node.kind === "note" || node.kind === "knowledge") && !passesState(node.bucket, filters.state, "graph")) {
      return null;
    }
    const placed: GNode = { ...node, degree: 0 };
    nodes.set(node.id, placed);
    return placed;
  };
  const addEdge = (type: GEdgeType, source: string, target: string) => {
    if (!filters.edgeTypes.has(type)) return;
    const id = `${type}|${source}|${target}`;
    if (!edges.has(id)) edges.set(id, { id, source, target, type });
  };

  const keptNoteIds = new Set<string>();
  for (const note of graph.notes) {
    if (note.state === "archived" && !filters.showArchived) continue;
    keptNoteIds.add(String(note.id));
    add({
      id: `n:${note.id}`,
      kind: "note",
      ref: String(note.id),
      label: noteLabel(note.text),
      bucket: noteBucket(note.state),
      missing: false,
    });
  }

  for (const row of knowledge) {
    add({
      id: `k:${row.id}`,
      kind: "knowledge",
      ref: String(row.id),
      label: row.title,
      layer: row.layer,
      bucket: knownBucket(row.status),
      missing: false,
    });
  }

  /** The node a link lands on, made as a stub when nothing else has made it. */
  const resolve = (link: NoteLink): GNode | null => {
    if (link.target_kind === "job") return null;
    const id = targetId(link.target_kind, link.target_ref);
    const existing = nodes.get(id);
    if (existing) return existing;
    const info = targetInfo.get(`${link.target_kind}:${link.target_ref}`);
    if (link.target_kind === "knowledge") {
      // Every real row was added above; one that is absent but known was filtered, not gone.
      if (knownIds.has(link.target_ref)) return null;
      return add({ id, kind: "knowledge", ref: link.target_ref, label: "knowledge — gone", bucket: "in_force", missing: true });
    }
    if (link.target_kind === "note") {
      // Archived and hidden, or gone: the nucleo resolved which; with no answer, it is gone.
      // A hidden archived note is "out", so the Estado filter treats it as the archived thing it is.
      const missing = info?.missing ?? true;
      return add({
        id,
        kind: "note",
        ref: link.target_ref,
        label: info?.label ?? link.target_ref,
        bucket: missing ? "in_force" : "out",
        missing,
      });
    }
    return add({
      id,
      kind: link.target_kind,
      ref: link.target_ref,
      label: info?.label ?? link.target_ref,
      bucket: "in_force",
      missing: info?.missing ?? false,
    });
  };

  for (const link of graph.links) {
    // A job is evidence, not a thing in the graph: its links live in the note panel only.
    if (link.target_kind === "job") continue;
    if (!keptNoteIds.has(String(link.note_id))) continue;
    if (!filters.edgeTypes.has(link.link_type) || !keepsKind(link.target_kind)) continue;
    const source = nodes.get(`n:${link.note_id}`);
    if (!source) continue; // the note itself was filtered by Estado
    const target = resolve(link);
    if (target) addEdge(link.link_type, source.id, target.id);
  }

  for (const row of knowledge) {
    const self = nodes.get(`k:${row.id}`);
    if (!self) continue;
    if (row.supersedes != null) {
      const old = nodes.get(`k:${row.supersedes}`);
      if (old) addEdge("supersedes_k", self.id, old.id);
    }
    if (row.scope_kind === "project" && row.scope_id && filters.edgeTypes.has("scope") && keepsKind("project")) {
      const id = `project:${row.scope_id}`;
      if (!nodes.has(id)) {
        // The roster only tells existence, so the id doubles as the label.
        add({ id, kind: "project", ref: row.scope_id, label: row.scope_id, bucket: "in_force", missing: projectIds !== null && !projectIds.has(row.scope_id) });
      }
      addEdge("scope", self.id, id);
    }
  }

  for (const e of edges.values()) {
    const s = nodes.get(e.source);
    const t = nodes.get(e.target);
    if (s) s.degree += 1;
    if (t) t.degree += 1;
  }

  const kept = [...nodes.values()].filter((n) => filters.showOrphans || n.degree > 0).sort(byId);
  const keptIds = new Set(kept.map((n) => n.id));
  const keptEdges = [...edges.values()].filter((e) => keptIds.has(e.source) && keptIds.has(e.target)).sort(byId);
  return { nodes: kept, edges: keptEdges };
}

/** The induced subgraph around `id`: its neighbours up to `depth`, and only the edges between them. */
export function localModel(model: GModel, id: string, depth: 1 | 2): GModel {
  const keep = neighbours(model, id, depth);
  const edges = model.edges.filter((e) => keep.has(e.source) && keep.has(e.target));
  const degree = new Map<string, number>();
  for (const e of edges) {
    degree.set(e.source, (degree.get(e.source) ?? 0) + 1);
    degree.set(e.target, (degree.get(e.target) ?? 0) + 1);
  }
  const nodes = model.nodes.filter((n) => keep.has(n.id)).map((n) => ({ ...n, degree: degree.get(n.id) ?? 0 }));
  return { nodes, edges };
}

/** Every link pointing at this item, newest first. */
export function backlinks(graph: NotesGraph, kind: "note" | "knowledge", ref: string): NoteLink[] {
  return graph.links
    .filter((l) => l.target_kind === kind && l.target_ref === ref)
    .sort((a, b) => (a.created_at < b.created_at ? 1 : a.created_at > b.created_at ? -1 : b.id - a.id));
}
