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

/** How far apart the fallback positions sit, and how wide a row is before it wraps. */
const STEP_X = 260;
const STEP_Y = 190;
const PER_ROW = 4;
const MARGIN = 24;

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
  const slot = hash(key) % (PER_ROW * PER_ROW);
  return {
    x: MARGIN + (slot % PER_ROW) * STEP_X,
    y: MARGIN + Math.floor(slot / PER_ROW) * STEP_Y,
  };
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
 */
export function positionsFor(keys: string[], saved: Layout): Layout {
  const layout: Layout = {};
  for (const key of keys) {
    layout[key] = saved[key] ?? fallbackPosition(key);
  }
  return layout;
}

/** The saved layout with one node moved. Returns a new object; the input is not touched. */
export function withMoved(layout: Layout, key: string, to: Point): Layout {
  return { ...layout, [key]: { x: Math.round(to.x), y: Math.round(to.y) } };
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
