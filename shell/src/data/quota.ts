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
 * The current quota, refreshed once a minute.
 *
 * No `pollWhile`: there is no terminal state to stop on. A quota that is spent is the reading most
 * worth keeping fresh, because the next thing it does is reopen.
 */
export function useQuota() {
  return useQuery({
    queryKey: keys.quota,
    queryFn: () => apiFetch<QuotaReport>("/quota"),
    refetchInterval: POLL.quota,
  });
}
