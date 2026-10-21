/**
 * The shell-side fleet constants, in one place (the daemon's live in `core/src/fleet/limits.rs`).
 */

/** A parent with MORE done children than this (counting `done_omitted`) folds them into one group node. */
export const GROUP_THRESHOLD = 6;
/** Below this zoom a node is drawn as a card: header, current line, footer. */
export const CARD_ZOOM = 0.6;
/** No byte for this long counts as a dead connection (the hub heartbeats every 15 s). */
export const SILENCE_MS = 30_000;
/** Rows of the terminal body, and how many lines the store keeps per node. */
export const RING_ROWS = 12;
/** This many failed reconnects in a row, with nothing yet shown, and the page says it cannot reach the stream. */
export const UNREACHABLE_AFTER = 3;
/** The route, one place so tests and the stream agree. */
export const LIVE_PATH = "/fleet/live";

/** Reconnect delay for the nth consecutive failed attempt (0-based): 1, 2, 4, 8, then 10 s. */
export function backoffFor(attempt: number): number {
  return Math.min(1000 * 2 ** attempt, 10_000);
}
