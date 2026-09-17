// §spec mapa-do-projeto
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";
import { readState, statesOf, type StateReading } from "../ui/state-map";

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
  /**
   * What the line is about, when the núcleo knows: `job:<id>`, `run:<id>`, `council:<id>`,
   * `team_run:<id>`, `vcs:<id>` or `errand:<id>`. Every line about one subject is one sequence on
   * the Feed's trace — a job's start, its failed gate and its finish are one row, not three.
   */
  subject: string | null;
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

/* ------------------------------------------------------------- timeline -- */

/**
 * `GET /feed/timeline` — every scope, inside a time window, oldest first.
 *
 * The listing above answers "the newest fifty", which is the wrong question for a time axis: a
 * busy night fills fifty lines in an hour, and an axis drawn from them shows eleven hours of
 * silence that never happened. This route answers "everything between these two instants",
 * ordered `created_at` then `id`, capped at the newest {@link FEED_TIMELINE_CAP} with `truncated`
 * saying so out loud.
 */
export interface FeedTimeline {
  entries: FeedEntry[];
  /** More lines fell inside the window than the cap; the OLDEST were left out. */
  truncated: boolean;
}

/** The route's cap, in lines. A window holding more says `truncated`. */
export const FEED_TIMELINE_CAP = 5000;

/** The widest window the route accepts; anything wider is a 400. */
export const FEED_WINDOW_MAX_DAYS = 31;

/** A window of the axis. `until` absent is a live window, reaching up to now. */
export interface FeedWindow {
  /** RFC 3339. */
  since: string;
  until?: string;
}

/**
 * The daemon's seen marker — `GET` and `POST /feed/seen`.
 *
 * `through` is the newest line id somebody has been shown; `through_created_at` is when that
 * line was written, and `seen_at` is when it was marked. All three are `null` on a machine where
 * nothing has ever been marked. The marker lives in the daemon rather than in `localStorage`
 * because the Feed is not the only reader it will ever have, and because a second window would
 * otherwise keep its own idea of what you have seen.
 */
export interface FeedSeen {
  through: number | null;
  through_created_at: string | null;
  seen_at: string | null;
}

/** The query string for one window, with the incremental cursor when there is one. */
export function timelineQueryString(range: FeedWindow, afterId: number | null = null): string {
  const params = new URLSearchParams();
  params.set("since", range.since);
  if (range.until !== undefined) params.set("until", range.until);
  if (afterId !== null) params.set("after_id", String(afterId));
  return `?${params.toString()}`;
}

/** The newest id in a list, or `null` for an empty one. Ids only grow, so this is the cursor. */
export function newestFeedId(entries: FeedEntry[]): number | null {
  let newest: number | null = null;
  for (const entry of entries) if (newest === null || entry.id > newest) newest = entry.id;
  return newest;
}

/** The route's own order: `created_at`, then `id` for two lines written in the same instant. */
export function compareFeedEntries(a: FeedEntry, b: FeedEntry): number {
  const at = Date.parse(a.created_at) - Date.parse(b.created_at);
  return at !== 0 ? at : a.id - b.id;
}

/**
 * A poll's answer folded into what the page already holds.
 *
 * Deduplicated by id, because `after_id` is a cursor over ids while the route orders by time, and
 * a line the núcleo stamped a moment late can arrive twice across two polls. Lines that have
 * slid out of the window's start are NOT dropped here: a live window's `since` is fixed when the
 * window is chosen, so nothing slides. The cap is re-applied, and trimming to it is itself a
 * truncation — the flag is sticky, because once the oldest lines were left out they stay out.
 */
export function mergeFeedTimeline(previous: FeedTimeline | undefined, next: FeedTimeline): FeedTimeline {
  if (previous === undefined) return next;
  if (next.entries.length === 0) return previous;
  const byId = new Map<number, FeedEntry>();
  for (const entry of previous.entries) byId.set(entry.id, entry);
  for (const entry of next.entries) byId.set(entry.id, entry);
  const merged = [...byId.values()].sort(compareFeedEntries);
  const overflow = merged.length > FEED_TIMELINE_CAP;
  return {
    entries: overflow ? merged.slice(merged.length - FEED_TIMELINE_CAP) : merged,
    truncated: previous.truncated || next.truncated || overflow,
  };
}

/**
 * One window of the time axis, kept current.
 *
 * The first read is the whole window; every poll after it asks only for lines past the newest id
 * already held (`after_id`) and merges them in, so a seven-day window of four thousand lines costs
 * one large request per visit rather than one every three seconds. A different window is a
 * different key and so a full read — which is the "full refetch when the window changes" the page
 * wants, for free.
 *
 * `null` holds the query: the page does not know its window until the seen marker has answered,
 * and asking for a guessed window first would draw one axis and then redraw another.
 *
 * A window with an `until` is a question about the past and does not poll — the same rule
 * {@link useFeed} follows for a search.
 */
export function useFeedTimeline(range: FeedWindow | null) {
  const client = useQueryClient();
  const since = range?.since ?? "";
  const until = range?.until ?? null;
  const key = keys.feed.timeline(since, until);
  return useQuery({
    queryKey: key,
    queryFn: async () => {
      const held = client.getQueryData<FeedTimeline>(key);
      const after = until === null && held !== undefined ? newestFeedId(held.entries) : null;
      const answer = await apiFetch<FeedTimeline>(
        `/feed/timeline${timelineQueryString({ since, until: until ?? undefined }, after)}`,
      );
      return after === null ? answer : mergeFeedTimeline(held, answer);
    },
    enabled: range !== null,
    refetchInterval: until === null ? POLL.fast : false,
    // A new window keeps the old one on screen until its own answer lands, so choosing a preset
    // does not blank the chart. The caller clips what it draws to the window it asked for.
    placeholderData: keepPreviousData,
  });
}

/**
 * The seen marker, read once per visit.
 *
 * No poll: the page snapshots it on arrival and shades from that snapshot for the whole visit,
 * so a later value would be read by nobody. `staleTime: 0` and a fresh read on mount are what
 * make "the marker as it was when you came in" true on every visit rather than on the first.
 */
export function useFeedSeen() {
  return useQuery({
    queryKey: keys.feed.seen,
    queryFn: () => apiFetch<FeedSeen>("/feed/seen"),
    refetchOnMount: "always",
    refetchOnWindowFocus: false,
  });
}

/**
 * Move the marker forward to `through`.
 *
 * A plain function and not a mutation hook, because its commonest caller is an effect's cleanup —
 * the page leaving — where a hook's state has already been torn down. The daemon clamps it
 * monotonic and to the newest id it holds, so a late or repeated call can never move it back.
 */
export async function markFeedSeen(through: number): Promise<FeedSeen> {
  return await apiFetch<FeedSeen>("/feed/seen", {
    method: "POST",
    body: JSON.stringify({ through }),
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

/**
 * A feed kind's reading, which is the map's reading — the same shape it always was.
 *
 * The table this alias replaces lived here, in a `.ts` file, with 46 `tone:` literals in it,
 * and `ui/badge-authorship.test.ts` walked only `.tsx` and only `<Badge` tags. So the app's
 * largest tone table was invisible to the one test that exists to find exactly that, and it
 * stayed invisible long enough for five rows to drift into Acting Green. It is in
 * `ui/state-map.ts` now, and the ratchet walks `.ts` too.
 */
export type FeedReading = StateReading;

/** The reading for a kind, or `null` when this shell has none. */
export function readFeedKind(kind: string): FeedReading | null {
  return readState("feed", kind);
}

/** Every mapped kind, sorted, for the filter's `datalist`. */
export const FEED_KIND_NAMES: string[] = statesOf("feed").sort();

/**
 * Why a `job_waiting` line is waiting, read out of its summary.
 *
 * The kind is one word for five different situations, and two of them ask for
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
 * The disk detail is *"the disk is too full for another checkout: only … MiB
 * free …"*, and it is asked before the slot for the same kind of reason: it
 * names this project's worktrees, and until 2026-09-14 the daemon reported it
 * as slot contention outright, which sent a reader after a run that was not
 * there.
 *
 * The returned string is a `wait_reason` literal, so the badge comes from the
 * one non-collapsing map (`ui/state-map.ts`) rather than from this page.
 * `kill-switch` has no reading there, on purpose: it renders as itself.
 */
export function waitReasonFromSummary(summary: string): string | null {
  const text = summary.toLowerCase();
  if (text.includes("exclusion") || text.includes("excluded")) return "excluded";
  if (text.includes("disk is too full")) return "disk";
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
 * prefix match, so this list cannot fall behind the núcleo the way a table of
 * kinds can — the worst it can do is leave a new family without a pretty name.
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
 * Nothing here derives from the state map's `feed` domain. That table is held to
 * the núcleo by `state-map-completeness.test.ts`, but only for kinds this
 * checkout's source writes; the observed kinds come from the database, which
 * also holds kinds an older or newer daemon wrote. The map is used for one thing
 * only, in the component: making a kind's label prettier, where `readFeedKind`
 * already falls back to the literal.
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
