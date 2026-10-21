import { useRef, useSyncExternalStore } from "react";

/**
 * One shared clock per tick length, so a hundred rows asking for the time every second share one
 * timer instead of owning a hundred. `fleet/clock.ts` is the same mechanism fixed at 1 Hz; this is
 * the version a caller can pick the rate of.
 *
 * A clock only ticks while somebody is subscribed, and the first subscriber refreshes `now` on the
 * way in. React re-reads the snapshot right after subscribing, so a component mounted after a long
 * pause corrects its first (stale) render at once rather than after a whole tick.
 */
interface Clock {
  now: number;
  timer: ReturnType<typeof setInterval> | undefined;
  readonly listeners: Set<() => void>;
}

const clocks = new Map<number, Clock>();

function clockFor(everyMs: number): Clock {
  let clock = clocks.get(everyMs);
  if (clock === undefined) {
    clock = { now: Date.now(), timer: undefined, listeners: new Set() };
    clocks.set(everyMs, clock);
  }
  return clock;
}

type Subscribe = (cb: () => void) => () => void;

/**
 * `useSyncExternalStore` compares subscribe functions by identity and re-subscribes when it
 * changes, so one is built per tick length and kept, never made fresh in a render.
 */
const subscriptions = new Map<number, Subscribe>();

function subscription(everyMs: number): Subscribe {
  let subscribe = subscriptions.get(everyMs);
  if (subscribe === undefined) {
    subscribe = subscribeTo(everyMs);
    subscriptions.set(everyMs, subscribe);
  }
  return subscribe;
}

function subscribeTo(everyMs: number): Subscribe {
  return (cb) => {
    const clock = clockFor(everyMs);
    clock.listeners.add(cb);
    if (clock.listeners.size === 1) {
      clock.now = Date.now();
      clock.timer = setInterval(() => {
        clock.now = Date.now();
        clock.listeners.forEach((listener) => listener());
      }, everyMs);
    }
    return () => {
      clock.listeners.delete(cb);
      if (clock.listeners.size === 0 && clock.timer !== undefined) {
        clearInterval(clock.timer);
        clock.timer = undefined;
      }
    };
  };
}

const never = (): (() => void) => () => undefined;

/**
 * The current time in epoch milliseconds, re-read every `everyMs` while `active`.
 *
 * While not `active` the hook unsubscribes and returns the last reading it had, so a finished run
 * stops ticking and keeps showing the time it stopped at. `everyMs` should be a constant (1000 for
 * a stopwatch, 30_000 for "3 min ago"): each distinct value is its own shared clock.
 */
export function useNow(everyMs: number, active = true): number {
  const clock = clockFor(everyMs);
  const subscribe = active ? subscription(everyMs) : never;
  const live = useSyncExternalStore(subscribe, () => clock.now);
  // A hook that has never ticked has no "last reading" to freeze on, and the shared clock's value
  // may be hours old: an inactive mount reads the wall clock once instead.
  const frozen = useRef<number | null>(null);
  if (active) frozen.current = live;
  else frozen.current ??= Date.now();
  return active ? live : frozen.current;
}
