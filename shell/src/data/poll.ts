/**
 * Poll cadences, in one place.
 *
 * The núcleo has no SSE and no WebSocket, but it does stream in two places —
 * the browser live view (`/browser/sessions/{id}/live`) and the fleet tree
 * (`/fleet/live`, NDJSON) — and both are read through `openStream`, not here.
 * For every other read polling is the contract, not a stopgap. That makes the
 * interval a product decision rather than an implementation detail, and
 * product decisions belong somewhere a person can read them all at once.
 * Pages never name a number: they ask a hook, and the hook fixes the cadence.
 */
export const POLL = {
  /** A chat turn in flight. The fastest thing in the app, because it is the one you watch. */
  turn: 1500,
  /** A council still filling seats. */
  council: 2000,
  /** Fleet, autopilot, live runs — anything that is the state of the machine right now. */
  fast: 3000,
  /** Queues that only move when something lands in them. */
  queue: 5000,
  /** The calendar. A month does not change every three seconds. */
  slow: 30000,
  /** Voice captures, which arrive from outside the window. */
  voice: 5000,
  /**
   * Provider quota. A minute, and the number is not a taste.
   *
   * It is the TTL the quota sidecar holds a good reading for, which is itself the one the Python
   * dashboard settled on against the same endpoint. Asking faster buys nothing — the sidecar
   * answers from its cache — and the whole point of a quota display is that watching the quota must
   * not become a reason to run out of it.
   */
  quota: 60000,
  /**
   * Provider quota that is not answering, or answering from the table.
   *
   * The other cadence in the same plan, and the one that makes the minute above affordable: while
   * the sidecar is down or a provider cannot be read at all, the displayed figure is already old
   * and the next good answer is the one worth having quickly. Ten seconds is the error TTL the
   * sidecar itself holds, so asking faster than this only re-reads its cache.
   *
   * Only conditions somebody is expected to come back from count — see `isDegraded` in `quota.ts`,
   * which deliberately does not count a lone rolled-over window, because on a machine that does not
   * run a provider that window is the steady state and this cadence would never end.
   */
  quotaDegraded: 10000,
} as const;

export type PollCadence = (typeof POLL)[keyof typeof POLL];

/**
 * The minimum of a react-query query that a vitality predicate needs to see.
 *
 * Structural on purpose: this module must stay pure and importable by a test
 * that never mounts a component, so it describes the shape rather than
 * importing `Query` from the library.
 */
export interface PolledQuery<T> {
  state: { data: T | undefined };
}

/**
 * A `refetchInterval` that stops when the thing stops.
 *
 * A run that has exited, a council that has settled and a queue that is empty
 * all answer the same bytes forever; polling them is a cost with no reader.
 * `alive` decides per tick, against the data already in the cache — so the
 * poll switches itself off on the tick that lands the final state, without a
 * second render to notice.
 *
 * Undefined data reads as alive: it means the first answer has not arrived,
 * and a query that turns its own poll off before its first success would
 * never start.
 */
export function pollWhile<T>(
  cadence: PollCadence,
  alive: (data: T) => boolean,
): (query: PolledQuery<T>) => number | false {
  return (query) => {
    const data = query.state.data;
    if (data === undefined) return cadence;
    return alive(data) ? cadence : false;
  };
}

/**
 * Where the window stands: `focused` polls at the pages' own cadences, `blurred` (visible, not
 * focused) refreshes at the blurred cadence in `app/pacing.ts`, and `hidden` (minimised or in the tray) leaves
 * only the queries that say they must keep going.
 */
export type Attention = "focused" | "blurred" | "hidden";

export function attentionOf(visible: boolean, focused: boolean): Attention {
  if (!visible) return "hidden";
  return focused ? "focused" : "blurred";
}

export function currentAttention(): Attention {
  if (typeof document === "undefined") return "focused";
  return attentionOf(document.visibilityState !== "hidden", document.hasFocus());
}

/**
 * How often a window that is on screen but not focused refreshes what it shows.
 *
 * Not zero: a conversation left open beside an editor is still being watched, and an answer
 * that stopped moving the moment somebody clicked elsewhere would read as a stuck turn. But it is
 * a glance, not a stare, and the 1.5–3 s cadences the pages ask for are for the window somebody
 * is working in.
 */
export const BLURRED_CADENCE = 10_000;

/**
 * The cadence for a query that keeps polling in the background, slowed while the window is
 * blurred or hidden.
 *
 * Health, the kill switch, the credential check and the chat list keep going in the tray, each for
 * a reason written beside it — but none of those reasons needs three seconds. Coming back from the
 * tray refetches every stale query at once anyway (react-query's refetch on focus), so a hidden
 * window only has to notice a change eventually, and the window somebody returns to is current the
 * instant it is shown. React-query re-reads this after every fetch, which is when it switches.
 *
 * Blurred (visible, not focused) gets {@link BLURRED_CADENCE}: these queries opt out of the blurred
 * sweep in `app/pacing.ts` (`refetchInterval` skips ticks only when hidden), so without this they
 * would poll at the focused cadence all day beside an editor. Hidden is slower still.
 */
export function backgroundCadence(visibleMs: number): () => number {
  return () => {
    const attention = currentAttention();
    if (attention === "hidden") return POLL.slow;
    return attention === "blurred" ? Math.max(visibleMs, BLURRED_CADENCE) : visibleMs;
  };
}
