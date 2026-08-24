import type { Edge, Node } from "@xyflow/react";
import type { GraphEdge, GraphNode, NodeKind, Role } from "../data/workflow-graph";

/**
 * Where a workflow's nodes go, and what xyflow is handed.
 *
 * **Laid out, not scattered — and that is the difference from `model.ts` next door.** The fleet
 * canvas positions a slot by hashing its key, because slots have no structure: no slot comes before
 * another, so any derived position is as good as any other and the person drags them where they
 * want. A workflow is the opposite: it is a sequence, the sequence is the entire content, and a
 * layout that ignored it would be a picture of the right nodes in the wrong shape.
 *
 * So positions are derived from the edges and are **not** draggable and not stored. Nothing here
 * remembers where somebody put a node, because there is nothing for them to put right — moving
 * `plan` after `gate` on screen would say something false about the workflow.
 */

/** How far apart the columns and the rows sit. Whole numbers, so nothing lands on a half pixel. */
export const COLUMN = 280;
export const ROW = 120;

export interface Point {
  x: number;
  y: number;
}

/**
 * How far along the sequence each node is.
 *
 * Longest path from a node with nothing pointing at it, which is what puts a node after everything
 * that can reach it rather than after the first thing that does. A node reached by both `plan` and
 * a long rescue chain belongs at the end of the longer one; the shorter answer would draw an edge
 * running backwards.
 *
 * **A cycle does not hang and does not throw.** A workflow with a loop in it is a real thing — a
 * rescue that returns to the gate is the obvious one — so relaxation stops after one pass per node,
 * which is enough for any acyclic part and leaves a cycle's members where the last pass put them.
 * A layout that refused to draw a looping workflow would be refusing to draw the interesting ones.
 */
export function layerOf(nodes: GraphNode[], edges: GraphEdge[]): Record<string, number> {
  const layer: Record<string, number> = {};
  for (const node of nodes) layer[node.id] = 0;

  for (let pass = 0; pass < nodes.length; pass += 1) {
    let moved = false;
    for (const edge of edges) {
      const from = layer[edge.from];
      const to = layer[edge.to];
      if (from === undefined || to === undefined) continue;
      if (to < from + 1) {
        layer[edge.to] = from + 1;
        moved = true;
      }
    }
    if (!moved) break;
  }
  return layer;
}

/**
 * A position per node: along the sequence, then down.
 *
 * Ordered within a column by the order the bundle lists them, because that order is an authoring
 * decision — somebody wrote `plan` before `rescue` — and sorting by id would throw it away for
 * nothing.
 */
export function placed(nodes: GraphNode[], edges: GraphEdge[]): Record<string, Point> {
  const layer = layerOf(nodes, edges);
  const filled: Record<number, number> = {};
  const at: Record<string, Point> = {};

  for (const node of nodes) {
    const column = layer[node.id] ?? 0;
    const row = filled[column] ?? 0;
    filled[column] = row + 1;
    at[node.id] = { x: column * COLUMN, y: row * ROW };
  }
  return at;
}

/** The nodes in the order somebody reads them, which is also the order the miniature draws them. */
export function inSequence(nodes: GraphNode[], edges: GraphEdge[]): GraphNode[] {
  const layer = layerOf(nodes, edges);
  const order = new Map(nodes.map((node, index) => [node.id, index]));
  return [...nodes].sort(
    (a, b) =>
      (layer[a.id] ?? 0) - (layer[b.id] ?? 0) ||
      (order.get(a.id) ?? 0) - (order.get(b.id) ?? 0),
  );
}

/**
 * The tone a node is drawn in, and the two questions it answers without any text being read.
 *
 * §6.4: *where does this spend money* and *where can this break something*. An **agent** is violet
 * because it spends tokens; a **command** is grey because it does not; a **gate** is amber because
 * it is where the pipeline stops. A **decision** is blue — it executes nothing and costs nothing,
 * and it is the one place the path forks.
 *
 * A **fan** is grey like the command it usually contains, and is told apart by its shape rather
 * than its colour. That is deliberate: its question is *how many at once*, which is neither of the
 * two the colours answer, and a fifth colour would dilute the two that matter.
 */
export function nodeTone(kind: NodeKind, role: Role): string {
  if (role === "gate") return "pending";
  switch (kind) {
    case "agent":
      return "shadow";
    case "decision":
      return "info";
    default:
      return "off";
  }
}

/** What a node is, in words, for the label under it and for a screen reader. */
export function nodeMeaning(kind: NodeKind, role: Role): string {
  if (role === "gate") return "a command a branch depends on";
  switch (kind) {
    case "agent":
      return "a model is asked to do this";
    case "command":
      return "a program runs";
    case "decision":
      return "the path forks here";
    case "fan":
      return "one step becomes many";
  }
}

export interface WorkflowNodeData extends Record<string, unknown> {
  node: GraphNode;
  /** Lit, because this is where the run is. */
  running: boolean;
}

export type WorkflowFlowNode = Node<WorkflowNodeData, "workflowNode">;

export interface WorkflowEdgeData extends Record<string, unknown> {
  edge: GraphEdge;
  /** Either end switched off here, which makes the edge a path nothing takes in this project. */
  dimmed: boolean;
}

export type WorkflowFlowEdge = Edge<WorkflowEdgeData, "workflowEdge">;

export interface WorkflowModel {
  nodes: WorkflowFlowNode[];
  edges: WorkflowFlowEdge[];
}

/**
 * The whole picture, from the daemon's answer.
 *
 * `running` is a parameter and not a field of the graph, because **nothing feeds it yet**: which
 * node a live run is on is execution semantics, and §14 keeps those for the second spec. The
 * mechanism is built and tested here so that wiring it up is one prop rather than a redraw — and
 * saying that out loud is better than shipping a canvas that quietly never lights.
 */
export function buildWorkflow(
  nodes: GraphNode[],
  edges: GraphEdge[],
  running: string | null,
): WorkflowModel {
  const at = placed(nodes, edges);
  const disabled = new Set(nodes.filter((node) => node.disabled).map((node) => node.id));

  return {
    nodes: nodes.map((node) => ({
      id: node.id,
      type: "workflowNode" as const,
      position: at[node.id] ?? { x: 0, y: 0 },
      data: { node, running: running === node.id },
    })),
    edges: edges.map((edge, index) => ({
      // The index is in the key because two nodes can be joined twice — a gate's `pass` and `fail`
      // both landing on the same node is unusual but legal, and an id built from the ends alone
      // would silently draw one of them.
      id: `${edge.from}->${edge.to}#${index}`,
      source: edge.from,
      target: edge.to,
      type: "workflowEdge" as const,
      data: { edge, dimmed: disabled.has(edge.from) || disabled.has(edge.to) },
    })),
  };
}
