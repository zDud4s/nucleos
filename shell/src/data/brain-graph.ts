import dagre from "@dagrejs/dagre";
import type { Edge, Node } from "@xyflow/react";
import type { Known } from "./knowledge";
import type { NotesGraph } from "./owner-notes";

/**
 * The notes graph as `@xyflow/react` nodes and edges.
 *
 * Pure: `toGraph` filters and shapes, `layout` places. Positions are left at the
 * origin by `toGraph` because the layout depends on which nodes survived the
 * filters, and a filter change must re-place them.
 *
 * Node ids: `n:<id>` a note, `k:<id>` a knowledge row, `<kind>:<ref>` any other
 * target. A link whose target is a note or a knowledge row points at the first
 * two, so one thing is one node however it is reached.
 */

export type BrainNodeType = "note" | "knowledge" | "entity";

export interface BrainFilters {
  knowledge: "linked" | "all" | "none";
  linkTypes: Set<string>;
  kinds: Set<string>;
  showArchived: boolean;
}

export interface BrainMeta {
  notes: number;
  knowledge: number;
  entities: number;
  edges: number;
}

export interface BrainGraph {
  nodes: Node[];
  edges: Edge[];
  meta: BrainMeta;
}

const NOTE_SIZE: [number, number] = [250, 92];
const OTHER_SIZE: [number, number] = [190, 28];

function sizeOf(node: Node): [number, number] {
  return node.type === "note" ? NOTE_SIZE : OTHER_SIZE;
}

function targetId(kind: string, ref: string): string {
  if (kind === "note") return `n:${ref}`;
  if (kind === "knowledge") return `k:${ref}`;
  return `${kind}:${ref}`;
}

export function toGraph(graph: NotesGraph, knowledge: Known[], filters: BrainFilters): BrainGraph {
  const nodes = new Map<string, Node>();
  const edges = new Map<string, Edge>();
  const knownById = new Map(knowledge.map((row) => [String(row.id), row]));
  const targetInfo = new Map(graph.targets.map((t) => [`${t.kind}:${t.ref}`, t]));

  const noteIds = new Set<string>();
  for (const note of graph.notes) {
    if (note.state === "archived" && !filters.showArchived) continue;
    noteIds.add(String(note.id));
    nodes.set(`n:${note.id}`, {
      id: `n:${note.id}`,
      type: "note",
      position: { x: 0, y: 0 },
      data: { note },
    });
  }

  const knowledgeNode = (ref: string): Node => ({
    id: `k:${ref}`,
    type: "knowledge",
    position: { x: 0, y: 0 },
    data: { known: knownById.get(ref) ?? null, missing: !knownById.has(ref) },
  });

  // "all" keeps every row, linked or not.
  if (filters.knowledge === "all") {
    for (const row of knowledge) nodes.set(`k:${row.id}`, knowledgeNode(String(row.id)));
  }

  for (const link of graph.links) {
    if (!noteIds.has(String(link.note_id))) continue;
    if (!filters.linkTypes.has(link.link_type)) continue;
    if (!filters.kinds.has(link.target_kind)) continue;
    if (link.target_kind === "knowledge" && filters.knowledge === "none") continue;

    const source = `n:${link.note_id}`;
    const target = targetId(link.target_kind, link.target_ref);

    if (!nodes.has(target)) {
      if (link.target_kind === "knowledge") {
        nodes.set(target, knowledgeNode(link.target_ref));
      } else if (link.target_kind === "note") {
        // A note that is archived and hidden, or gone: kept as a missing stub
        // so the link does not silently vanish.
        nodes.set(target, {
          id: target,
          type: "note",
          position: { x: 0, y: 0 },
          data: { note: null, missing: true },
        });
      } else {
        const info = targetInfo.get(`${link.target_kind}:${link.target_ref}`);
        nodes.set(target, {
          id: target,
          type: "entity",
          position: { x: 0, y: 0 },
          data: {
            kind: link.target_kind,
            ref: link.target_ref,
            label: info?.label ?? link.target_ref,
            missing: info?.missing ?? false,
          },
        });
      }
    }

    const id = `${link.link_type}|${source}|${target}`;
    if (!edges.has(id)) {
      edges.set(id, { id, source, target, label: link.link_type, data: { linkType: link.link_type } });
    }
  }

  const all = [...nodes.values()];
  return {
    nodes: all,
    edges: [...edges.values()],
    meta: {
      notes: all.filter((n) => n.type === "note").length,
      knowledge: all.filter((n) => n.type === "knowledge").length,
      entities: all.filter((n) => n.type === "entity").length,
      edges: edges.size,
    },
  };
}

/** Place every node left to right; dagre answers centres, a node wants its top-left. */
export function layout(g: { nodes: Node[]; edges: Edge[] }): Node[] {
  const dg = new dagre.graphlib.Graph();
  dg.setGraph({ rankdir: "LR", nodesep: 26, ranksep: 130, marginx: 20, marginy: 20 });
  dg.setDefaultEdgeLabel(() => ({}));

  for (const node of g.nodes) {
    const [width, height] = sizeOf(node);
    dg.setNode(node.id, { width, height });
  }
  for (const edge of g.edges) dg.setEdge(edge.source, edge.target);

  dagre.layout(dg);

  return g.nodes.map((node) => {
    const placed = dg.node(node.id);
    const [width, height] = sizeOf(node);
    return { ...node, position: { x: placed.x - width / 2, y: placed.y - height / 2 } };
  });
}
