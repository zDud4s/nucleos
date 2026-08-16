/**
 * Where each node sits on the fleet canvas.
 *
 * Pure, and stored in this window rather than in the daemon. A layout is a preference of whoever is
 * looking — the same fleet is a different arrangement on a laptop and on a second monitor — and
 * making it system state would mean a migration and a route for something no other reader consults.
 *
 * The unit is the NODE KEY, `"job:41"` and not `41`: job ids and run ids come from different
 * sequences and collide constantly, and a layout keyed on the bare number would drop a run on top of
 * the job that shares it. Same reasoning `slotDetail` gives for comparing the pair.
 */

/** A point on the canvas, in CSS pixels from the top left of the surface. */
export interface Point {
  x: number;
  y: number;
}

export type Layout = Record<string, Point>;

/**
 * How far apart the fallback positions sit, and how wide a row is before it wraps.
 *
 * **A cell has to be bigger than a card**, and the first pass at this got it wrong: 260 wide was
 * exactly a card's width and 190 tall was well under one, so two nodes in the same column of the
 * grid overlapped on the very first paint. jsdom cannot see that — it does no layout — and the
 * screenshot could.
 */
const STEP_X = 300;
const STEP_Y = 340;
const PER_ROW = 4;
const MARGIN = 24;

/**
 * How many cells the derived positions are spread over before they repeat.
 *
 * Cells beyond this are still valid — `cellPosition` simply carries on into lower rows — and that
 * is what gives the collision walk below somewhere to go.
 */
const CELLS = PER_ROW * PER_ROW;

/** Where the nth cell of the fallback grid is. Defined for every n, not only the first `CELLS`. */
function cellPosition(index: number): Point {
  return {
    x: MARGIN + (index % PER_ROW) * STEP_X,
    y: MARGIN + Math.floor(index / PER_ROW) * STEP_Y,
  };
}

/**
 * Where a node with no saved position goes.
 *
 * **Derived from the key, never `(0, 0)`.** The first time somebody opens the canvas nothing has a
 * saved position, and dropping every node at the origin would put the whole fleet in one pile in the
 * corner — which reads as a broken canvas, and has to be undone by hand before the feature can be
 * judged at all.
 *
 * Derived and not random: the same node lands in the same place on every reload until somebody moves
 * it, so the arrangement a person learns stays learned. A random scatter would look better on the
 * first paint and be unusable on the second.
 */
export function fallbackPosition(key: string): Point {
  return cellPosition(hash(key) % CELLS);
}

/**
 * A small, stable, order-independent hash of the key.
 *
 * FNV-1a, in 32 bits. It is here so the fallback is a pure function of the key — the alternative,
 * spreading nodes by their position in the current list, moves every node whenever one of them ends.
 */
function hash(key: string): number {
  let value = 0x811c9dc5;
  for (let index = 0; index < key.length; index += 1) {
    value ^= key.charCodeAt(index);
    value = Math.imul(value, 0x01000193) >>> 0;
  }
  return value;
}

/**
 * The position of every live node, saved or derived.
 *
 * Takes the keys that exist NOW and answers for exactly those. That is what prunes the layout: a job
 * that ended keeps its entry in storage until the next write, and this never hands it back, so
 * nothing downstream can draw a card for work that is over.
 *
 * **Two keys that want the same cell are separated here**, and that is a deliberate dent in the
 * "a node never moves because its neighbours changed" rule. It has to be: any position derived from
 * the key alone collides, and measuring it says half of all five-job fleets contain such a pair. The
 * two failures are not comparable — a card exactly underneath another cannot be read, cannot be
 * clicked, and cannot even be dragged out from under, because the one on top takes the pointer.
 *
 * The tie is broken by the KEY and not by arrival order, so it is the same node that gives way every
 * time, and the one that keeps the cell keeps it whoever else turns up. The loser walks to the next
 * free cell, which is why cells past the last row still have to be valid.
 */
export function positionsFor(keys: string[], saved: Layout): Layout {
  const layout: Layout = {};
  const taken = new Set<number>();
  for (const key of [...keys].sort()) {
    const chosen = saved[key];
    if (chosen !== undefined) {
      layout[key] = chosen;
      continue;
    }
    let cell = hash(key) % CELLS;
    while (taken.has(cell)) cell += 1;
    taken.add(cell);
    layout[key] = cellPosition(cell);
  }
  return layout;
}

/**
 * A whole-pixel position no node can be lost at.
 *
 * **Nothing lives in negative space.** The surface scrolls into positive coordinates only, so a card
 * dropped past the top or the left edge is partly unreachable — and one dropped far enough past it
 * is gone altogether, with no way back short of clearing the browser's storage by hand. Caught by
 * dragging a card off the left edge in a real browser; jsdom has no edges to fall off.
 */
export function clamped(at: Point): Point {
  return { x: Math.max(0, Math.round(at.x)), y: Math.max(0, Math.round(at.y)) };
}

/** The saved layout with one node moved. Returns a new object; the input is not touched. */
export function withMoved(layout: Layout, key: string, to: Point): Layout {
  return { ...layout, [key]: clamped(to) };
}

/**
 * Drops entries for nodes that are no longer there.
 *
 * Called before writing, not before reading — `positionsFor` already ignores the dead ones, so this
 * exists only to stop the stored object growing by one entry per job for the life of the machine.
 */
export function pruned(layout: Layout, keys: string[]): Layout {
  const live = new Set(keys);
  const kept: Layout = {};
  for (const [key, point] of Object.entries(layout)) {
    if (live.has(key)) kept[key] = point;
  }
  return kept;
}

const STORAGE_KEY = "nucleos.fleet.layout";

/**
 * Reads the saved layout, and treats anything it does not recognise as nothing.
 *
 * `localStorage` holds text somebody else wrote — a previous version of this app, an extension, a
 * person with the console open — so every field is checked rather than trusted. A malformed entry
 * costs one node its saved position; a malformed entry that got through would cost a `NaN` in a CSS
 * transform, which draws nothing and reports nothing.
 */
export function readLayout(storage: Pick<Storage, "getItem">): Layout {
  const raw = storage.getItem(STORAGE_KEY);
  if (raw === null) return {};
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    const layout: Layout = {};
    for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
      const point = value as Partial<Point> | null;
      if (
        point !== null &&
        typeof point === "object" &&
        Number.isFinite(point.x) &&
        Number.isFinite(point.y)
      ) {
        layout[key] = { x: point.x as number, y: point.y as number };
      }
    }
    return layout;
  } catch {
    return {};
  }
}

/** Writes the layout, and says nothing when it cannot. */
export function writeLayout(storage: Pick<Storage, "setItem">, layout: Layout): void {
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(layout));
  } catch {
    // Private mode, a full quota, a locked-down profile. Losing an arrangement is not worth taking
    // the canvas down over, and there is nothing the person could do about it if told.
  }
}

/** Which of the two ways of looking at the fleet is open. */
export type FleetView = "columns" | "canvas";

const VIEW_KEY = "nucleos.fleet.view";

/**
 * Which view was open last time, kept beside the layout because it is the same kind of thing: a
 * preference of whoever is looking, not something the daemon has an opinion about.
 *
 * **Anything unrecognised reads as `columns`**, and that is not merely a default. The columns are
 * the view that already existed and the only one carrying `n/limit`, so a corrupted preference costs
 * a click rather than opening a screen the reader cannot get capacity out of.
 */
export function readView(storage: Pick<Storage, "getItem">): FleetView {
  return storage.getItem(VIEW_KEY) === "canvas" ? "canvas" : "columns";
}

/** Remembers the view, and says nothing when it cannot — same reasoning as the layout. */
export function writeView(storage: Pick<Storage, "setItem">, view: FleetView): void {
  try {
    storage.setItem(VIEW_KEY, view);
  } catch {
    // A preference is worth even less than an arrangement; it is certainly not worth an exception.
  }
}
