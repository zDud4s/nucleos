import { useSyncExternalStore } from "react";

/**
 * One 1 Hz clock for every running node's elapsed time, instead of one timer per node.
 *
 * The clock only ticks while somebody is subscribed, and a subscriber refreshes `now` on the way
 * in, so a node mounted after a long pause does not show a stale reading for its first second.
 */
let now = Date.now();
let timer: ReturnType<typeof setInterval> | undefined;
const listeners = new Set<() => void>();

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  if (listeners.size === 1) {
    now = Date.now();
    timer = setInterval(() => {
      now = Date.now();
      listeners.forEach((listener) => listener());
    }, 1000);
  }
  return () => {
    listeners.delete(cb);
    if (listeners.size === 0 && timer !== undefined) {
      clearInterval(timer);
      timer = undefined;
    }
  };
}
const getNow = (): number => now;
const never = (): (() => void) => () => undefined;

/** The current time, ticking each second while `active`; frozen (and unsubscribed) otherwise. */
export function useNow(active: boolean): number {
  return useSyncExternalStore(active ? subscribe : never, getNow);
}
