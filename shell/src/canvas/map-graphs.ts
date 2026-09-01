// §spec mapa-do-projeto

/**
 * The project's structure as two nested pictures, each drawn with the instrument its own density
 * earns.
 *
 * **The instrument is chosen by measurement and not by taste.** The núcleo is 99 files joined by
 * 580 dependencies — 5.9 a file. Node-link drawings stop being readable somewhere around 2.5, and
 * no layout algorithm rescues a graph past that: the first attempt at this screen drew it anyway
 * and 73% of its edges crossed a box they had nothing to do with. So the top level is a **matrix**,
 * which scales to hundreds of rows, and has the further property that a dependency pointing
 * backwards lands below the diagonal where it can be counted instead of admired.
 *
 * Inside a community the graph is sparse — that is what being a community means — and a layered
 * drawing is the better instrument. Where even that is too dense, {@link measure} says so and the
 * surface lists instead of drawing. **A picture nobody can read is worse than a sentence saying
 * why, because the picture still looks like an answer.**
 */

import type { MapImport, MapModule } from "../data/project-map";
import { communities, feedback, seriate } from "./communities";
import { type Layout, type Link, layout } from "./layered";

/** Above this many boxes a layered drawing stops being read and starts being scanned. */
export const MAX_BOXES = 44;
/** Links per box, past which no ordering saves the picture. Measured, not chosen. */
export const MAX_DENSITY = 2.6;
/** Crossings per link. One each is already a lot. */
export const MAX_CROSSINGS = 1;
/** Wider than this and the reader is scrolling sideways to follow one edge. */
export const MAX_WIDTH = 2800;

/** The name of a file without its folder or extension, which is what a box is labelled with. */
export function moduleName(path: string): string {
  const cut = path.lastIndexOf("/");
  const file = cut === -1 ? path : path.slice(cut + 1);
  const dot = file.lastIndexOf(".");
  return dot === -1 ? file : file.slice(0, dot);
}

/** Why a graph is not being drawn, in the reader's words. Empty means it is. */
export function reasonsNotToDraw(count: number, links: number, drawn: Layout): string[] {
  const bad: string[] = [];
  const density = links / Math.max(1, count);
  const ratio = drawn.crossings / Math.max(1, links);
  if (count > MAX_BOXES) bad.push(`${count} boxes, over the measured limit of ${MAX_BOXES}`);
  if (density > MAX_DENSITY) {
    bad.push(`${density.toFixed(1)} links a box, over ${MAX_DENSITY}`);
  }
  if (ratio > MAX_CROSSINGS) bad.push(`${ratio.toFixed(1)} crossings a link`);
  if (drawn.width > MAX_WIDTH) bad.push(`${Math.round(drawn.width)}px wide`);
  return bad;
}

export interface Drawing {
  drawn: Layout;
  /** Empty when the picture is worth showing; otherwise why it is not. */
  refused: string[];
  density: number;
}

/** Lay a graph out and judge it in one step, so the verdict is about the picture that exists. */
export function draw(
  names: string[],
  links: Link[],
  maxWidth?: number,
  labelOf: (id: string) => string = moduleName,
): Drawing {
  const bound = maxWidth ?? Math.max(4, Math.round(0.85 * Math.sqrt(names.length)));
  const drawn = layout(names, links, bound, labelOf);
  return {
    drawn,
    refused: reasonsNotToDraw(names.length, links.length, drawn),
    density: links.length / Math.max(1, names.length),
  };
}

export interface Community {
  /** The file the rest of the group leans on most, which is what it is called. */
  title: string;
  members: string[];
}

export interface CommunityMatrix {
  /** Communities in the order the rows are drawn — the one leaving least below the diagonal. */
  order: string[];
  members: Map<string, string[]>;
  /** How many imports cross from one community into another, keyed `from\0to`. */
  cells: Map<string, number>;
  /** Imports that point backwards under this ordering, and forwards. */
  back: number;
  forward: number;
  files: number;
  deps: number;
  /** Files with no dependency either way — they belong to no community and are not a box. */
  alone: string[];
}

export const cellKey = (from: string, to: string) => `${from}\0${to}`;

function linksBetween(imports: MapImport[], known: Set<string>): Link[] {
  const tally = new Map<string, Link>();
  for (const line of imports) {
    // Keyed by path and never by name: `core/src/presets.rs` and `shell/src/.../presets.ts` are
    // two files, and merging them into one box was the first thing the real answer showed.
    const from = line.from;
    const to = line.to;
    if (from === to || !known.has(from) || !known.has(to)) continue;
    const at = cellKey(from, to);
    const seen = tally.get(at);
    if (seen) seen.weight += 1;
    else tally.set(at, { from, to, weight: 1 });
  }
  return [...tally.values()];
}

/**
 * The whole project as communities and the traffic between them.
 *
 * **A community is named after the file the rest of it leans on most**, rather than by a number or
 * by whichever name sorts first: `map_join` is a name somebody recognises and `community 7` is not.
 * Where two groups would take the same name the later one is numbered, because a duplicate label on
 * two different rows is worse than an ugly one.
 *
 * **A file nothing imports and that imports nothing is not a box.** `main.rs` is only `mod`
 * declarations and `logging.rs` stands alone; both would sit in the matrix as a row and a column of
 * pure silence. They come back in {@link CommunityMatrix.alone} so a surface can list them, because
 * dropping them entirely is the quiet kind of wrong.
 */
export function buildCommunities(modules: MapModule[], imports: MapImport[]): CommunityMatrix {
  const named = modules.map((module) => module.path);
  const known = new Set(named);
  const links = linksBetween(imports, known);

  const linked = new Set<string>();
  for (const link of links) {
    linked.add(link.from);
    linked.add(link.to);
  }
  const names = named.filter((name) => linked.has(name)).sort();
  const alone = named.filter((name) => !linked.has(name)).sort();

  const found = communities(names, links);
  const members = new Map<string, string[]>();
  for (const name of names) {
    const where = found.get(name)!;
    (members.get(where) ?? members.set(where, []).get(where)!).push(name);
  }

  const pull = (group: string[]) => {
    const inside = new Set(group);
    const tally = new Map<string, number>();
    for (const link of links) {
      if (inside.has(link.from) && inside.has(link.to)) {
        tally.set(link.to, (tally.get(link.to) ?? 0) + link.weight);
      }
    }
    let best = [...group].sort()[0];
    let most = -1;
    for (const [name, weight] of tally) {
      if (weight > most) {
        most = weight;
        best = name;
      }
    }
    return best;
  };

  const titles = new Map<string, string>();
  const taken = new Map<string, number>();
  for (const [where, group] of [...members].sort((a, b) => b[1].length - a[1].length)) {
    // The file's name and not its path: a matrix row is a label, and `core/src/map_join.rs` sitting
    // sideways down a column is not one.
    const wanted = moduleName(pull(group));
    const seen = (taken.get(wanted) ?? 0) + 1;
    taken.set(wanted, seen);
    titles.set(where, seen === 1 ? wanted : `${wanted} (${seen})`);
  }

  const byTitle = new Map<string, string[]>();
  const homeOf = new Map<string, string>();
  for (const [where, group] of members) {
    const title = titles.get(where)!;
    byTitle.set(title, [...group].sort());
    for (const name of group) homeOf.set(name, title);
  }

  const cells = new Map<string, number>();
  const crossing: Link[] = [];
  for (const link of links) {
    const from = homeOf.get(link.from)!;
    const to = homeOf.get(link.to)!;
    if (from === to) continue;
    const at = cellKey(from, to);
    cells.set(at, (cells.get(at) ?? 0) + link.weight);
  }
  for (const [at, weight] of cells) {
    const [from, to] = at.split("\0");
    crossing.push({ from, to, weight });
  }

  const order = seriate([...byTitle.keys()], crossing);
  const { back, forward } = feedback(order, crossing);

  return {
    order,
    members: byTitle,
    cells,
    back,
    forward,
    files: names.length,
    deps: links.length,
    alone,
  };
}

/**
 * How many dependencies the map would actually draw.
 *
 * Not `imports.length`, and the difference is the point: an edge with an end this reader cannot
 * find is dropped rather than counted. The núcleo sends none of those today, and the day it does,
 * this number must not quietly start counting things nobody will ever see.
 */
export function drawableLinks(modules: MapModule[], imports: MapImport[]): number {
  return linksBetween(imports, new Set(modules.map((module) => module.path))).length;
}

/** The files inside one community, and what they import from each other. */
export function buildCommunity(
  members: string[],
  imports: MapImport[],
): Drawing & { links: Link[] } {
  const inside = new Set(members);
  const links = linksBetween(imports, inside);
  return { ...draw([...members].sort(), links, 5), links };
}

/** One community's traffic with another, in the direction it flows. */
export interface Traffic {
  title: string;
  /** Imports crossing, which is the same number the matrix cell holds. */
  weight: number;
}

/**
 * Which communities this one leans on, and which lean on it.
 *
 * **Navigation along the structure rather than along a list.** The rail answers
 * *what else is there*; this answers *what does this one actually touch*, which
 * is the question somebody standing inside a community has. They are different
 * questions and a reader with only the first has to guess.
 *
 * The two directions stay apart and are never summed. `council` using `job` and
 * `job` using `council` are opposite facts about a dependency, and one number
 * over the pair would say a community is "connected to" another while hiding
 * which way the arrow points — the same flattening the matrix exists to refuse
 * by putting one above the diagonal and the other below.
 *
 * Read off `cells`, which the matrix already computed. A second walk of the
 * imports could disagree with the picture drawn beside it.
 */
export function trafficFor(
  matrix: CommunityMatrix,
  title: string,
): { uses: Traffic[]; usedBy: Traffic[] } {
  const uses: Traffic[] = [];
  const usedBy: Traffic[] = [];
  for (const [at, weight] of matrix.cells) {
    const [from, to] = at.split("\0");
    if (from === title) uses.push({ title: to, weight });
    else if (to === title) usedBy.push({ title: from, weight });
  }
  const heaviest = (a: Traffic, b: Traffic) => b.weight - a.weight || a.title.localeCompare(b.title);
  return { uses: uses.sort(heaviest), usedBy: usedBy.sort(heaviest) };
}

/**
 * One file and everything in its community that touches it, drawn.
 *
 * **The answer to a refusal that is not another refusal.** A community too dense
 * to draw is told so honestly today, in the numbers that decided it — and then
 * the reader has nothing. The page this was measured against offers the way out
 * instead: *"escolhe um módulo e vês só ele e os vizinhos directos"*. A
 * neighbourhood is small by construction, so it draws where the whole does not,
 * and it is still the truth: every link shown is a link that exists.
 *
 * Direct neighbours only, in both directions. Two steps out is the density that
 * refused in the first place, arriving one ring later.
 */
export function sliceAround(
  members: string[],
  imports: MapImport[],
  centre: string,
): Drawing & { links: Link[]; members: string[] } {
  const links = linksBetween(imports, new Set(members));
  const near = new Set<string>([centre]);
  for (const link of links) {
    if (link.from === centre) near.add(link.to);
    if (link.to === centre) near.add(link.from);
  }
  const shown = [...near].sort();
  const between = links.filter((link) => near.has(link.from) && near.has(link.to));
  return { ...draw(shown, between, 5), links: between, members: shown };
}

/** How many files in the community touch this one, either way. */
export function neighbourCount(members: string[], imports: MapImport[], centre: string): number {
  return sliceAround(members, imports, centre).members.length - 1;
}
