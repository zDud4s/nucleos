/**
 * Poll cadences, in one place.
 *
 * The núcleo exposes no SSE and no WebSocket — polling is the contract, not a
 * stopgap. That makes the interval a product decision rather than an
 * implementation detail, and product decisions belong somewhere a person can
 * read them all at once. Pages never name a number: they ask a hook, and the
 * hook fixes the cadence.
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
