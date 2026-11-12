import { ANY_STATE, COMPUTED_STATE, type StateMachine } from "../data/project-states";

export interface StateNode {
  id: string;
  label: string;
  column: number;
  row: number;
}

export interface StateEdge {
  from: string;
  to: string;
  /** Unique, sorted; "top level" stands for a statement with no enclosing function. */
  functions: string[];
  anchors: { path: string; line: number; function: string | null }[];
}

export interface MachineLayout {
  nodes: StateNode[];
  edges: StateEdge[];
  columns: number;
  rows: number;
}

function labelOf(id: string): string {
  if (id === ANY_STATE) return "any";
  if (id === COMPUTED_STATE) return "computed";
  return id;
}

/**
 * One machine as columns and rows: a pure function, so the drawing holds no layout logic.
 *
 * Columns are the shortest distance from a source (a state nothing enters, and always the
 * any-state); rows are alphabetical inside a column, so the same machine always draws the same.
 */
export function layoutMachine(machine: StateMachine): MachineLayout {
  const ids = new Set<string>();
  const grouped = new Map<string, StateEdge>();
  for (const t of machine.transitions) {
    ids.add(t.from);
    ids.add(t.to);
    const key = `${t.from}\u0000${t.to}`;
    let edge = grouped.get(key);
    if (edge === undefined) {
      edge = { from: t.from, to: t.to, functions: [], anchors: [] };
      grouped.set(key, edge);
    }
    const name = t.function ?? "top level";
    if (!edge.functions.includes(name)) edge.functions.push(name);
    edge.anchors.push({ path: t.path, line: t.line, function: t.function });
  }
  const edges = [...grouped.values()].sort(
    (a, b) => a.from.localeCompare(b.from) || a.to.localeCompare(b.to),
  );
  for (const edge of edges) edge.functions.sort();

  const sorted = [...ids].sort();
  const entered = new Set(edges.filter((e) => e.from !== e.to).map((e) => e.to));
  const roots = sorted.filter((id) => id === ANY_STATE || !entered.has(id));
  if (roots.length === 0 && sorted.length > 0) roots.push(sorted[0]);

  const depth = new Map<string, number>();
  const queue: string[] = [];
  for (const root of roots) {
    depth.set(root, 0);
    queue.push(root);
  }
  for (let head = 0; head < queue.length; head += 1) {
    const current = queue[head];
    const next = (depth.get(current) ?? 0) + 1;
    for (const edge of edges) {
      if (edge.from !== current || depth.has(edge.to)) continue;
      depth.set(edge.to, next);
      queue.push(edge.to);
    }
  }

  const perColumn = new Map<number, string[]>();
  for (const id of sorted) {
    const column = depth.get(id) ?? 0;
    perColumn.set(column, [...(perColumn.get(column) ?? []), id]);
  }
  const nodes: StateNode[] = [];
  let rows = 0;
  let columns = 0;
  for (const [column, members] of perColumn) {
    members.sort();
    members.forEach((id, row) => nodes.push({ id, label: labelOf(id), column, row }));
    rows = Math.max(rows, members.length);
    columns = Math.max(columns, column + 1);
  }
  nodes.sort((a, b) => a.column - b.column || a.row - b.row);
  return { nodes, edges, columns, rows };
}
