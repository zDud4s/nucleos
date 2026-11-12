// §spec mapa-do-projeto
import type { MapImport, MapModule, Reader, Seam } from "../data/project-map";

/**
 * The project as an architecture drawing: sides as columns, folders as blocks, and the blocks of
 * a side stacked so that what a folder uses sits below it.
 *
 * **Folders and never communities.** The old picture clustered files by their imports and named
 * each cluster after its busiest file — 64 groups the owner had never seen, laid out as a matrix.
 * A folder is a grouping somebody already chose and already knows by name, which is the whole
 * point of a map: you recognise the ground before you read anything off it. The one exception is
 * a folder too big to be a block at all (`core/src`, flat), which is cut along its name prefixes
 * and its imports — see `communities`.
 */

/** One folder's worth of readable modules. */
export interface Block {
  /** Stable id: the side and the folder, `shell/pages`. */
  key: string;
  side: string;
  /** What the owner reads on the block: `pages`, `crates/nucleos-base`, or the side's own `src`. */
  label: string;
  files: MapModule[];
  tested: number;
  /** Files naming at least one `§`. */
  citing: number;
  /** 0 = nothing on this side uses it; higher rows are used by the rows above. */
  layer: number;
}

export interface Column {
  /** `shell · typescript`: a folder and the reader that understood it. */
  key: string;
  folder: string;
  /**
   * What the side is called on screen: its folder, or — when two readers share one folder, as the
   * shell's TypeScript and its Tauri host's Rust do — the folder the smaller side actually lives in.
   */
  name: string;
  reader: Reader;
  files: number;
  /** Blocks by layer, top (entry points) to bottom (foundations). */
  layers: Block[][];
}

/** A pile of files nothing here could read, by top folder and kind. */
export interface Unread {
  folder: string;
  files: number;
  /** The most frequent extensions, most first. */
  kinds: string[];
}

export interface Atlas {
  columns: Column[];
  blocks: Map<string, Block>;
  /** Imports between two different blocks, keyed by both block keys, counted. */
  edges: Map<string, number>;
  /** Source the map cannot follow — Go, Python, SQL, scripts — by folder. */
  unread: Unread[];
  /** Files in {@link unread}: build output is left out of both. */
  unreadFiles: number;
  /** Files that are not code at all — images, fonts, docs, styles, data — counted, not listed. */
  notCode: number;
  /** The most frequent extensions among {@link notCode}, most first. */
  notCodeKinds: string[];
  /** The block a module belongs to, by path. */
  blockOf: Map<string, string>;
}

/** Block keys hold NULs of their own, so an edge joins two of them with a different separator. */
const EDGE = "\u0001";
const edgeKey = (from: string, to: string) => `${from}${EDGE}${to}`;

/** A folder of packages, where the package and not the folder is what an owner names. */
const PACKAGE_FOLDERS = new Set(["crates", "packages", "apps", "libs"]);

/**
 * The block a path belongs to: its folders with every `src` dropped, cut one below the side.
 *
 * `src` says nothing anyone navigates by, and cutting there folds a `ui/vendor` into `ui` and a
 * `brain/knowledge` into `brain` — the level an owner names when they talk about their code. Under
 * `crates/` or `packages/` the cut is one deeper, at the package.
 */
export function blockKey(path: string): string {
  const parts = path.split("/");
  parts.pop();
  const kept = parts.filter((part) => part !== "src");
  const depth = PACKAGE_FOLDERS.has(kept[1] ?? "") ? 3 : 2;
  return kept.slice(0, depth).join("/") || ".";
}

/**
 * The path a module is grouped by: its own, except that a Rust `verify.rs` beside a `verify/`
 * folder is that folder's parent module (`verify/batch.rs` is `crate::verify::batch`, declared in
 * `verify.rs`) and belongs in the folder's block, not alone at the root above it.
 */
export function groupingPath(path: string, folders: Set<string>): string {
  if (!path.endsWith(".rs")) return path;
  const own = path.slice(0, -".rs".length);
  return folders.has(own) ? `${own}/${own.split("/").pop()}.rs` : path;
}

/** Every folder that holds a module, at any depth. */
function foldersOf(modules: MapModule[]): Set<string> {
  const folders = new Set<string>();
  for (const module of modules) {
    const parts = module.path.split("/");
    for (let depth = 1; depth < parts.length; depth++) folders.add(parts.slice(0, depth).join("/"));
  }
  return folders;
}

function labelFor(key: string, side: string, files: MapModule[]): string {
  const rest = key === side ? "" : key.slice(side.length + 1);
  if (rest !== "") return rest;
  // The side's own root: name it after the folder most of its files really sit in.
  const tally = new Map<string, number>();
  for (const file of files) {
    const dir = file.path.split("/").slice(1, -1).join("/") || side;
    tally.set(dir, (tally.get(dir) ?? 0) + 1);
  }
  const dir = [...tally.entries()].sort((a, b) => b[1] - a[1])[0]?.[0];
  return dir === undefined || dir === side ? side : `${side}/${dir}`;
}

/**
 * Rows for a side: entry points on top, the folders everything leans on at the bottom.
 *
 * Folders in a real project use each other both ways — `pages` imports `data` a hundred times and
 * `data` reaches back once — and a plain walk cuts such a cycle wherever it happens to close it,
 * which stacked seventeen folders eleven rows deep. So a two-way pair keeps only its heavier
 * direction, what still loops after that is one strongly connected group sharing a row, and a
 * block's row is counted up from the bottom: its distance to the deepest thing it leans on.
 */
function layer(keys: string[], edges: Map<string, number>): Map<string, number> {
  const known = new Set(keys);
  const out = new Map<string, string[]>(keys.map((key) => [key, []]));
  for (const [at, weight] of edges) {
    const [from, to] = at.split(EDGE);
    if (!known.has(from) || !known.has(to)) continue;
    const back = edges.get(edgeKey(to, from)) ?? 0;
    if (back >= weight) continue;
    out.get(from)!.push(to);
  }

  // Tarjan: what still loops is one group, and one group is one row.
  const index = new Map<string, number>();
  const low = new Map<string, number>();
  const stack: string[] = [];
  const onStack = new Set<string>();
  const group = new Map<string, number>();
  let counter = 0;
  let groups = 0;
  const connect = (key: string) => {
    index.set(key, counter);
    low.set(key, counter);
    counter += 1;
    stack.push(key);
    onStack.add(key);
    for (const next of out.get(key)!) {
      if (!index.has(next)) {
        connect(next);
        low.set(key, Math.min(low.get(key)!, low.get(next)!));
      } else if (onStack.has(next)) {
        low.set(key, Math.min(low.get(key)!, index.get(next)!));
      }
    }
    if (low.get(key) === index.get(key)) {
      let member: string;
      do {
        member = stack.pop()!;
        onStack.delete(member);
        group.set(member, groups);
      } while (member !== key);
      groups += 1;
    }
  };
  for (const key of keys) if (!index.has(key)) connect(key);

  const below = new Map<number, Set<number>>();
  for (const [from, targets] of out) {
    for (const to of targets) {
      const [a, b] = [group.get(from)!, group.get(to)!];
      if (a !== b) below.set(a, (below.get(a) ?? new Set()).add(b));
    }
  }
  const height = new Map<number, number>();
  const heightOf = (g: number): number => {
    const known = height.get(g);
    if (known !== undefined) return known;
    let h = 0;
    for (const next of below.get(g) ?? []) h = Math.max(h, heightOf(next) + 1);
    height.set(g, h);
    return h;
  };
  const top = Math.max(0, ...keys.map((key) => heightOf(group.get(key)!)));
  return new Map(keys.map((key) => [key, top - heightOf(group.get(key)!)]));
}

/**
 * Folders a build or a tool writes into. They are not the project's source, and listing them as
 * what the map "could not read" made the pile look like debt when it was only output.
 */
const BUILD_OUTPUT =
  /^(dist.*|preview-shots|target|node_modules|\.cargo-target.*|coverage|__pycache__)$/;

/**
 * Extensions that are source code in a language the map does not read. Everything else the núcleo
 * reports as unread — images, fonts, Markdown, CSS, JSON, lockfiles — was never code, and counting
 * it beside the Go sidecars made a pile of 650 where the real blind spot was a third of that.
 */
const SOURCE = new Set([
  "go",
  "py",
  "sql",
  "sh",
  "bash",
  "ps1",
  "js",
  "mjs",
  "cjs",
  "jsx",
  "c",
  "h",
  "cc",
  "cpp",
  "hpp",
  "cs",
  "java",
  "kt",
  "swift",
  "rb",
  "php",
  "lua",
  "zig",
  "dart",
  "ex",
  "exs",
  "scala",
  "vue",
  "svelte",
]);

/** The deepest folder every one of these files sits in, at most two levels down. */
function sharedFolder(files: MapModule[]): string {
  const dirs = files.map((file) => file.path.split("/").slice(0, -1));
  const common: string[] = [];
  for (let at = 0; at < 2; at += 1) {
    const part = dirs[0]?.[at];
    if (part === undefined || dirs.some((dir) => dir[at] !== part)) break;
    common.push(part);
  }
  return common.join("/");
}

/** A folder holding this many readable files is split by its imports instead of drawn as one. */
const SPLIT_AT = 40;
/** A file that uses at least this share of its folder is a router and stands alone. */
const ROUTER_SHARE = 0.45;
/** A file used by at least this share of its folder, using little, is a foundation. */
const FOUNDATION_SHARE = 0.3;
/** A name family with this many files keeps its own block. */
const ANCHOR_AT = 5;

/** What a file is called in a label: its stem, or its folder's name for a `mod.rs` / `index.ts`. */
function fileName(path: string): string {
  const parts = path.split("/");
  const stem = parts[parts.length - 1].replace(/\.[^.]+$/, "");
  return stem === "mod" || stem === "index" ? (parts[parts.length - 2] ?? stem) : stem;
}

/**
 * Cuts a flat folder into the groups its own imports draw.
 *
 * `core/src` holds 146 files side by side, and as one block it hid 988 imports inside itself —
 * the core read as a single box beside a shell of fifteen. Two rules keep the cut recognisable:
 * files sharing a name prefix (`map_seam`, `map_stamp`, `verify_plan`) are the owner's own grouping
 * and never part, and a big family (`devtime_*`, `verify_*`, `map_*`) keeps a block of its own;
 * the rest go wherever modularity puts them (Louvain's local moving, nodes in name order, so the
 * same tree always cuts the same way), with routers and foundations held out first. Each group is
 * named after its two largest members, so a label is words the owner wrote, never a cluster id.
 */
function communities(
  files: MapModule[],
  imports: MapImport[],
): { label: string; files: MapModule[] }[] {
  const head = (path: string) => fileName(path).split("_")[0];
  const heads = new Map<string, number>();
  for (const file of files) heads.set(head(file.path), (heads.get(head(file.path)) ?? 0) + 1);
  const unitOf = new Map<string, string>();
  const nameOf = new Map<string, string>();
  for (const file of files) {
    const family = (heads.get(head(file.path)) ?? 0) > 1;
    const unit = family ? `\0${head(file.path)}` : file.path;
    unitOf.set(file.path, unit);
    nameOf.set(unit, family ? head(file.path) : fileName(file.path));
  }

  const adjacent = new Map<string, Map<string, number>>();
  const uses = new Map<string, Set<string>>();
  const usedBy = new Map<string, Set<string>>();
  const link = (a: string, b: string) => {
    const row = adjacent.get(a) ?? new Map<string, number>();
    row.set(b, (row.get(b) ?? 0) + 1);
    adjacent.set(a, row);
  };
  for (const line of imports) {
    const a = unitOf.get(line.from);
    const b = unitOf.get(line.to);
    if (a === undefined || b === undefined || a === b) continue;
    uses.set(a, (uses.get(a) ?? new Set()).add(b));
    usedBy.set(b, (usedBy.get(b) ?? new Set()).add(a));
    link(a, b);
    link(b, a);
  }

  const units = [...new Set(unitOf.values())].sort();
  const reach = (of: Map<string, Set<string>>, unit: string) => of.get(unit)?.size ?? 0;
  /*
    Two kinds of file belong to no neighbourhood, and left in they drag one along with them. A
    router uses almost everything (`http.rs` reaches 71 of 91 units), so modularity drops it
    beside whatever it happened to meet first. A foundation is used by almost everything and uses
    little (`config.rs`, `state.rs`), so it lands in whichever group leaned on it hardest. Routers
    stand alone; foundations share one block, which the layering then puts at the bottom.
  */
  const router = (unit: string) => reach(uses, unit) >= ROUTER_SHARE * units.length;
  const foundation = (unit: string) =>
    !router(unit) &&
    reach(usedBy, unit) >= FOUNDATION_SHARE * units.length &&
    reach(uses, unit) < reach(usedBy, unit) / 2;
  const FOUNDATIONS = "\0\0foundations";
  // A family this big is a module in all but folder: it takes neighbours, it never joins one.
  const anchor = (unit: string) =>
    files.filter((file) => unitOf.get(file.path) === unit).length >= ANCHOR_AT;
  const moving = units.filter((unit) => !router(unit) && !foundation(unit));
  const inPlay = new Set(moving);

  const degree = new Map<string, number>();
  for (const unit of moving) {
    let k = 0;
    for (const [other, w] of adjacent.get(unit) ?? []) if (inPlay.has(other)) k += w;
    degree.set(unit, k);
  }
  const twiceTotal = [...degree.values()].reduce((sum, k) => sum + k, 0) || 1;
  const community = new Map(
    units.map((unit) => [unit, foundation(unit) ? FOUNDATIONS : unit] as [string, string]),
  );
  const total = new Map(degree);
  for (let moved = true, rounds = 0; moved && rounds < 50; rounds += 1) {
    moved = false;
    for (const unit of moving) {
      if (anchor(unit)) continue;
      const k = degree.get(unit)!;
      const was = community.get(unit)!;
      total.set(was, total.get(was)! - k);
      const towards = new Map<string, number>();
      for (const [other, w] of adjacent.get(unit) ?? []) {
        if (!inPlay.has(other)) continue;
        const c = community.get(other)!;
        towards.set(c, (towards.get(c) ?? 0) + w);
      }
      const gain = (c: string) => (towards.get(c) ?? 0) - (k * (total.get(c) ?? 0)) / twiceTotal;
      let best = was;
      for (const c of [...towards.keys()].sort()) if (gain(c) > gain(best) + 1e-9) best = c;
      community.set(unit, best);
      total.set(best, (total.get(best) ?? 0) + k);
      if (best !== was) moved = true;
    }
  }
  const rank = (unit: string) => reach(uses, unit) + reach(usedBy, unit);

  const members = new Map<string, string[]>();
  for (const unit of units) {
    const c = community.get(unit)!;
    members.set(c, [...(members.get(c) ?? []), unit]);
  }
  const size = (unit: string) => files.filter((file) => unitOf.get(file.path) === unit).length;
  const taken = new Set<string>();
  return [...members.values()].map((group) => {
    const named = group
      .sort((a, b) => size(b) - size(a) || rank(b) - rank(a) || a.localeCompare(b))
      .slice(0, 2)
      .map((unit) => nameOf.get(unit)!);
    let label = named.join(" · ");
    while (taken.has(label)) label += "'";
    taken.add(label);
    return { label, files: files.filter((file) => group.includes(unitOf.get(file.path)!)) };
  });
}

export function buildAtlas(modules: MapModule[], imports: MapImport[], unread: string[]): Atlas {
  const grouped = new Map<string, MapModule[]>();
  const blockOf = new Map<string, string>();
  const folders = foldersOf(modules);
  for (const module of modules) {
    const side = `${module.path.split("/")[0]}\0${module.reader}`;
    const key = `${side}\0${blockKey(groupingPath(module.path, folders))}`;
    blockOf.set(module.path, key);
    grouped.set(key, [...(grouped.get(key) ?? []), module]);
  }

  // A folder too big to be one block is cut where its own imports cut it — see `communities`.
  // Only a side's own root: `shell/data` at 55 files is a folder somebody named, and stays one.
  for (const [key, files] of [...grouped]) {
    const [folder, , path] = key.split("\0");
    if (path !== folder || files.length < SPLIT_AT) continue;
    const parts = communities(files, imports);
    if (parts.length < 2) continue;
    grouped.delete(key);
    for (const part of parts) {
      const partKey = `${key}/${part.label}`;
      grouped.set(partKey, part.files);
      for (const file of part.files) blockOf.set(file.path, partKey);
    }
  }

  const edges = new Map<string, number>();
  for (const line of imports) {
    const from = blockOf.get(line.from);
    const to = blockOf.get(line.to);
    if (from === undefined || to === undefined || from === to) continue;
    edges.set(edgeKey(from, to), (edges.get(edgeKey(from, to)) ?? 0) + 1);
  }

  const blocks = new Map<string, Block>();
  const bySide = new Map<string, Block[]>();
  for (const [key, files] of grouped) {
    const [folder, reader, path] = key.split("\0");
    const sideKey = `${folder}\0${reader}`;
    const block: Block = {
      key,
      side: sideKey,
      label: labelFor(path, folder, files),
      files: [...files].sort((a, b) => a.path.localeCompare(b.path)),
      tested: files.filter((file) => file.tested).length,
      citing: files.filter((file) => file.cites.length > 0).length,
      layer: 0,
    };
    blocks.set(key, block);
    bySide.set(sideKey, [...(bySide.get(sideKey) ?? []), block]);
  }

  const columns: Column[] = [];
  for (const [sideKey, members] of bySide) {
    const depth = layer(
      members.map((block) => block.key),
      edges,
    );
    const layers: Block[][] = [];
    for (const block of members) {
      block.layer = depth.get(block.key) ?? 0;
      (layers[block.layer] ??= []).push(block);
    }
    for (const row of layers) row?.sort((a, b) => b.files.length - a.files.length);
    const [folder, reader] = sideKey.split("\0");
    columns.push({
      key: sideKey,
      folder,
      name: folder,
      reader: reader as Reader,
      files: members.reduce((sum, block) => sum + block.files.length, 0),
      layers: layers.filter((row) => row !== undefined),
    });
  }
  columns.sort((a, b) => b.files - a.files);
  for (const column of columns) {
    if (columns.some((other) => other !== column && other.folder === column.folder)) {
      column.name = sharedFolder(column.layers.flat().flatMap((block) => block.files));
    }
  }

  const piles = new Map<string, { files: number; kinds: Map<string, number> }>();
  const other = new Map<string, number>();
  for (const path of unread) {
    const parts = path.split("/");
    if (parts.some((part) => BUILD_OUTPUT.test(part))) continue;
    const folder = parts.length > 2 ? parts.slice(0, 2).join("/") : parts[0];
    const name = parts[parts.length - 1];
    const dot = name.lastIndexOf(".");
    const kind = dot > 0 ? name.slice(dot + 1).toLowerCase() : "other";
    if (!SOURCE.has(kind)) {
      other.set(kind, (other.get(kind) ?? 0) + 1);
      continue;
    }
    const pile = piles.get(folder) ?? { files: 0, kinds: new Map() };
    pile.files += 1;
    pile.kinds.set(kind, (pile.kinds.get(kind) ?? 0) + 1);
    piles.set(folder, pile);
  }
  const unreadList = [...piles.entries()]
    .map(([folder, pile]) => ({
      folder,
      files: pile.files,
      kinds: [...pile.kinds.entries()].sort((a, b) => b[1] - a[1]).map(([kind]) => kind),
    }))
    .sort((a, b) => b.files - a.files);

  const unreadFiles = unreadList.reduce((sum, pile) => sum + pile.files, 0);
  const notCode = [...other.values()].reduce((sum, n) => sum + n, 0);
  const notCodeKinds = [...other.entries()].sort((a, b) => b[1] - a[1]).map(([kind]) => kind);
  return {
    columns,
    blocks,
    edges,
    unread: unreadList,
    unreadFiles,
    notCode,
    notCodeKinds,
    blockOf,
  };
}

/** What one block uses and what uses it, each with the import count, busiest first. */
export function neighbours(
  atlas: Atlas,
  key: string,
): { uses: [Block, number][]; usedBy: [Block, number][] } {
  const uses: [Block, number][] = [];
  const usedBy: [Block, number][] = [];
  for (const [at, count] of atlas.edges) {
    const [from, to] = at.split(EDGE);
    if (from === key) uses.push([atlas.blocks.get(to)!, count]);
    if (to === key) usedBy.push([atlas.blocks.get(from)!, count]);
  }
  uses.sort((a, b) => b[1] - a[1]);
  usedBy.sort((a, b) => b[1] - a[1]);
  return { uses, usedBy };
}

/** The boundary between sides, in the three numbers an owner reads it by. */
export interface SeamSummary {
  routes: number;
  calls: number;
  /** Calls whose route nobody serves — the one finding the seam can make. */
  broken: number;
  /** Calls the scan could not finish reading. */
  unsure: number;
}

export function seamSummary(seam: Seam | undefined): SeamSummary | null {
  if (seam === undefined || (seam.served.length === 0 && seam.calls === 0)) return null;
  return {
    routes: seam.served.length,
    calls: seam.calls,
    broken: seam.unmatched.length,
    unsure: seam.computed.length + seam.opaque.length,
  };
}

/* ------------------------------------------------------------ the graph -- */

/** One folder placed on its side's drawing. */
export interface GraphNode {
  block: Block;
  x: number;
  y: number;
  w: number;
  h: number;
}

/** One import line between two folders of a side, with the curve that draws it. */
export interface GraphLink {
  from: string;
  to: string;
  count: number;
  path: string;
  /** Drawn against the grain — a lighter direction of a pair, or a cycle the rows had to cut. */
  up: boolean;
}

export interface GraphDrawing {
  width: number;
  height: number;
  nodes: GraphNode[];
  links: GraphLink[];
}

const NODE_H = 46;
const ROW_GAP = 56;
const NODE_GAP = 14;
const PAD = 12;
/** A layer with more blocks than this wraps onto stacked lines, a tighter gap apart. */
const ROW_MAX = 6;
const WRAP_GAP = 24;

/** Wide enough for the label in a 13px face and the count under it. */
function nodeWidth(block: Block): number {
  return Math.max(84, Math.min(200, block.label.length * 7.4 + 28));
}

/**
 * The layered drawing of one side: rows from {@link Column.layers}, reordered so lines cross as
 * little as a few sweeps can manage, then every import between two of its folders as a curve.
 *
 * Barycentre sweeps, down and back up: each folder moves towards the average place of the folders
 * it is joined to in the row before. Cheap, deterministic, and on twenty folders indistinguishable
 * from anything cleverer.
 */
export function drawColumn(column: Column, edges: Map<string, number>): GraphDrawing {
  const rows = column.layers.map((row) => [...row]);
  const keys = new Set(rows.flat().map((block) => block.key));
  const joined = new Map<string, Set<string>>();
  for (const at of edges.keys()) {
    const [from, to] = at.split(EDGE);
    if (!keys.has(from) || !keys.has(to)) continue;
    joined.set(from, (joined.get(from) ?? new Set()).add(to));
    joined.set(to, (joined.get(to) ?? new Set()).add(from));
  }
  const slot = new Map<string, number>();
  const number = () => rows.forEach((row) => row.forEach((block, i) => slot.set(block.key, i)));
  number();
  const sweep = (row: Block[], against: Block[]) => {
    const near = new Set(against.map((block) => block.key));
    const centre = (block: Block) => {
      const seen = [...(joined.get(block.key) ?? [])].filter((key) => near.has(key));
      return seen.length === 0
        ? slot.get(block.key)!
        : seen.reduce((sum, key) => sum + slot.get(key)!, 0) / seen.length;
    };
    const at = new Map(row.map((block) => [block.key, centre(block)]));
    row.sort((a, b) => at.get(a.key)! - at.get(b.key)!);
  };
  for (let round = 0; round < 3; round += 1) {
    for (let r = 1; r < rows.length; r += 1) {
      sweep(rows[r], rows[r - 1]);
      number();
    }
    for (let r = rows.length - 2; r >= 0; r -= 1) {
      sweep(rows[r], rows[r + 1]);
      number();
    }
  }

  /*
    A flat side cut by its imports puts a dozen groups on one layer, and drawn as one line that
    layer was five times wider than the shell beside it — fitted, both sides shrank to unreadable.
    Wrapped lines keep the crossing order the sweeps found and sit closer than layers do, so a
    wrapped layer still reads as one tier.
  */
  const lines: { row: Block[]; wrapped: boolean }[] = rows.flatMap((row) => {
    if (row.length <= ROW_MAX) return [{ row, wrapped: false }];
    const per = Math.ceil(row.length / Math.ceil(row.length / ROW_MAX));
    const out: { row: Block[]; wrapped: boolean }[] = [];
    for (let i = 0; i < row.length; i += per)
      out.push({ row: row.slice(i, i + per), wrapped: i > 0 });
    return out;
  });
  const tops: number[] = [];
  lines.forEach((line, i) => {
    tops.push(i === 0 ? PAD : tops[i - 1] + NODE_H + (line.wrapped ? WRAP_GAP : ROW_GAP));
  });

  const widths = lines.map(
    ({ row }) =>
      row.reduce((sum, block) => sum + nodeWidth(block), 0) + NODE_GAP * (row.length - 1),
  );
  const width = Math.max(...widths, 160) + PAD * 2;
  const nodes = new Map<string, GraphNode>();
  lines.forEach(({ row }, r) => {
    let x = (width - widths[r]) / 2;
    for (const block of row) {
      const w = nodeWidth(block);
      nodes.set(block.key, { block, x, y: tops[r], w, h: NODE_H });
      x += w + NODE_GAP;
    }
  });

  const links: GraphLink[] = [];
  for (const [at, count] of edges) {
    const [from, to] = at.split(EDGE);
    const a = nodes.get(from);
    const b = nodes.get(to);
    if (a === undefined || b === undefined) continue;
    const ax = a.x + a.w / 2;
    const bx = b.x + b.w / 2;
    let path: string;
    const up = b.y <= a.y;
    if (b.y > a.y) {
      const y1 = a.y + a.h;
      const y2 = b.y;
      const bend = (y2 - y1) / 2;
      path = `M${ax},${y1} C${ax},${y1 + bend} ${bx},${y2 - bend} ${bx},${y2}`;
    } else if (b.y < a.y) {
      const y1 = a.y;
      const y2 = b.y + b.h;
      const bend = (y1 - y2) / 2;
      path = `M${ax + 6},${y1} C${ax + 6},${y1 - bend} ${bx + 6},${y2 + bend} ${bx + 6},${y2}`;
    } else {
      // Same row: an arc under the two, deeper the farther apart they are, so neighbours keep a
      // tight loop and a long jump clears it instead of tracing the same curve.
      const y = a.y + a.h;
      // Never into the line below, which a wrapped layer puts closer than the next layer.
      const next = tops[tops.indexOf(a.y) + 1];
      const room = next === undefined ? ROW_GAP : next - y;
      const dip = Math.min(room - 8, 12 + Math.abs(bx - ax) * 0.18);
      const off = ax < bx ? 4 : -4;
      path = `M${ax + off},${y} C${ax + off},${y + dip} ${bx + off},${y + dip} ${bx + off},${y}`;
    }
    links.push({ from, to, count, path, up });
  }
  // Heaviest last, so the lines that matter are drawn over the incidental ones.
  links.sort((a, b) => a.count - b.count);
  const height = (tops[tops.length - 1] ?? PAD) + NODE_H + PAD + 20;
  return { width, height, nodes: [...nodes.values()], links };
}
