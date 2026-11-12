// §spec mapa-do-projeto

/**
 * Finding the groups a codebase already has, and ordering them so the picture can be read.
 *
 * **Seven decompositions were tried against this repository before this one, and six failed.**
 * `impl` blocks group 6% of the items. Reachability from entry points leaves 35% belonging to one
 * of them. Dominator trees give either a single chapter holding 288 of 360 items or 142 chapters
 * for 148. Type affinity leaves 52% of functions touching no local type. Name prefixes put 82 of
 * 100 modules in one bucket. Label propagation collapsed 100 modules into a community of 95.
 *
 * None of those is a decomposition; each is a measurement of the same fact, which is that these
 * files are flat. Modularity is the seventh, and the first that found groups a person recognises —
 * the nine `map_*` files, the browser trio, `speak` with `transcribe` — without being told any of
 * them. It is preferred here not because it is better in general but because **when it fails it
 * fails on a number**: the resolution is raised until the biggest community is small enough to
 * draw, and if that never happens the caller can see it.
 */

import type { Link } from "./layered";

/** How large a community may be before the resolution is raised again. */
export const WANT_MAX = 14;

const RESOLUTIONS = [1, 1.5, 2, 3, 4, 6, 8, 11, 15, 20];

function weights(links: Link[]): Map<string, Map<string, number>> {
  const w = new Map<string, Map<string, number>>();
  const add = (a: string, b: string, by: number) => {
    const row = w.get(a) ?? new Map<string, number>();
    row.set(b, (row.get(b) ?? 0) + by);
    w.set(a, row);
  };
  for (const link of links) {
    if (link.from === link.to) continue;
    add(link.from, link.to, link.weight);
    add(link.to, link.from, link.weight);
  }
  return w;
}

/**
 * One local-moving pass of modularity optimisation: each node joins whichever neighbouring
 * community it most improves, until nobody wants to move.
 */
function onePass(names: string[], w: Map<string, Map<string, number>>, gamma: number) {
  const degree = new Map(names.map((n) => [n, [...(w.get(n)?.values() ?? [])].reduce((a, b) => a + b, 0)]));
  const twoM = [...degree.values()].reduce((a, b) => a + b, 0) || 1;
  const community = new Map(names.map((n) => [n, n]));
  const total = new Map(degree);
  const order = [...names].sort((a, b) => degree.get(b)! - degree.get(a)! || (a < b ? -1 : 1));

  for (let round = 0; round < 30; round += 1) {
    let moved = 0;
    for (const node of order) {
      const here = community.get(node)!;
      total.set(here, total.get(here)! - degree.get(node)!);
      const pull = new Map<string, number>();
      for (const [other, by] of w.get(node) ?? []) {
        const theirs = community.get(other)!;
        pull.set(theirs, (pull.get(theirs) ?? 0) + by);
      }
      let best = here;
      let gain = (pull.get(here) ?? 0) - (gamma * (total.get(here) ?? 0) * degree.get(node)!) / twoM;
      for (const [where, by] of pull) {
        const score = by - (gamma * (total.get(where) ?? 0) * degree.get(node)!) / twoM;
        if (score > gain + 1e-12) {
          best = where;
          gain = score;
        }
      }
      total.set(best, (total.get(best) ?? 0) + degree.get(node)!);
      if (best !== here) {
        community.set(node, best);
        moved += 1;
      }
    }
    if (moved === 0) break;
  }
  return community;
}

/**
 * Group the names by the links between them, raising the resolution until the largest group is
 * small enough to be a drawable picture.
 *
 * Returns which community each name landed in. A name with no links keeps itself, and the caller
 * decides whether that is a group of one or a file to list on its own.
 */
export function communities(names: string[], links: Link[], wantMax = WANT_MAX): Map<string, string> {
  const w = weights(links);
  let found = new Map(names.map((n) => [n, n]));
  for (const gamma of RESOLUTIONS) {
    found = onePass(names, w, gamma);
    const sizes = new Map<string, number>();
    for (const where of found.values()) sizes.set(where, (sizes.get(where) ?? 0) + 1);
    if (Math.max(...sizes.values()) <= wantMax) break;
  }
  return found;
}

/**
 * Order rows so that as little as possible ends up below the diagonal.
 *
 * **Ordering by depth in the graph was the obvious thing and it is not the right thing.** Measured
 * over the núcleo, depth put 146 of 372 dependencies below the diagonal where this puts 84. Sixty
 * of the arrows a reader saw pointing backwards were pointing backwards because of the sort, and
 * "how much of my architecture points the wrong way" is exactly the number somebody might act on.
 * What survives this ordering is coupling that no arrangement removes.
 *
 * Eades, Lin and Smyth: peel sinks off the back and sources off the front, and when neither exists
 * take whichever node has the most weight leaving it relative to what enters.
 */
export function seriate(names: string[], links: Link[]): string[] {
  const rest = new Set(names);
  const reaches = new Map<string, Set<string>>();
  const reachedBy = new Map<string, Set<string>>();
  const out = new Map<string, Map<string, number>>();
  for (const link of links) {
    if (link.from === link.to) continue;
    (reaches.get(link.from) ?? reaches.set(link.from, new Set()).get(link.from)!).add(link.to);
    (reachedBy.get(link.to) ?? reachedBy.set(link.to, new Set()).get(link.to)!).add(link.from);
    const row = out.get(link.from) ?? new Map<string, number>();
    row.set(link.to, (row.get(link.to) ?? 0) + link.weight);
    out.set(link.from, row);
  }
  const inside = (of: Map<string, Set<string>>, node: string) => {
    for (const other of of.get(node) ?? []) if (rest.has(other)) return true;
    return false;
  };

  const left: string[] = [];
  const right: string[] = [];
  while (rest.size > 0) {
    let moved = true;
    while (moved) {
      moved = false;
      for (const node of [...rest].sort()) {
        if (!inside(reaches, node)) {
          right.push(node);
          rest.delete(node);
          moved = true;
        }
      }
      for (const node of [...rest].sort()) {
        if (!inside(reachedBy, node)) {
          left.push(node);
          rest.delete(node);
          moved = true;
        }
      }
    }
    if (rest.size > 0) {
      const pull = (node: string) => {
        let leaving = 0;
        let entering = 0;
        for (const [other, by] of out.get(node) ?? []) if (rest.has(other)) leaving += by;
        for (const other of reachedBy.get(node) ?? []) {
          if (rest.has(other)) entering += out.get(other)?.get(node) ?? 0;
        }
        return leaving - entering;
      };
      const best = [...rest].sort().reduce((a, b) => (pull(b) > pull(a) ? b : a));
      left.push(best);
      rest.delete(best);
    }
  }
  return [...left, ...right.reverse()];
}

/** How many links point backwards under a given order, and how many point forward. */
export function feedback(order: string[], links: Link[]): { back: number; forward: number } {
  const at = new Map(order.map((name, index) => [name, index] as const));
  let back = 0;
  let forward = 0;
  for (const link of links) {
    const from = at.get(link.from);
    const to = at.get(link.to);
    if (from === undefined || to === undefined || from === to) continue;
    if (to < from) back += link.weight;
    else forward += link.weight;
  }
  return { back, forward };
}
