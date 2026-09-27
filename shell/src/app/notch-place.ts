import { useCallback, useEffect, useState } from "react";

/**
 * How far down the right edge the owner put the quota notch: 0 the top of the screen's work area,
 * 1 the bottom, one half the middle it hung from before anybody moved it.
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

/** Remembers where the notch is now. Silent on a storage that refuses, for the reason above. */
export function writeAlong(along: number): void {
  try {
    window.localStorage.setItem(KEY, String(clampAlong(along)));
  } catch {
    // Nothing to do: the next launch hangs the notch in the middle.
  }
}

/**
 * The notch's place, and how to move it.
 *
 * `move(along, false)` while a drag is under way puts the notch there without writing anything —
 * a drag is sixty positions a second and only the last one is a decision. `move(along, true)`
 * is the drop, and the drop is what is remembered.
 */
export function useNotchAlong(): [number, (along: number, done: boolean) => void] {
  const [along, setAlong] = useState(readAlong);

  useEffect(() => {
    const heard = (event: StorageEvent) => {
      if (event.key === KEY) setAlong(readAlong());
    };
    window.addEventListener("storage", heard);
    return () => window.removeEventListener("storage", heard);
  }, []);

  const move = useCallback((next: number, done: boolean) => {
    const placed = clampAlong(next);
    setAlong(placed);
    if (done) writeAlong(placed);
  }, []);

  return [along, move];
}
