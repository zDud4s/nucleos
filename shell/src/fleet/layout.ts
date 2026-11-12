import { GROUP_THRESHOLD } from "./limits";
import type { FleetNode } from "./types";

/** Node sizes in flow pixels. 286 = 38 header + 216 body (12 x 17 + 12 padding) + 30 footer + 2 borders. */
export const NODE_W = 380;
export const NODE_H = 286;
export const CARD_H = 98;
export const GROUP_H = 64;
/** Gap between siblings, between a row and the next, between trees, around and between project bands. */
export const GAP_X = 36;
export const GAP_Y = 70;
export const TREE_GAP = 110;
export const BAND_PAD = 28;
export const BAND_HEAD = 26;
export const BAND_GAP = 90;
export const NO_PROJECT = "no project";
export const GROUP_PREFIX = "group:";

export type NodeSize = "terminal" | "card";
export interface LayoutOptions {
  size: NodeSize;
  /** Hide finished nodes, and trees with nothing alive in them. */
  onlyActive: boolean;
  /** Only this project's band; `null` is all of them. */
  project: string | null;
  /** Parents whose done group is open. */
  expanded: ReadonlySet<string>;
}
export interface GroupInfo {
  parentId: string;
  /** `present + omitted`: what "✓ N finished" says. */
  count: number;
  present: number;
  omitted: number;
  open: boolean;
}
export interface PlacedNode {
  id: string;
  kind: "agent" | "group";
  x: number;
  y: number;
  w: number;
  h: number;
  project: string;
  group: GroupInfo | null;
}
export interface PlacedEdge { id: string; source: string; target: string }
export interface Band { project: string; x: number; y: number; w: number; h: number }
export interface ForestLayout { nodes: PlacedNode[]; edges: PlacedEdge[]; bands: Band[] }

interface Tree {
  id: string;
  group: GroupInfo | null;
  kids: Tree[];
  /** Width of the whole subtree, and of its children laid side by side. */
  width: number;
  childrenWidth: number;
  depth: number;
}

const time = (iso: string): number => {
  const ms = Date.parse(iso);
  return Number.isNaN(ms) ? 0 : ms;
};
const byAge = (a: FleetNode, b: FleetNode): number =>
  time(a.started_at) - time(b.started_at) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);

/**
 * The forest as positions. Pure: nothing here knows React, xyflow or the DOM.
 *
 * Each tree grows downward; a subtree is as wide as its children side by side plus the gaps, the
 * parent is centered over them, and a leaf is one fixed-size node. Trees of a project sit side by
 * side by `started_at`; projects are bands stacked vertically (alphabetical, so a band never jumps
 * rows because its project's news changed). Input order does not matter.
 */
export function layoutForest(all: readonly FleetNode[], options: LayoutOptions): ForestLayout {
  const byId = new Map(all.map((n) => [n.id, n]));
  const kidsOf = new Map<string, FleetNode[]>();
  const roots: FleetNode[] = [];
  for (const n of all) {
    if (n.parent_id !== null && byId.has(n.parent_id)) {
      const list = kidsOf.get(n.parent_id) ?? [];
      list.push(n);
      kidsOf.set(n.parent_id, list);
    } else {
      roots.push(n);
    }
  }
  for (const list of kidsOf.values()) list.sort(byAge);

  const aliveMemo = new Map<string, boolean>();
  const hasLive = (n: FleetNode): boolean => {
    const known = aliveMemo.get(n.id);
    if (known !== undefined) return known;
    aliveMemo.set(n.id, false); // cycle guard
    const alive = n.state === "busy" || n.state === "idle" || (kidsOf.get(n.id) ?? []).some(hasLive);
    aliveMemo.set(n.id, alive);
    return alive;
  };

  const rowPitch = (options.size === "terminal" ? NODE_H : CARD_H) + GAP_Y;
  const nodeH = options.size === "terminal" ? NODE_H : CARD_H;

  function build(n: FleetNode): Tree {
    const visible = (kidsOf.get(n.id) ?? []).filter((k) => !options.onlyActive || hasLive(k));
    const groupable = new Set(visible.filter((k) => k.state === "done" && !hasLive(k)).map((k) => k.id));
    const count = options.onlyActive ? 0 : groupable.size + n.done_omitted;
    const fold = count > GROUP_THRESHOLD;
    const open = options.expanded.has(n.id);
    const kids: Tree[] = [];
    if (fold) {
      kids.push({
        id: `${GROUP_PREFIX}${n.id}`,
        group: { parentId: n.id, count, present: groupable.size, omitted: n.done_omitted, open },
        kids: [],
        width: NODE_W,
        childrenWidth: 0,
        depth: 1,
      });
    }
    for (const k of visible) {
      if (fold && !open && groupable.has(k.id)) continue;
      kids.push(build(k));
    }
    const childrenWidth = kids.reduce((sum, k) => sum + k.width, 0) + GAP_X * Math.max(0, kids.length - 1);
    return {
      id: n.id,
      group: null,
      kids,
      width: Math.max(NODE_W, childrenWidth),
      childrenWidth,
      depth: 1 + kids.reduce((max, k) => Math.max(max, k.depth), 0),
    };
  }

  const projectOf = (root: FleetNode): string => root.project ?? NO_PROJECT;
  const live = roots
    .filter((r) => (options.project === null || projectOf(r) === options.project))
    .filter((r) => !options.onlyActive || hasLive(r));
  const projects = [...new Set(live.map(projectOf))].sort((a, b) =>
    a === NO_PROJECT ? 1 : b === NO_PROJECT ? -1 : a < b ? -1 : a > b ? 1 : 0,
  );

  const out: ForestLayout = { nodes: [], edges: [], bands: [] };
  let cursor = 0;
  for (const project of projects) {
    const trees = live.filter((r) => projectOf(r) === project).sort(byAge).map((r) => build(r));
    let x = 0;
    let depth = 0;
    for (const tree of trees) {
      place(tree, null, x, cursor + BAND_HEAD, project);
      x += tree.width + TREE_GAP;
      depth = Math.max(depth, tree.depth);
    }
    const width = x - TREE_GAP;
    const h = BAND_HEAD + depth * rowPitch - GAP_Y + BAND_PAD;
    out.bands.push({ project, x: -BAND_PAD, y: cursor, w: width + 2 * BAND_PAD, h });
    cursor += h + BAND_GAP;
  }
  return out;

  function place(tree: Tree, parentId: string | null, x: number, y: number, project: string): void {
    out.nodes.push({
      id: tree.id,
      kind: tree.group === null ? "agent" : "group",
      x: x + (tree.width - NODE_W) / 2,
      y,
      w: NODE_W,
      h: tree.group === null ? nodeH : GROUP_H,
      project,
      group: tree.group,
    });
    if (parentId !== null) out.edges.push({ id: `e:${parentId}>${tree.id}`, source: parentId, target: tree.id });
    let cx = x + (tree.width - tree.childrenWidth) / 2;
    for (const kid of tree.kids) {
      place(kid, tree.id, cx, y + rowPitch, project);
      cx += kid.width + GAP_X;
    }
  }
}

/** The node and every ancestor up to its root (a group node counts as its parent's child). */
export function pathToRoot(parentOf: (id: string) => string | null | undefined, id: string): Set<string> {
  const path = new Set<string>();
  let current: string | null | undefined = id;
  while (current !== null && current !== undefined && !path.has(current)) {
    path.add(current);
    current = current.startsWith(GROUP_PREFIX) ? current.slice(GROUP_PREFIX.length) : parentOf(current);
  }
  return path;
}
