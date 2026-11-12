// §spec motor-de-workflows
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
 * What a node is drawn as: one of five shapes, and never one of the seven tones.
 *
 * §6.4 asks the picture to answer *where does this spend money* and *where can this break
 * something*. This used to answer both in colour — agents violet, gates amber, decisions blue — and
 * every one of those hues is a STATE in this system: violet is shadow mode, amber is the only tone
 * that asks something of you, blue is a stated fact. A graph where every gate is permanently amber
 * teaches the eye to ignore the one colour that is meant to summon it, and a project in shadow mode
 * had violet meaning three things on one page. DESIGN.md: chroma reports state and nothing else.
 *
 * So the kind is carried by shape, face and an icon, on the neutral ladder: an agent is rounded, a
 * command is square and set in mono, a gate is a command with a heavier edge, a decision has a double
 * edge, a fan is dashed with a stack behind it. The role beats the kind — a gate is a role, so the
 * function says so rather than every caller remembering to.
 */
export type NodeShape = "agent" | "command" | "gate" | "decision" | "fan";

export function nodeShape(kind: NodeKind, role: Role): NodeShape {
  return role === "gate" ? "gate" : kind;
}

/** The order the key lists shapes in: the sequence somebody usually meets them in. */
export const SHAPES: readonly NodeShape[] = ["agent", "command", "gate", "decision", "fan"];

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
      // On the node object and not on the card inside it: xyflow puts keyboard focus on its own
      // wrapper (`role="group"`), and that wrapper reads `ariaLabel` from here. A name on the inner
      // card was a name on something focus never lands on, so a screen reader tabbing through the
      // graph heard "group" once per node and nothing else.
      ariaLabel: [
        `${node.label}, ${nodeMeaning(node.type, node.role)}`,
        node.disabled ? "off in this project" : null,
        node.overridden ? "changed by this project" : null,
        running === node.id ? "running" : null,
      ]
        .filter((part) => part !== null)
        .join(", "),
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
