import type { Turn } from "../lib/turns";
import { turnIsLive } from "../lib/turns";
import { formatSpan } from "../lib/when";

const MINUTE = 60_000;

/** The prompt-cache lifetimes the CLI uses, in milliseconds. */
const TTL_MS = { "5m": 5 * MINUTE, "1h": 60 * MINUTE } as const;

export type CacheState =
  /** A turn is running, which re-reads the prefix and so refreshes the cache. */
  | { kind: "refreshing"; ms: number; approx: boolean }
  | { kind: "warm"; ms: number; approx: boolean }
  | { kind: "cold"; approx: boolean };

/**
 * How long the conversation's prompt cache has left, from its last turn.
 *
 * The clock starts when the last turn SETTLED (`completedAt`, falling back to `createdAt` on a
 * daemon that does not send it) and runs for the turn's TTL. A daemon that does not say which TTL
 * applies is assumed to be on the short one, and the answer is marked `approx` so the chip can say
 * it is a guess. Null when there is no turn to measure from.
 */
export function cacheState(turns: Turn[] | undefined, now: number): CacheState | null {
  if (turns === undefined || turns.length === 0) return null;
  const last = turns[turns.length - 1];
  const approx = (last.cacheTtl ?? null) === null;
  const ttl = TTL_MS[last.cacheTtl ?? "5m"];
  // A turn in flight is using the cache: show the whole lifetime, refreshing.
  if (turnIsLive(last.status)) return { kind: "refreshing", ms: ttl, approx };
  const from = Date.parse(last.completedAt ?? last.createdAt);
  if (Number.isNaN(from)) return null;
  const left = from + ttl - now;
  if (left <= 0) return { kind: "cold", approx };
  return { kind: "warm", ms: left, approx };
}

/**
 * "42m", "1h 05m". Rounds up so a chip never reads 0m while the cache is still warm, and never
 * drops to seconds: the chip ticks every 30 s, so a count of seconds would be stale as it is drawn.
 */
export function leftText(ms: number): string {
  return formatSpan(Math.max(MINUTE, ms), { round: "ceil" });
}
