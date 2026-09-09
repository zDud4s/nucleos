// §spec mapa-do-projeto
import { keepPreviousData, useQuery } from "@tanstack/react-query";
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
