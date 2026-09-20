// §spec notch-de-quota
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * How much of each assistant's usage window the owner has burned — `GET /quota`.
 *
 * **The route never fails.** A quota sidecar that is down comes back as a normal 200 carrying
 * `source: "stored"` and an `unreachable` sentence, with whatever figures were last recorded. That
 * is deliberate on the daemon's side and it changes what a page must do: switch on `source`, never
 * on whether the request threw. An empty answer and an unanswered question look identical on
 * screen and mean opposite things.
 *
 * **`state` comes from the núcleo, not from a threshold in here.** The bands become the owner's
 * settings in a later phase, and a copy of them in TypeScript would be a second policy drifting on
 * its own schedule. A component hands the literal to `readState("quota", …)` and paints what comes
 * back.
 *
 * **`used_fraction` is a fraction in [0,1], never a percentage.** Both providers report percentages
 * in fields named inconsistently and the sidecar converts at its own edge — that conversion was
 * missing once and drew every ring full at 54%.
 */

/** One limit period of one provider — `quota::Window`. */
export interface QuotaWindow {
  /** `5h` or `7d`. This design's vocabulary, not the vendor's. */
  window: string;
  /** In [0,1]. */
  used_fraction: number;
  /** RFC3339, or null — a window that never says when it reopens is a real answer. */
  resets_at: string | null;
  /** The reading describes a window that has already rolled over. */
  stale: boolean;
  /** One of `QUOTA_STATES` in `core/src/quota.rs`. Handed straight to `readState("quota", …)`. */
  state: string;
}

/** One provider's quota — `quota::Provider`. */
export interface QuotaProvider {
  provider: string;
  /**
   * `official` is the vendor's own endpoint; `derived` was computed from files on this machine and
   * only moves when the owner runs something; `unmeasured` means there was nothing to read, and it
   * carries no windows at all rather than a zero.
   */
  fidelity: "official" | "derived" | "unmeasured";
  /** RFC3339. The age of the figure, which is shown rather than implied to be nil. */
  read_at: string;
  windows: QuotaWindow[];
  /** Why a reading is `unmeasured`, in the owner's words. Empty otherwise. */
  detail: string;
  /** The vendor's own word for how bad this is. Shown, never acted on. */
  severity: string;
}

export interface QuotaReport {
  providers: QuotaProvider[];
  /** `sidecar` is live; `stored` means the sidecar could not be reached and these are last known. */
  source: "sidecar" | "stored";
  /** The sidecar answered from its own TTL window rather than calling the vendor. */
  cached: boolean;
  /** Present only when `source` is `stored`. */
  unreachable?: string;
}

/**
 * Whether the answer in hand is a degraded one, and therefore worth replacing sooner.
 *
 * Two shapes of the same fact, and both of them are about the PROVIDER: the route fell back to the
 * table (`source: "stored"`), or a provider could not be read at all (`fidelity: "unmeasured"`).
 * Each says the number on screen is not the current one *and* that somebody is expected to fix it —
 * the sidecar comes back up, the vendor stops refusing, the owner signs in again — so asking every
 * ten seconds instead of every minute buys the recovery being noticed quickly.
 *
 * **A stale window on its own is deliberately NOT enough**, and that is the whole narrowing. A
 * rolled-over window is not necessarily a transient condition: on a machine that never runs Codex,
 * the Codex 5h window sits past its reset forever and no future poll will change it. Counting it
 * would put the app on the ten-second cadence permanently, which is exactly the cost `POLL.quota`'s
 * minute exists to avoid — a quota display must not become a reason to spend quota. The window's
 * own `stale` flag is still how the ring is drawn; it is just not evidence that asking again sooner
 * would help.
 */
export function isDegraded(report: QuotaReport): boolean {
  return (
    report.source === "stored" ||
    report.providers.some((provider) => provider.fidelity === "unmeasured")
  );
}

/**
 * The cadence for the next poll, decided against the answer already in the cache.
 *
 * Exported and pure so the decision can be asserted without mounting anything — the same reason
 * `pollWhile` is shaped the way it is. It is not `pollWhile` itself, because this never stops: a
 * quota has no terminal state, and a quota that is spent is the reading most worth keeping fresh,
 * since the next thing it does is reopen.
 *
 * **A failed request is not how this route reports an outage.** `GET /quota` answers 200 with
 * `source: "stored"` when the sidecar is down, so a `refetchInterval` that only switched on
 * `isError` would sit at a minute through exactly the outage it was meant to shorten. The error
 * branch is still here for the case the route itself is gone — the shell not reaching its own
 * núcleo — and it wants the same ten seconds.
 */
export function quotaCadence(query: {
  state: { data: QuotaReport | undefined; status: string };
}): number {
  if (query.state.status === "error") return POLL.quotaDegraded;
  const report = query.state.data;
  if (report === undefined) return POLL.quota;
  return isDegraded(report) ? POLL.quotaDegraded : POLL.quota;
}

/**
 * The current quota, refreshed once a minute — every ten seconds while the answer is degraded.
 */
export function useQuota() {
  return useQuery({
    queryKey: keys.quota,
    queryFn: () => apiFetch<QuotaReport>("/quota"),
    refetchInterval: quotaCadence,
  });
}
