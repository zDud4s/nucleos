// §spec mapa-do-projeto

/**
 * One file's own declarations, laid out with the same instrument the level above it uses.
 *
 * **Nothing here is new machinery, and that is the result rather than the shortcut.** `layered.ts`
 * takes names and pairs and knows nothing about modules; it was written that way one slice ago
 * against the possibility of exactly this level, and the whole of the drawing below is
 * {@link draw} called with different names. A second layout engine for items would have been a
 * second set of crossing counts, a second set of thresholds and a second thing to keep true.
 *
 * **The instrument survives the descent because the measurement says it does.** The project as a
 * whole is 5.9 dependencies a file, which is why the top level is a matrix and not a graph. Inside
 * a file the same measurement comes back at **0.90 references an item at the median and 1.65 at
 * the 90th percentile**, and of this repository's 272 readable files **not one** is over the 2.6
 * that {@link reasonsNotToDraw} refuses at. The 29 that do refuse all refuse on size — `http.rs`
 * declares 385 things — and they refuse with their numbers showing.
 */

import type { FileItem, FileItems, ItemReference } from "../data/project-map";
import type { Link } from "./layered";
import { type Drawing, draw } from "./map-graphs";

/**
 * What a box is called: the bare name, unless this file gives that name to more than one thing.
 *
 * **The qualified id is the fallback and never the default.** `open` is what the reader came for
 * and `Door::open` is what the parser needed; showing the second everywhere would spend the width
 * of every box on a container that is usually the same for its whole neighbourhood. But a file with
 * `A::new` and `B::new` in it has two different functions called `new`, and two boxes labelled the
 * same is the quiet kind of wrong this map exists to catch — so where the short name would lie,
 * the long one is used.
 */
export function labelFor(items: FileItem[]): (id: string) => string {
  const seen = new Map<string, number>();
  for (const item of items) seen.set(item.name, (seen.get(item.name) ?? 0) + 1);
  const label = new Map<string, string>();
  for (const item of items) {
    label.set(item.id, (seen.get(item.name) ?? 0) > 1 ? item.id : item.name);
  }
  return (id) => label.get(id) ?? id;
}

/**
 * The references this file's own boxes can carry.
 *
 * An edge with an end that is not a box is dropped rather than drawn to nowhere — the same rule
 * `drawableLinks` keeps one level up, and for the same reason: the núcleo sends none of those
 * today, and the day it does, this must not start counting things nobody can see.
 */
export function itemLinks(items: FileItem[], references: ItemReference[]): Link[] {
  const known = new Set(items.map((item) => item.id));
  const tally = new Map<string, Link>();
  for (const edge of references) {
    if (edge.from === edge.to || !known.has(edge.from) || !known.has(edge.to)) continue;
    const at = `${edge.from} ${edge.to}`;
    const seen = tally.get(at);
    if (seen) seen.weight += 1;
    else tally.set(at, { from: edge.from, to: edge.to, weight: 1 });
  }
  return [...tally.values()];
}

/** What this file says about itself, in the two numbers the owner's question rests on. */
export interface FileFacts {
  items: number;
  /** Reachable from outside the file. */
  exported: number;
  /** Carrying a doc comment the language itself would print. */
  documented: number;
  /** Declarations nothing else in this file uses, and that nothing outside it can reach. */
  unreachable: number;
}

/**
 * The facts a drawing cannot show, counted so they can be written beside it.
 *
 * **`unreachable` is the one worth having and the one that needed care.** A declaration nothing in
 * this file calls and nothing outside it can reach is dead by both routes — and it is the closest
 * this level gets to answering *está como eu queria?* without asking anybody. It is deliberately
 * not called *dead*: a `#[test]` helper, a trait method invoked through the trait, and a React
 * component named only by a route are all reachable in ways this reader cannot see. The word says
 * what was measured — nothing here reaches it — and leaves the verdict to whoever is looking.
 */
export function fileFacts(found: FileItems): FileFacts {
  const reached = new Set(found.references.map((edge) => edge.to));
  return {
    items: found.items.length,
    exported: found.items.filter((item) => item.exported).length,
    documented: found.items.filter((item) => item.documented).length,
    unreachable: found.items.filter((item) => !item.exported && !reached.has(item.id)).length,
  };
}

/** One file's declarations, laid out and judged in the same step. */
export function buildFileItems(found: FileItems): Drawing & { links: Link[] } {
  const links = itemLinks(found.items, found.references);
  const names = found.items.map((item) => item.id);
  return { ...draw(names, links, undefined, labelFor(found.items)), links };
}
