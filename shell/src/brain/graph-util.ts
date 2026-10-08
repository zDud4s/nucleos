import type { GModel, GNode } from "./graph-types";

/** The node's radius on screen at zoom 1: grows with its links, never past 16. */
export function nodeRadius(degree: number): number {
  return Math.min(16, 4 + 2 * Math.sqrt(Math.max(0, degree)));
}

/** The CSS custom property NAME that colours this node; the canvas reads its value, never writes style. */
export function colorToken(node: GNode): string {
  if (node.kind === "knowledge") return `--brain-k-${node.layer ?? "semantic"}`;
  return `--brain-${node.kind}`;
}

/** `id` and every node within `depth` edges of it, edges taken in both directions. */
export function neighbours(model: GModel, id: string, depth: 1 | 2): Set<string> {
  const adjacent = new Map<string, string[]>();
  for (const e of model.edges) {
    (adjacent.get(e.source) ?? adjacent.set(e.source, []).get(e.source)!).push(e.target);
    (adjacent.get(e.target) ?? adjacent.set(e.target, []).get(e.target)!).push(e.source);
  }
  const seen = new Set<string>([id]);
  let frontier = [id];
  for (let step = 0; step < depth; step++) {
    const next: string[] = [];
    for (const n of frontier) {
      for (const m of adjacent.get(n) ?? []) {
        if (!seen.has(m)) {
          seen.add(m);
          next.push(m);
        }
      }
    }
    frontier = next;
  }
  return seen;
}
