import { useCallback, useEffect, useState } from "react";

/**
 * Where the owner put the quota notch: which edge of the screen it hangs from, and how far along
 * that edge — 0 the top (or left) of the screen's work area, 1 the bottom (or right), one half the
 * middle it hung from before anybody moved it. The right edge until somebody drags it to another.
 *
 * **A fraction of the work area, not pixels,** because it has to mean the same place in both hosts
 * and across everything pixels do not survive: the floating window hangs from it (`notch.rs`,
 * `hang_along`, fed through `notch_fit`), and the contained notch hangs from the same line of the
 * same screen (`screen-line.ts`) — so docking or floating the notch does not move it, and neither
 * does a taskbar that moved or a resolution that changed.
 *
 * `localStorage` for the reasons `last-place.ts` gives: a property of this machine, readable before
 * the first request, and nothing a daemon still starting up should be able to move. Both windows
 * share the origin, so they share the value — and the `storage` event tells the one not being
 * dragged, which is how a notch moved while floating docks back in the same place.
 */

const KEY = "nucleos.notch-along";
const EDGE_KEY = "nucleos.notch-edge";

/** The four edges the notch can hang from. `right` is where it always hung. */
export type NotchEdge = "right" | "left" | "top" | "bottom";

export const EDGES: readonly NotchEdge[] = ["right", "left", "top", "bottom"];

/** Where the notch is: the edge, and the fraction of the way along it. */
export interface NotchPlace {
  edge: NotchEdge;
  along: number;
}

/** Left and right hold a column; top and bottom lay the readings out side by side. */
export function isVertical(edge: NotchEdge): boolean {
  return edge === "right" || edge === "left";
}

/** An edge the notch can hang from. Anything else — a hand-edited value, nothing — is the right. */
export function readEdgeWord(word: unknown): NotchEdge {
  return EDGES.includes(word as NotchEdge) ? (word as NotchEdge) : "right";
}

/** Where the notch hangs until somebody moves it: the middle of the edge, as it always has. */
export const MIDDLE = 0.5;

/** A fraction the notch can hang from. Not a number is the middle; outside [0, 1] the nearer end. */
export function clampAlong(value: number): number {
  return Number.isFinite(value) ? Math.min(1, Math.max(0, value)) : MIDDLE;
}

/**
 * Where the owner last put the notch. Read from storage a person can edit, so checked rather than
 * trusted, and a storage that throws is the middle: a remembered position is a convenience, and
 * the notch in its old place is no worse than it was before this file existed.
 */
export function readAlong(): number {
  try {
    const stored = window.localStorage.getItem(KEY);
    if (stored === null || stored.trim() === "") return MIDDLE;
    return clampAlong(Number(stored));
  } catch {
    return MIDDLE;
  }
}

/** The edge the owner last put the notch on, checked for the reason `readAlong` gives. */
export function readEdge(): NotchEdge {
  try {
    return readEdgeWord(window.localStorage.getItem(EDGE_KEY));
  } catch {
    return "right";
  }
}

export function readPlace(): NotchPlace {
  return { edge: readEdge(), along: readAlong() };
}

/** Remembers where the notch is now. Silent on a storage that refuses, for the reason above. */
export function writeAlong(along: number): void {
  try {
    window.localStorage.setItem(KEY, String(clampAlong(along)));
  } catch {
    // Nothing to do: the next launch hangs the notch in the middle.
  }
}

/** Remembers the edge as well as the fraction along it. */
export function writePlace(place: NotchPlace): void {
  writeAlong(place.along);
  try {
    window.localStorage.setItem(EDGE_KEY, readEdgeWord(place.edge));
  } catch {
    // Nothing to do: the next launch hangs the notch on the right.
  }
}

/**
 * The notch's place, and how to move it.
 *
 * `move(place, false)` while a drag is under way puts the notch there without writing anything —
 * a drag is sixty positions a second and only the last one is a decision. `move(place, true)`
 * is the drop, and the drop is what is remembered. Both keys are heard from the other window, so a
 * notch moved to another edge while floating docks back on that edge.
 */
export function useNotchPlace(): [NotchPlace, (place: NotchPlace, done: boolean) => void] {
  const [place, setPlace] = useState(readPlace);

  useEffect(() => {
    const heard = (event: StorageEvent) => {
      if (event.key === KEY || event.key === EDGE_KEY) setPlace(readPlace());
    };
    window.addEventListener("storage", heard);
    return () => window.removeEventListener("storage", heard);
  }, []);

  const move = useCallback((next: NotchPlace, done: boolean) => {
    const placed = { edge: readEdgeWord(next.edge), along: clampAlong(next.along) };
    setPlace((current) =>
      current.edge === placed.edge && current.along === placed.along ? current : placed,
    );
    if (done) writePlace(placed);
  }, []);

  return [place, move];
}
