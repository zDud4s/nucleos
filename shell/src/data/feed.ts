// §spec mapa-do-projeto
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";
import type { BadgeTone } from "../ui";

/**
 * The feed, and the notifications the calendar is holding back.
 *
 * The núcleo's `GET /feed` is two routes wearing one name, and every decision
 * in this file follows from that. With none of `q` / `kind` / `since` / `until`
 * / `limit` it **lists**: the newest fifty of one scope. With any one of them it
 * **searches** (`get_feed`, `core/src/http.rs`). A page that polled a search
 * would be re-running a question about the past every three seconds, so the
 * cadence is decided by which of the two the filters have asked for — see
 * {@link feedIsSearching}.
 *
 * The scope is the other half. `list_feed(None)` and `FeedScope::Global` both
 * mean *the machine's own lines* — `project_id IS NULL AND errand_id IS NULL` —
 * and not "everything". A feed page that sent no scope would therefore show a
 * fraction of the feed and look broken, so `scope=all` is sent whenever nothing
 * narrower was asked for. It is **not** sent alongside a project or an errand:
 * `all` short-circuits the scope chain in both branches of the route, and
 * sending both would silently ignore the narrower one.
 */

/**
 * One line of the feed, exactly as `feed::FeedEntry` serialises.
 *
 * `errand_id` is real and is in every `SELECT` in `core/src/feed.rs` — an
 * errand and a project are two different owners and a row has at most one of
 * them, so a shell that only knew about `project_id` would read every errand's
 * line as the machine's own.
 */
export interface FeedEntry {
  id: number;
  project_id: string | null;
  kind: string;
  summary: string;
  /** The run this line is about, when it is about one. */
  run_id: number | null;
  /** The errand this line belongs to. Never set together with `project_id`. */
  errand_id: number | null;
  created_at: string;
}

/**
 * A notification the calendar held back, or held back and later delivered.
 *
 * Delivered rows are kept and served on purpose (`notify::list_pending`): the
 * question this route exists to answer is "did the calendar swallow something?",
 * and a queue that only listed what has not arrived yet cannot answer it. The
 * two must never be merged — `delivered_at` is the whole distinction.
 */
export interface PendingNotification {
  id: number;
  kind: string;
  summary: string;
  queued_at: string;
  delivered_at: string | null;
}

/**
 * The filters the page accepts, named as a person reads them.
 *
 * `project` becomes `project_id` and `errand` becomes `errand_id` in the query
 * string; the translation happens once, below, rather than at each caller.
 * Everything is a string because these come out of the location, where there
 * are no numbers.
 */
export interface FeedFilters {
  /** Free text. FTS5 over summaries in the núcleo. */
  q?: string;
  kind?: string;
  project?: string;
  errand?: string;
  /** RFC 3339, or the daemon answers 400 (`parse_time_bound`). */
  since?: string;
  until?: string;
  limit?: string;
}

/** What the daemon lists without being asked (`get_feed`). */
export const FEED_LIST_LIMIT = 50;

/** `SEARCH_LIMIT_MAX` in `core/src/http.rs`. Asking for more silently gets this. */
export const FEED_LIMIT_MAX = 200;

/** How many lines the drawer shows. Design §3.2: the last twenty. */
export const DRAWER_FEED_LIMIT = 20;

/**
 * An empty filter is not a filter for the empty string.
 *
 * `?kind=` is not the same request as leaving `kind` out — it asks for rows
 * whose kind is `""`, gets none, **and** flips the route from listing to
 * searching on the way. Both halves of that are wrong, and both are prevented
 * here rather than at each of the seven call sites.
 */
function blankToUndefined(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  const trimmed = value.trim();
  return trimmed === "" ? undefined : trimmed;
}

/** A filter set as a plain record, which is what the query key is built from. */
export function feedFilterFields(filters: FeedFilters): Record<string, string | undefined> {
  return {
    q: blankToUndefined(filters.q),
    kind: blankToUndefined(filters.kind),
    project: blankToUndefined(filters.project),
    errand: blankToUndefined(filters.errand),
    since: blankToUndefined(filters.since),
    until: blankToUndefined(filters.until),
    limit: blankToUndefined(filters.limit),
  };
}

/**
 * Has the caller turned the route from a listing into a search?
 *
 * The five fields are the daemon's own `has_search_filters`, copied field for
 * field. `project` and `errand` are deliberately **not** among them: they narrow
 * a listing, which still polls, and treating them as a search would freeze the
 * page for someone who only picked a project out of a select.
 */
export function feedIsSearching(filters: FeedFilters): boolean {
  const fields = feedFilterFields(filters);
  return (
    fields.q !== undefined ||
    fields.kind !== undefined ||
    fields.since !== undefined ||
    fields.until !== undefined ||
    fields.limit !== undefined
  );
}

/** Is anything narrowing this list at all, search or not? */
export function feedIsFiltered(filters: FeedFilters): boolean {
  return Object.values(feedFilterFields(filters)).some((value) => value !== undefined);
}

/**
 * The query string, with the scope rule applied.
 *
 * Unfilled fields are omitted rather than sent empty, and `scope=all` goes out
 * only when neither owner was named — see this module's header for why both of
 * those are load-bearing rather than tidiness.
 */
export function feedQueryString(filters: FeedFilters): string {
  const fields = feedFilterFields(filters);
  const params = new URLSearchParams();

  if (fields.project !== undefined) params.set("project_id", fields.project);
  else if (fields.errand !== undefined) params.set("errand_id", fields.errand);
  else params.set("scope", "all");

  if (fields.q !== undefined) params.set("q", fields.q);
  if (fields.kind !== undefined) params.set("kind", fields.kind);
  if (fields.since !== undefined) params.set("since", fields.since);
  if (fields.until !== undefined) params.set("until", fields.until);
  if (fields.limit !== undefined) params.set("limit", fields.limit);

  return `?${params.toString()}`;
}

/**
 * A `datetime-local` value as the daemon wants it, or `undefined`.
 *
 * `parse_time_bound` answers 400 for anything that is not RFC 3339, so a
 * half-typed bound must never leave this file. `new Date("2026-08-17T09:00")`
 * is *local* time by specification, which is what the widget means.
 */
export function boundToRfc3339(local: string): string | undefined {
  const trimmed = local.trim();
  if (trimmed === "") return undefined;
  const at = new Date(trimmed);
  if (Number.isNaN(at.getTime())) return undefined;
  return at.toISOString();
}

/** The same instant back in the widget's local spelling, to the minute. */
export function rfc3339ToBoundInput(value: string | undefined): string {
  if (value === undefined) return "";
  const at = new Date(value);
  if (Number.isNaN(at.getTime())) return "";
  const pad = (part: number) => String(part).padStart(2, "0");
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}T${pad(at.getHours())}:${pad(at.getMinutes())}`;
}

/** A typed limit, held inside what the daemon will actually honour. */
export function clampFeedLimit(value: string): string | undefined {
  const parsed = Number.parseInt(value.trim(), 10);
  if (Number.isNaN(parsed)) return undefined;
  return String(Math.min(Math.max(parsed, 1), FEED_LIMIT_MAX));
}

/**
 * The feed, listing or searching.
 *
 * `keepPreviousData` because this is a list whose filters are in the URL: a
 * keystroke changes the key, and a list that blanks between two keys makes
 * filtering feel like the app breaking.
 *
 * The poll is off while a search is active, and that is the page's whole freeze.
 * A search is a question about the past — re-asking it every three seconds
 * costs a request per tick and changes nothing on screen — while a listing is
 * the machine right now and is exactly what `POLL.fast` is for.
 */
export function useFeed(filters: FeedFilters, options: { enabled?: boolean } = {}) {
  const fields = feedFilterFields(filters);
  return useQuery({
    queryKey: keys.feed.search(fields),
    queryFn: () => apiFetch<FeedEntry[]>(`/feed${feedQueryString(filters)}`),
    enabled: options.enabled !== false,
    refetchInterval: feedIsSearching(filters) ? false : POLL.fast,
    placeholderData: keepPreviousData,
  });
}

/**
 * The last {@link DRAWER_FEED_LIMIT} lines from every scope, for the drawer.
 *
 * Its own hook rather than `useFeed({ limit })`, because the freeze above would
 * catch it: `limit` is one of the daemon's search fields, so the drawer would
 * open on a snapshot and quietly stop refreshing while it stayed open. This
 * window is a fixed listing nobody typed, not somebody's question about the
 * past, so it keeps a cadence — the slow one, since a drawer is read at a
 * glance and not watched.
 *
 * `enabled` is the other half of the cost: the drawer is mounted on every page
 * in the app, and a hook that fetched while shut would poll the feed for the
 * whole session on behalf of a panel nobody has opened.
 */
export function useRecentFeed(options: { enabled?: boolean } = {}) {
  const filters: FeedFilters = { limit: String(DRAWER_FEED_LIMIT) };
  return useQuery({
    queryKey: keys.feed.search(feedFilterFields(filters)),
    queryFn: () => apiFetch<FeedEntry[]>(`/feed${feedQueryString(filters)}`),
    enabled: options.enabled !== false,
    refetchInterval: POLL.queue,
  });
}

/**
 * What the calendar is holding, and what it held and later let through.
 *
 * `POLL.queue`: this only moves when a notification is written or the calendar
 * opens, and both are events rather than states.
 */
export function usePendingNotifications(options: { enabled?: boolean } = {}) {
  return useQuery({
    queryKey: keys.notifications.pending,
    queryFn: () => apiFetch<PendingNotification[]>("/notifications/pending"),
    enabled: options.enabled !== false,
    refetchInterval: POLL.queue,
  });
}

/** Held by the calendar right now — `delivered_at` is the whole test. */
export function isHeld(notification: PendingNotification): boolean {
  return notification.delivered_at === null;
}

/* ------------------------------------------------------------- readings -- */

export interface FeedReading {
  tone: BadgeTone;
  /** What this kind of line means, in a phrase. */
  label: string;
}

/**
 * Every `kind` the núcleo actually writes, mapped to a reading.
 *
 * **Built by enumeration, not by guessing**: every key below was taken from a
 * `feed::append` / `append_on` / `append_for_errand` call site in `core/src/`,
 * plus the two indirect writers — `job::say` (thirteen `job_*` kinds) and
 * `notify::deliver_or_defer`, which is how `token_efficiency` and the e-mail
 * classes reach the feed. Nothing here is a name that looked plausible.
 *
 * **An unmapped kind renders its own literal**, exactly as `ui/state-map.ts`
 * does for an unmapped state. The núcleo grows kinds faster than this table
 * will, and a guessed label is a claim the shell cannot support — showing the
 * raw literal admits ignorance, which is the only honest fallback.
 *
 * Two kinds are deliberately absent and cannot be added:
 *
 * - `email_<class>` is built at run time from `notify_classes`
 *   (`triage.rs`: `format!("email_{}", verdict.class)`), which is configuration.
 *   Only the shipped default — `urgent` — is mapped; anybody else's class reads
 *   as its literal, which is right, because only they know what it means.
 * - The tones are the seven of `tokens.css` and nothing else. Where the núcleo's
 *   own line covers several outcomes at once — `vcs_request_finished` carries
 *   *succeeded*, *failed*, *blocked* and *escalated*; `council_finished` carries
 *   whatever status settled it — the reading is `info` and the verdict is left
 *   in the summary, rather than the shell picking one of four and being wrong
 *   three times.
 */
export const FEED_KINDS: Record<string, FeedReading> = {
  /* -- jobs: `job::say`, fifteen kinds ------------------------------------ */
  job_started: { tone: "active", label: "job started" },
  job_planned: { tone: "info", label: "job planned" },
  job_replanned: { tone: "info", label: "job replanned" },
  job_plan_failed: { tone: "danger", label: "job could not be planned" },
  job_item_failed: { tone: "danger", label: "job item failed" },
  job_gate_failed: { tone: "danger", label: "job gate failed" },
  job_waiting: { tone: "pending", label: "job waiting" },
  /**
   * A round in which no item passed has nothing for a review to judge, so none
   * runs (job 27, 2026-09-14: a review read a reverted tree and reported "no work
   * was done"). Information, not a failure: the red items already said so.
   */
  job_review_skipped: { tone: "info", label: "job review skipped" },
  /**
   * A review that never reached the API is run once more (job 26, 2026-09-13:
   * a DNS outage ended it and the next round opened without a verdict). Pending,
   * because the verdict it stands for is still to come.
   */
  job_review_retried: { tone: "pending", label: "job review retried" },
  job_finished: { tone: "active", label: "job finished" },
  job_failed: { tone: "danger", label: "job failed" },
  /**
   * The three ways a job stops that are **not** failures, and never share a
   * reading with `job_failed` — §7's sharpest row. `stopped` is a person or a
   * brake halting the chain, `cancelled` is the request being withdrawn, and
   * `expired` is the four-hour window closing on it.
   */
  job_stopped: { tone: "off", label: "job stopped" },
  job_cancelled: { tone: "off", label: "job cancelled" },
  job_expired: { tone: "paused", label: "job expired" },
  /** The daemon died under it. A defect in us, not in the work. */
  job_interrupted: { tone: "paused", label: "job interrupted" },

  /* -- runs --------------------------------------------------------------- */
  run_retry: { tone: "info", label: "run retried" },
  run_failed_final: { tone: "danger", label: "run failed for good" },
  run_interrupted: { tone: "paused", label: "run interrupted" },
  run_stopped_probing: { tone: "danger", label: "run stopped after repeated refusals" },
  /** Not an alarm. See {@link readEfficiencySignal}. */
  token_efficiency: { tone: "info", label: "efficiency observation" },

  /* -- worktrees ---------------------------------------------------------- */
  worktree_gate_failed: { tone: "danger", label: "worktree gate failed" },
  worktree_provision_failed: { tone: "danger", label: "worktree could not be made" },
  worktree_released: { tone: "off", label: "worktree released" },
  worktree_branch_kept: { tone: "info", label: "unmerged branch kept" },
  worktree_removed: { tone: "off", label: "worktree removed" },
  worktree_gc_failed: { tone: "danger", label: "worktree cleanup failed" },

  /* -- the git queue ------------------------------------------------------ */
  vcs_request_finished: { tone: "info", label: "git request settled" },
  vcs_request_cancelled: { tone: "off", label: "git request cancelled" },
  vcs_request_interrupted: { tone: "paused", label: "git request interrupted" },

  /* -- council ------------------------------------------------------------ */
  council_started: { tone: "active", label: "council started" },
  council_stage: { tone: "info", label: "council stage" },
  council_finished: { tone: "info", label: "council settled" },

  /* -- errands and the scheduler ------------------------------------------ */
  schedule_rule_invalid: { tone: "danger", label: "schedule rule invalid" },
  errand_rule_fired: { tone: "active", label: "errand rule fired" },
  errand_rule_failed: { tone: "danger", label: "errand rule failed" },
  errand_investigation_done: { tone: "active", label: "errand investigation done" },
  errand_investigation_failed: { tone: "danger", label: "errand investigation failed" },

  /* -- e-mail ------------------------------------------------------------- */
  email_digest: { tone: "info", label: "e-mail digest" },
  email_urgent: { tone: "pending", label: "urgent e-mail" },
  email_triage_failed: { tone: "danger", label: "e-mail triage failed" },
  email_triage_paused: { tone: "paused", label: "e-mail triage paused" },
  email_triage_stalled: { tone: "paused", label: "e-mail triage stalled" },
  email_fetch_skipped: { tone: "info", label: "e-mail skipped" },
  /** A misconfigured `sent_mailbox`: mail arrives and correspondents are lost. */
  email_sent_mailbox_foreign: { tone: "danger", label: "sent mail filed elsewhere" },

  /* -- governance and the rest -------------------------------------------- */
  action_authorized: { tone: "info", label: "action authorised by a grant" },
  proposal_record_failed: { tone: "danger", label: "proposal not recorded" },
  promotion_ready: { tone: "pending", label: "promotion ready" },
  /** The one kind the núcleo spells with a dot (`web.rs`). */
  "web.read": { tone: "info", label: "web page read" },
};

/** The reading for a kind, or `null` when this shell has none. */
export function readFeedKind(kind: string): FeedReading | null {
  return FEED_KINDS[kind.trim()] ?? null;
}

/** Every mapped kind, sorted, for the filter's `datalist`. */
export const FEED_KIND_NAMES: string[] = Object.keys(FEED_KINDS).sort();

/**
 * Why a `job_waiting` line is waiting, read out of its summary.
 *
 * The kind is one word for four different situations, and two of them ask for
 * opposite responses: a job held by the budget wants a ceiling raised, a job
 * behind a worktree slot wants you to wait or to stop something else. The
 * daemon does not put the reason in a column of its own — `park` writes
 * `job {id} is waiting: {detail}` and the detail is the only carrier — so it is
 * matched here, against the exact sentences `job.rs` and `budget.rs` build.
 *
 * The order matters. The exclusion detail is *"job 9 holds a slot and the two
 * are excluded"* and the slot detail is *"another run holds the project's
 * worktree slot"*; a naive match on "slot" would read the first as the second
 * and send somebody looking for capacity that is already there.
 *
 * The returned string is a `wait_reason` literal, so the badge comes from the
 * one non-collapsing map (`ui/state-map.ts`) rather than from this page.
 * `kill-switch` has no reading there, on purpose: it renders as itself.
 */
export function waitReasonFromSummary(summary: string): string | null {
  const text = summary.toLowerCase();
  if (text.includes("exclusion") || text.includes("excluded")) return "excluded";
  if (text.includes("worktree slot")) return "slot";
  if (text.includes("would exceed") || text.includes("budget check failed")) return "budget";
  if (text.includes("emergency stop")) return "kill-switch";
  return null;
}

export interface EfficiencyReading {
  /** Which detector spoke. */
  signal: string;
  /** What to go and look at. Never an instruction to stop anything. */
  cause: string;
}

/**
 * A `token_efficiency` line, read as the deferred observation it is.
 *
 * This is the one kind in the núcleo that travels through the waiting room by
 * design — `token_efficiency.rs` sends it via `deliver_or_defer` precisely
 * because it is *not* governance and can wait for the calendar. Rendering it
 * like a failure would invert that decision on screen: nothing is wrong, four
 * runs in a row simply looked the same way.
 *
 * The measure is the summary itself, which carries the numbers ("cache is cold"
 * without them is a claim nobody can check — `summarise`'s own comment). What
 * is added here is the signal's name and where to look. An unrecognised
 * phrasing returns `null` and the line keeps its summary and no invented cause.
 */
export function readEfficiencySignal(summary: string): EfficiencyReading | null {
  const text = summary.toLowerCase();
  if (text.includes("cached token")) {
    return {
      signal: "cold cache",
      cause: "a prompt this size should be cacheable — look for a prefix that changes between runs",
    };
  }
  if (text.includes("without handing off")) {
    return {
      signal: "context swelling",
      cause: "the window fills past the handoff mark and no successor is taken — look at the handoff path for this mode",
    };
  }
  if (text.includes("turns and ended")) {
    return {
      signal: "starved output",
      cause: "many turns and no result — look at whether the task is reachable with the tools these runs are given",
    };
  }
  if (text.includes("recent median")) {
    return {
      signal: "cost drift",
      cause: "this shape of run costs more than it recently did — compare it against the median in the line above",
    };
  }
  return null;
}

/* ── The notification selection policy ─────────────────────────────────────
 *
 * Which feed kinds still reach Telegram. The preference is stored in the
 * núcleo (`notify_policy.rs`) and RESOLVED in the sidecar; everything below is
 * presentation of those two, and decides nothing on its own.
 */

/** The body of `GET`/`PUT /notifications/policy`, as it is on the wire. */
export interface NotifyRule {
  selector: string;
  enabled: boolean;
}

export interface NotifyPolicy {
  families: NotifyRule[];
  kinds: NotifyRule[];
}

/**
 * The three states a kind can be in.
 *
 * `inherit` is the ABSENCE of a kind rule — and it is called that rather than
 * `family` because a loose kind uses it too, and a loose kind has no family to
 * inherit from. Naming it after the family would make the one case the word
 * does not cover the case somebody has to special-case.
 */
export type KindVerdict = "inherit" | "always" | "never";

export interface KindRow {
  kind: string;
  /**
   * Whether the núcleo has seen this kind in the feed's retention window (90
   * days). `false` means the row is here only because a stored rule names it —
   * shown, not hidden: it is a rule somebody wrote, and hiding it would leave
   * it silencing with nowhere to undo it.
   */
  recentlySeen: boolean;
  verdict: KindVerdict;
}

export interface FamilyRow {
  selector: string;
  /** `null` for a stored family `NOTIFY_FAMILIES` does not know — drawn by its literal prefix. */
  label: string | null;
  /** `null` is "no stored rule", which the resolution treats as passes. */
  rule: boolean | null;
  kinds: KindRow[];
}

/**
 * The one hand-written list in the whole design: a prefix and a human label per
 * family, and nothing else.
 *
 * It contains NO kinds. Which kind belongs to which family is computed by
 * prefix match, so this list cannot fall behind the núcleo the way `FEED_KINDS`
 * has — the worst it can do is leave a new family without a pretty name.
 *
 * One family is exactly one prefix. Two prefixes under one label would make a
 * single switch write two rules, make a state where the two disagree reachable,
 * and force the UI to draw "half on" — which is why `errand_` and `schedule_`
 * are two families and not one "errands and agenda".
 */
export const NOTIFY_FAMILIES: { selector: string; label: string }[] = [
  { selector: "job_", label: "jobs" },
  { selector: "run_", label: "runs" },
  { selector: "worktree_", label: "worktrees" },
  { selector: "vcs_", label: "git queue" },
  { selector: "council_", label: "council" },
  { selector: "errand_", label: "errands" },
  { selector: "schedule_", label: "agenda" },
  { selector: "email_", label: "e-mail" },
  { selector: "team_", label: "team" },
  { selector: "web.", label: "web" },
];

/**
 * Everything the notifications tab draws, resolved once.
 *
 * Takes the policy as well as the observed kinds because three of the four
 * things it produces cannot be derived from the kinds alone: families that
 * exist only as a stored rule (`label: null`), kinds that exist only as a
 * stored rule (`recentlySeen: false` — the union of §4.2, done here rather than
 * in the núcleo so that route stays a pure observation), and each switch's
 * state.
 *
 * The component does NOT read the policy again. A second read would be a second
 * resolution of the same rules, written somewhere else and free to disagree
 * with this one.
 *
 * Nothing here derives from `FEED_KINDS`. That table is missing every `team_`
 * kind and half the `vcs_` ones; building the screen on it would hide from the
 * owner exactly the kinds nobody remembered to add. `FEED_KINDS` is used for
 * one thing only, in the component: making a kind's label prettier, where
 * `readFeedKind` already falls back to the literal.
 */
export function groupKinds(
  observed: string[],
  policy: NotifyPolicy,
): { families: FamilyRow[]; loose: KindRow[] } {
  const kindRules = new Map<string, boolean>();
  for (const rule of policy.kinds) {
    if (rule.selector) kindRules.set(rule.selector, rule.enabled);
  }
  const familyRules = new Map<string, boolean>();
  for (const rule of policy.families) {
    if (rule.selector) familyRules.set(rule.selector, rule.enabled);
  }

  // The union of §4.2: what the machine wrote in the last ninety days, plus
  // whatever a stored rule names. Without the second half a rule written a year
  // ago keeps silencing with no row on screen to undo it.
  const seen = new Set(observed.filter((kind) => kind));
  const allKinds = new Set([...seen, ...kindRules.keys()]);

  // Families likewise: the known list, plus any prefix somebody stored that we
  // have no label for. An unknown one still works — it just draws as itself.
  const selectors = new Set([
    ...NOTIFY_FAMILIES.map((family) => family.selector),
    ...familyRules.keys(),
  ]);
  const labels = new Map(NOTIFY_FAMILIES.map((f) => [f.selector, f.label]));

  const verdictOf = (kind: string): KindVerdict => {
    const rule = kindRules.get(kind);
    if (rule === undefined) return "inherit";
    return rule ? "always" : "never";
  };
  const rowOf = (kind: string): KindRow => ({
    kind,
    recentlySeen: seen.has(kind),
    verdict: verdictOf(kind),
  });

  // Longest prefix wins, exactly as the sidecar resolves it — so a kind appears
  // under the family whose switch actually governs it, and not under a shorter
  // prefix that would be overruled.
  const familyOf = (kind: string): string | null => {
    let best: string | null = null;
    for (const selector of selectors) {
      if (!kind.startsWith(selector)) continue;
      if (best === null || selector.length > best.length) best = selector;
    }
    return best;
  };

  const byFamily = new Map<string, KindRow[]>();
  for (const selector of selectors) byFamily.set(selector, []);
  const loose: KindRow[] = [];
  for (const kind of [...allKinds].sort()) {
    const selector = familyOf(kind);
    if (selector === null) loose.push(rowOf(kind));
    else byFamily.get(selector)!.push(rowOf(kind));
  }

  // A family with no kinds is SHOWN with a count of zero, not hidden: "team: 0
  // kinds" says this machine has written none in ninety days, which is
  // information, where a missing row reads as a bug. Its switch still works —
  // a prefix matches future lines nobody has seen yet.
  const families: FamilyRow[] = [...selectors]
    .sort((a, b) => (labels.get(a) ?? a).localeCompare(labels.get(b) ?? b))
    .map((selector) => ({
      selector,
      label: labels.get(selector) ?? null,
      rule: familyRules.has(selector) ? familyRules.get(selector)! : null,
      kinds: byFamily.get(selector) ?? [],
    }));

  return { families, loose };
}
