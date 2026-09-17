// §spec novo-frontend

import { useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { ChevronDown, ChevronRight } from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { isApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import { UI_LOCALE } from "../lib/locale";
import {
  FEED_KIND_NAMES,
  FEED_LIMIT_MAX,
  FEED_LIST_LIMIT,
  FEED_TIMELINE_CAP,
  boundToRfc3339,
  clampFeedLimit,
  feedIsSearching,
  markFeedSeen,
  newestFeedId,
  readEfficiencySignal,
  readFeedKind,
  rfc3339ToBoundInput,
  useFeed,
  useFeedSeen,
  useFeedTimeline,
  waitReasonFromSummary,
  type FeedEntry,
  type FeedFilters,
  type FeedSeen,
} from "../data/feed";
import { buildSequences, type FeedSequence } from "../lib/sequences";
import {
  QUIET_GAP,
  buildFeedDays,
  clock,
  dayHeading,
  lookedAtOf,
  resolveWindow,
  span,
  stamp,
  type FeedDay,
  type FeedPreset,
  type ResolvedWindow,
} from "../lib/timeline";
import { useProjects, type ProjectSummary } from "../data/system";
import {
  Badge,
  Button,
  ErrorNote,
  FEED_LANES,
  PageHeader,
  Quiet,
  RefusalNote,
  StaleNote,
  StateBadge,
  Teach,
  feedGravityOf,
  feedGravityTone,
  type FeedGravity,
} from "../ui";
import { FeedTrace, duration } from "./FeedTrace";
import "./feed.css";

/**
 * The feed: everything the núcleo did, drawn on the time it did it, with every line one gesture
 * away.
 *
 * Design §6.19 — *a feed entry **is** a notification*. There is no second channel and no toast:
 * this page and the drawer beside it are where a line goes. So the page answers the question a
 * person opens it with — *is everything fine since I looked?* — before it offers the record.
 *
 * Four layers, each an answer at a different depth. The verdict under the title names the window
 * and how many lines went wrong, were held or ask for you. The trace (`FeedTrace.tsx`) shows WHAT
 * RAN AND HOW IT ENDED: a row per job, run, council or errand under its lane, a bar from its first
 * line to its last, and a replay that walks the window back. The sequence selected in it opens
 * underneath, its own lines in order. The list at the bottom is the record — every line, grouped
 * by day, routine folded, every exception a full row.
 *
 * **What you have seen lives in the daemon, and moves when you leave.** On arrival the page
 * snapshots the marker and shades from it for the whole visit, so what was new when you came in
 * stays new while you read it. Leaving the route — or the window going hidden after you had it in
 * front of you — marks through the newest line the page actually showed. A search is a question
 * about the past and moves nothing.
 *
 * The filters still live in the **route**, like the run index's, and a search still freezes the
 * page: with any of `q` / `kind` / `since` / `until` / `limit` set the núcleo is answering about
 * the past, the poll stops, and the page says so. `project` and `errand` narrow the live view
 * without freezing it, as they always did.
 */

/** The seven filters, as the route spells them. */
export interface FeedSearch {
  q?: string;
  kind?: string;
  project?: string;
  errand?: string;
  since?: string;
  until?: string;
  limit?: string;
}

/**
 * What `/feed` accepts in its search params.
 *
 * Nothing is trusted, and two of the fields are checked harder than the rest:
 * `since` / `until` reach `parse_time_bound`, which answers **400** for
 * anything that is not RFC 3339, and `limit` reaches a `parse::<i64>` that
 * answers 400 for anything that is not a number. A location carrying junk in
 * either would take the whole page down to a refusal, so junk is dropped here
 * instead of being forwarded.
 */
export function validateFeedSearch(search: Record<string, unknown>): FeedSearch {
  return {
    q: searchText(search.q),
    kind: searchText(search.kind),
    project: searchText(search.project),
    errand: searchDigits(search.errand),
    since: searchInstant(search.since),
    until: searchInstant(search.until),
    limit: searchLimit(search.limit),
  };
}

function searchText(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed === "" ? undefined : trimmed;
}

/** An errand id is a row id, so anything that is not one is not a filter. */
function searchDigits(value: unknown): string | undefined {
  const text = searchText(typeof value === "number" ? String(value) : value);
  if (text === undefined) return undefined;
  return /^\d+$/.test(text) ? text : undefined;
}

function searchInstant(value: unknown): string | undefined {
  const text = searchText(value);
  if (text === undefined) return undefined;
  const at = new Date(text);
  return Number.isNaN(at.getTime()) ? undefined : at.toISOString();
}

function searchLimit(value: unknown): string | undefined {
  const text = searchText(typeof value === "number" ? String(value) : value);
  return text === undefined ? undefined : clampFeedLimit(text);
}

const PRESETS: { id: FeedPreset; label: string }[] = [
  { id: "seen", label: "Since you looked" },
  { id: "day", label: "24 h" },
  { id: "week", label: "7 days" },
];

/** The words for each exceptional gravity, singular and plural. Words only — the tone is the map's. */
const GRAVITY_WORDS: Record<Exclude<FeedGravity, "routine">, [string, string]> = {
  wrong: ["went wrong", "went wrong"],
  held: ["held", "held"],
  asks: ["asks for you", "ask for you"],
};
const EXCEPTIONS: Exclude<FeedGravity, "routine">[] = ["wrong", "held", "asks"];

export function Feed() {
  const search = useSearch({ strict: false }) as FeedSearch;
  const navigate = useNavigate();
  const client = useQueryClient();
  const filters: FeedFilters = {
    q: search.q,
    kind: search.kind,
    project: search.project,
    errand: search.errand,
    since: search.since,
    until: search.until,
    limit: search.limit,
  };
  const searching = feedIsSearching(filters);
  const projects = useProjects();

  function applyFilters(patch: Partial<FeedSearch>) {
    void navigate({ to: "/feed", search: validateFeedSearch({ ...filters, ...patch }) });
  }
  const backToLive = () =>
    applyFilters({ q: undefined, kind: undefined, since: undefined, until: undefined, limit: undefined });

  /* ----------------------------------------------------- the seen marker -- */

  const seen = useFeedSeen();
  /** The marker as it was when this visit began. `undefined` until the daemon has answered. */
  const [snapshot, setSnapshot] = useState<FeedSeen | null | undefined>(undefined);
  const [range, setRange] = useState<ResolvedWindow | null>(null);
  const [preset, setPreset] = useState<FeedPreset>("seen");

  useEffect(() => {
    if (snapshot !== undefined) return;
    // `isFetchedAfterMount`, not `data`: a cached marker from the last visit is the one this page
    // moved on its way out, and shading from it would call last visit's lines new.
    if (seen.isFetchedAfterMount && seen.data !== undefined) {
      setSnapshot(seen.data);
      setRange(resolveWindow(preset, Date.now(), seen.data));
    } else if (seen.isError) {
      // No marker to be had — an older núcleo, or its database. The window falls back to a day and
      // nothing is shaded, which is true: the page does not know what you have seen.
      setSnapshot(null);
      setRange(resolveWindow(preset, Date.now(), null));
    }
  }, [seen.isFetchedAfterMount, seen.data, seen.isError, snapshot, preset]);

  const snapshotRef = useRef(snapshot);
  snapshotRef.current = snapshot;

  /* ------------------------------------------------------------ the data -- */

  const timeline = useFeedTimeline(
    searching || range === null ? null : { since: new Date(range.start).toISOString() },
  );
  const found = useFeed(filters, { enabled: searching });
  /*
    Clipped to the window asked for: while a new window loads, the previous one is still the query's
    data, and a line from before the new start would be drawn piled against the axis's left edge.
  */
  const rangeStart = range?.start ?? null;
  const allEntries = useMemo(
    () =>
      (timeline.data?.entries ?? []).filter(
        (entry) => rangeStart === null || Date.parse(entry.created_at) >= rangeStart,
      ),
    [timeline.data, rangeStart],
  );

  const ownerFiltered = search.project !== undefined || search.errand !== undefined;
  const entries = useMemo(
    () => allEntries.filter((entry) => ownsLine(entry, search.project, search.errand)),
    [allEntries, search.project, search.errand],
  );

  /* ---------------------------------------- marking seen on the way out -- */

  const newestShown = useRef<number | null>(null);
  const hadVisible = useRef(false);
  const posted = useRef<number | null>(null);

  useEffect(() => {
    // Only a live view counts as having shown anything: a search is the past, and a line it
    // happened to return must not mark everything older than it as read.
    if (searching || timeline.data === undefined) return;
    const newest = newestFeedId(entries);
    if (newest !== null && (newestShown.current === null || newest > newestShown.current)) newestShown.current = newest;
    if (typeof document === "undefined" || document.visibilityState === "visible") hadVisible.current = true;
  }, [entries, searching, timeline.data]);

  const flushSeen = useCallback(() => {
    const through = newestShown.current;
    if (!hadVisible.current || through === null) return;
    const floor = Math.max(snapshotRef.current?.through ?? -1, posted.current ?? -1);
    if (through <= floor) return;
    posted.current = through;
    markFeedSeen(through).then(
      (answer) => client.setQueryData(keys.feed.seen, answer),
      () => {
        // The marker stays where it was, which is the safe side: a line shown again is not lost.
        posted.current = null;
      },
    );
  }, [client]);

  useEffect(() => {
    const onVisibility = () => {
      if (document.visibilityState === "hidden") flushSeen();
      else if (newestShown.current !== null) hadVisible.current = true;
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      flushSeen();
    };
  }, [flushSeen]);

  const newestAll = newestFeedId(allEntries);
  const unseenExists =
    !searching && newestAll !== null && (snapshot?.through === null || snapshot === null || newestAll > (snapshot?.through ?? -1));

  async function markEverythingSeen() {
    if (newestAll === null) return;
    posted.current = newestAll;
    try {
      const answer = await markFeedSeen(newestAll);
      client.setQueryData(keys.feed.seen, answer);
      setSnapshot(answer);
      if (preset === "seen") setRange(resolveWindow("seen", Date.now(), answer));
      setSaid("everything in this window is marked seen");
    } catch {
      posted.current = null;
      setSaid("the núcleo did not take the mark — nothing changed");
    }
  }

  /* --------------------------------------------------------- the trace -- */

  const sequences = useMemo(() => buildSequences(entries), [entries]);
  /** The tally pressed in the verdict, dimming every row that has no line of its gravity. */
  const [filter, setFilter] = useState<FeedGravity | null>(null);
  /**
   * The sequence open under the trace. `undefined` until somebody chooses — and until then it is
   * the newest one that went wrong, was held or asks for you, so the page opens on the thing most
   * likely to be the reason it was opened.
   */
  const [chosen, setChosen] = useState<string | null | undefined>(undefined);
  /** The replay's playhead, or `null` at rest on now. */
  const [at, setAt] = useState<number | null>(null);
  const [said, setSaid] = useState("");

  /** The newest sequence holding a line of `gravity` — or, with `null`, of any exceptional one. */
  const newestWith = (gravity: FeedGravity | null): FeedSequence | null => {
    let best: FeedSequence | null = null;
    for (const sequence of sequences) {
      const weighs =
        gravity === null
          ? sequence.gravity !== "routine"
          : sequence.lines.some((line) => feedGravityOf(line.kind) === gravity);
      if (weighs && (best === null || sequence.last > best.last)) best = sequence;
    }
    return best;
  };
  const selectedKey = chosen === undefined ? (newestWith(null)?.key ?? null) : chosen;
  const selected = sequences.find((sequence) => sequence.key === selectedKey) ?? null;

  function choosePreset(next: FeedPreset) {
    setPreset(next);
    setChosen(undefined);
    setFilter(null);
    setAt(null);
    if (snapshot !== undefined) setRange(resolveWindow(next, Date.now(), snapshot));
    if (searching) backToLive();
  }

  /* ------------------------------------------------ announcing new lines -- */

  const announcedThrough = useRef<{ start: number; id: number | null } | null>(null);
  useEffect(() => {
    if (searching || range === null || timeline.data === undefined) return;
    const newest = newestFeedId(entries);
    const last = announcedThrough.current;
    if (last === null || last.start !== range.start) {
      announcedThrough.current = { start: range.start, id: newest };
      return;
    }
    if (newest !== null && (last.id === null || newest > last.id)) {
      const fresh = entries.filter((entry) => last.id === null || entry.id > last.id).length;
      setSaid(fresh === 1 ? "1 new line" : `${fresh} new lines`);
      announcedThrough.current = { start: range.start, id: newest };
    }
  }, [entries, range, searching, timeline.data]);

  /* ------------------------------------------------------------- render -- */

  const now = useNow(searching ? found.data : timeline.data);
  const lookedAt = lookedAtOf(snapshot);
  const counts = gravityCounts(entries);

  const headline = searching
    ? searchHeadline(found.data, filters)
    : range === null || timeline.data === undefined
      ? undefined
      : (
          <Verdict
            window={windowSentence(range, now)}
            counts={counts}
            pressed={filter}
            onPress={(gravity) => {
              if (filter === gravity) {
                setFilter(null);
                return;
              }
              setFilter(gravity);
              const newest = newestWith(gravity);
              if (newest !== null) setChosen(newest.key);
            }}
          />
        );

  return (
    <>
      <PageHeader title="Feed" headline={headline} />

      <div className="feed-tools">
        <div className="ui-switch" role="group" aria-label="Window">
          {PRESETS.map((row) => (
            <button
              key={row.id}
              type="button"
              className="ui-switch-seg"
              aria-pressed={!searching && preset === row.id}
              onClick={() => choosePreset(row.id)}
            >
              {row.label}
            </button>
          ))}
        </div>
        {unseenExists && (
          <Button variant="quiet" onClick={() => void markEverythingSeen()}>
            Mark everything seen
          </Button>
        )}
        <FeedSearchForm filters={filters} projects={projects.data} onChange={applyFilters} />
      </div>

      <p className="sr-only" aria-live="polite">
        {said}
      </p>

      {searching ? (
        <SearchResults query={found} filters={filters} now={now} onFilter={applyFilters} onBack={backToLive} />
      ) : (
        <LiveView
          range={range}
          timeline={timeline}
          entries={entries}
          sequences={sequences}
          selected={selected}
          onSelect={setChosen}
          filter={filter}
          at={at}
          onAt={setAt}
          now={now}
          lookedAt={lookedAt}
          seenThrough={snapshot?.through ?? null}
          ownerFiltered={ownerFiltered}
          owner={search.project ?? (search.errand !== undefined ? `errand ${search.errand}` : null)}
          onFilter={applyFilters}
          onEveryScope={() => applyFilters({ project: undefined, errand: undefined })}
          onWiden={preset === "week" ? null : () => choosePreset("week")}
        />
      )}
    </>
  );
}

/* ---------------------------------------------------------------- verdict -- */

/**
 * The sentence under the title: the window, then what in it wants a look.
 *
 * Each count is a toggle on the trace — pressed, it dims every row without a line of that gravity
 * and opens the newest one that has one — so "2 went wrong" is answered by the rows it counts. A
 * count of zero is left out rather than said, and when all three are zero the sentence says so
 * plainly: "nothing went wrong" is the answer most visits come for, and it should not have to be
 * inferred from an absence.
 *
 * The small mark before each count is the key to the trace — the same solid mark, in the same
 * tone — so a reader learns what a red dot means from the sentence that counts them.
 */
function Verdict({
  window: windowText,
  counts,
  pressed,
  onPress,
}: {
  window: string;
  counts: Record<FeedGravity, number>;
  pressed: FeedGravity | null;
  onPress: (gravity: FeedGravity) => void;
}) {
  const parts: ReactNode[] = [];
  for (const gravity of EXCEPTIONS) {
    const n = counts[gravity];
    if (n === 0) continue;
    parts.push(
      <button
        key={gravity}
        type="button"
        className="feed-verdict-count"
        aria-pressed={pressed === gravity}
        onClick={() => onPress(gravity)}
      >
        <svg className="feed-verdict-key" width="10" height="10" aria-hidden="true">
          <circle className={`ui-mark-${feedGravityTone(gravity)}`} cx="5" cy="5" r="4" />
        </svg>
        <span className="feed-verdict-n">{n}</span> {GRAVITY_WORDS[gravity][n === 1 ? 0 : 1]}
      </button>,
    );
  }
  const total = counts.wrong + counts.held + counts.asks + counts.routine;
  return (
    <span className="feed-verdict">
      <span className="feed-verdict-window">{windowText}</span>
      {total === 0 ? (
        <Separated>
          <span>the machine wrote nothing</span>
        </Separated>
      ) : (
        <>
          {parts.length === 0 ? (
            <Separated>
              <span>nothing went wrong</span>
            </Separated>
          ) : (
            parts.map((part, index) => <Separated key={index}>{part}</Separated>)
          )}
          {counts.routine > 0 && (
            <Separated>
              <span>
                <span className="feed-verdict-n">{counts.routine}</span> routine
              </span>
            </Separated>
          )}
        </>
      )}
    </span>
  );
}

function Separated({ children }: { children: ReactNode }) {
  return (
    <>
      <span className="feed-verdict-dot" aria-hidden="true">
        ·
      </span>
      {children}
    </>
  );
}

/** How the window was arrived at, in words — including the clamps, which are never silent. */
function windowSentence(range: ResolvedWindow, now: number): string {
  const looked = range.lookedAt;
  switch (range.basis) {
    case "marker":
      return `Since you looked · ${stamp(looked ?? range.start, now)}`;
    case "min":
      return `Last 12 h · you looked at ${stamp(looked ?? now, now)}`;
    case "max":
      return `Last 7 days · you last looked ${dayHeading(looked ?? range.start, now).date}`;
    case "unmarked":
      return "Last 24 h · nothing marked seen yet";
    default:
      return range.preset === "week" ? "Last 7 days" : "Last 24 h";
  }
}

function searchHeadline(rows: FeedEntry[] | undefined, filters: FeedFilters): string | undefined {
  if (rows === undefined) return undefined;
  const what = filters.q !== undefined ? ` matching “${filters.q}”` : "";
  if (rows.length === 0) return `Search · nothing${what}`;
  return `Search · ${rows.length} ${rows.length === 1 ? "line" : "lines"}${what}`;
}

/* ------------------------------------------------------------------ search -- */

/**
 * One search field, and the rest of the filters behind "More".
 *
 * The field is what a person reaches for; kind, owner, dates and a limit are what a person
 * debugging reaches for, and they are one gesture away rather than seven controls on every visit.
 * They open by themselves when the route already carries one, so a shared link never hides the
 * filter that shaped it.
 *
 * Typed fields commit on Enter and chosen ones commit on change, which is the split a person
 * expects and also the one that matters here: every keystroke in the text box would otherwise be
 * a navigation *and* a new query key, and while you typed `worktree` the page would ask the
 * daemon eight questions and freeze itself on the first letter.
 *
 * The form is `display: contents`, so the field sits in the toolbar's row and the panel opens
 * across the whole width under it — while every control is still one form, submitted together.
 */
function FeedSearchForm({
  filters,
  projects,
  onChange,
}: {
  filters: FeedFilters;
  projects: ProjectSummary[] | undefined;
  onChange: (patch: Partial<FeedSearch>) => void;
}) {
  const panelId = useId();
  const carries =
    filters.kind !== undefined ||
    filters.project !== undefined ||
    filters.errand !== undefined ||
    filters.since !== undefined ||
    filters.until !== undefined ||
    filters.limit !== undefined;
  const [open, setOpen] = useState(carries);

  return (
    <form
      className="feed-search"
      onSubmit={(event) => {
        event.preventDefault();
        const form = new FormData(event.currentTarget);
        const read = (name: string) => {
          const value = form.get(name);
          return typeof value === "string" ? value : undefined;
        };
        onChange({ q: read("q"), kind: read("kind"), errand: read("errand"), limit: read("limit") });
      }}
    >
      <div className="feed-search-bar" role="search" aria-label="Search the feed">
        <input
          className="feed-search-input"
          name="q"
          type="search"
          defaultValue={filters.q ?? ""}
          key={filters.q ?? ""}
          placeholder="Search the feed"
          aria-label="Search the feed"
        />
        <button
          type="button"
          className="feed-search-more"
          aria-expanded={open}
          aria-controls={panelId}
          onClick={() => setOpen(!open)}
        >
          More
          <ChevronDown className="feed-search-chevron" size={14} aria-hidden="true" />
        </button>
      </div>

      <div className="feed-filters" id={panelId} hidden={!open}>
        <label className="feed-filter">
          <span>Kind</span>
          {/* A `datalist` and not a `select`: the núcleo writes kinds this shell
              has never heard of — `email_<class>` is configuration — so the list
              has to suggest without refusing anything. */}
          <input
            name="kind"
            list="feed-kind-options"
            defaultValue={filters.kind ?? ""}
            key={filters.kind ?? ""}
            placeholder="any kind"
            aria-label="Filter by kind"
          />
          <datalist id="feed-kind-options">
            {FEED_KIND_NAMES.map((kind) => (
              <option key={kind} value={kind} />
            ))}
          </datalist>
        </label>

        <label className="feed-filter">
          <span>Project</span>
          <select
            value={filters.project ?? ""}
            aria-label="Filter by project"
            /* The two owners are exclusive in the núcleo and in the route: a
               request naming both asks for rows that cannot exist. */
            onChange={(event) => onChange({ project: event.target.value, errand: undefined })}
          >
            <option value="">every scope</option>
            {(projects ?? []).map((project) => (
              <option key={project.project_id} value={project.project_id}>
                {project.project_id}
              </option>
            ))}
          </select>
        </label>

        <label className="feed-filter feed-filter-narrow">
          <span>Errand</span>
          <input
            name="errand"
            type="number"
            min={1}
            defaultValue={filters.errand ?? ""}
            key={filters.errand ?? ""}
            placeholder="id"
            aria-label="Filter by errand id"
          />
        </label>

        <label className="feed-filter">
          <span>Since</span>
          <input
            type="datetime-local"
            /** The browser's date placeholder follows the element language, for both bounds. */
            lang={UI_LOCALE}
            value={rfc3339ToBoundInput(filters.since)}
            aria-label="Only lines after"
            onChange={(event) => onChange({ since: boundToRfc3339(event.target.value) })}
          />
        </label>

        <label className="feed-filter">
          <span>Until</span>
          <input
            type="datetime-local"
            lang={UI_LOCALE}
            value={rfc3339ToBoundInput(filters.until)}
            aria-label="Only lines before"
            onChange={(event) => onChange({ until: boundToRfc3339(event.target.value) })}
          />
        </label>

        <label className="feed-filter feed-filter-narrow">
          <span>Limit</span>
          <input
            name="limit"
            type="number"
            min={1}
            max={FEED_LIMIT_MAX}
            defaultValue={filters.limit ?? ""}
            key={filters.limit ?? ""}
            placeholder={String(FEED_LIST_LIMIT)}
            aria-label="How many lines at most"
          />
        </label>

        <div className="feed-filter-submit">
          <Button type="submit">Search</Button>
        </div>
      </div>
    </form>
  );
}

function SearchResults({
  query,
  filters,
  now,
  onFilter,
  onBack,
}: {
  query: ReturnType<typeof useFeed>;
  filters: FeedFilters;
  now: number;
  onFilter: (patch: Partial<FeedSearch>) => void;
  onBack: () => void;
}) {
  const rows = query.data;
  const days = useMemo(
    () =>
      buildFeedDays(rows ?? [], {
        // A search answers with exactly the lines asked for, so none of them is folded away.
        isRoutine: () => false,
        seenThrough: null,
        lookedAt: null,
        gapAfter: Infinity,
      }),
    [rows],
  );

  return (
    <>
      <p className="feed-frozen">
        <span className="feed-frozen-text">
          This is a search, so the núcleo is answering about the past and the page has stopped
          refreshing.
        </span>
        <Button intent="go" onClick={onBack}>
          Back to live
        </Button>
      </p>

      {query.isError && rows !== undefined && <StaleNote dataUpdatedAt={query.dataUpdatedAt} />}
      {query.isError && rows === undefined && <FeedError error={query.error} />}
      {rows === undefined && !query.isError && <p className="feed-loading">searching the feed…</p>}
      {rows !== undefined && rows.length === 0 && (
        <Quiet says="Nothing matches this search. The núcleo has lines — none of them match; clear a filter to widen it." />
      )}
      {rows !== undefined && rows.length > 0 && (
        <>
          <FeedDays days={days} now={now} onFilter={onFilter} label="Search results" level={2} />
          <p className="feed-end">
            {rows.length === 1 ? "That is the one line" : `That is all ${rows.length} lines`} this search
            returned
            {filters.limit === undefined && rows.length >= FEED_LIST_LIMIT
              ? ` — a search stops at ${FEED_LIST_LIMIT} unless it is given a limit, up to ${FEED_LIMIT_MAX}`
              : ""}
            .
          </p>
        </>
      )}
    </>
  );
}

/* -------------------------------------------------------------------- live -- */

interface LiveViewProps {
  range: ResolvedWindow | null;
  timeline: ReturnType<typeof useFeedTimeline>;
  entries: FeedEntry[];
  sequences: FeedSequence[];
  selected: FeedSequence | null;
  onSelect: (key: string | null) => void;
  filter: FeedGravity | null;
  at: number | null;
  onAt: (at: number | null) => void;
  now: number;
  lookedAt: number | null;
  seenThrough: number | null;
  ownerFiltered: boolean;
  owner: string | null;
  onFilter: (patch: Partial<FeedSearch>) => void;
  onEveryScope: () => void;
  onWiden: (() => void) | null;
}

function LiveView(props: LiveViewProps) {
  const { range, timeline, entries, sequences, selected, now, lookedAt, seenThrough, ownerFiltered, owner } = props;
  const data = timeline.data;
  const empty = data !== undefined && entries.length === 0;
  /*
    Whether an empty window is a quiet machine or a machine that has never written a line. The
    listing is the cheapest way to ask — its newest fifty, any scope — and it is only asked while
    the window is empty and nothing narrows it, which is the only time the answer changes a word.
  */
  const history = useFeed({}, { enabled: empty && !ownerFiltered });
  const days = useMemo(
    () =>
      buildFeedDays(entries, {
        isRoutine: (entry) => feedGravityOf(entry.kind) === "routine",
        seenThrough,
        lookedAt,
        gapAfter: ownerFiltered ? Infinity : QUIET_GAP,
      }),
    [entries, seenThrough, lookedAt, ownerFiltered],
  );

  if (timeline.isError && data === undefined) return <FeedError error={timeline.error} />;
  if (range === null || data === undefined) return <p className="feed-loading">reading the feed…</p>;

  if (empty && !ownerFiltered && history.data !== undefined && history.data.length === 0) {
    return (
      <Teach
        title="Nothing has happened yet"
        action={
          <span className="feed-teach-actions">
            <Link to="/runs" className="feed-link">
              Start a run
            </Link>
            <Link to="/autopilot" className="feed-link">
              Turn on autopilot
            </Link>
          </span>
        }
      >
        <p>
          Every autonomous thing the núcleo does writes a line here: a job starting, a gate failing, a
          worktree released, an urgent e-mail. A feed line <em>is</em> the notification — nothing pops
          up anywhere else — and this page replays them on the time they happened.
        </p>
      </Teach>
    );
  }

  // The oldest line the list ends on. Taken as a minimum rather than the first element, so it does
  // not lean on the order lines arrive in.
  const oldest = entries.length > 0 ? Math.min(...entries.map((entry) => Date.parse(entry.created_at))) : null;
  const newestElsewhere = history.data?.[0];

  return (
    <>
      {timeline.isError && <StaleNote dataUpdatedAt={timeline.dataUpdatedAt} />}
      {data.truncated && (
        <p className="feed-truncated" role="status">
          This window holds more than {FEED_TIMELINE_CAP.toLocaleString(UI_LOCALE)} lines, so only the
          newest {FEED_TIMELINE_CAP.toLocaleString(UI_LOCALE)} are drawn. A narrower window shows the rest —
          24 h, or a search between two dates.
        </p>
      )}

      {ownerFiltered && (
        <p className="feed-narrowed">
          <span className="feed-narrowed-what">
            Showing only {owner ?? "one owner"} · <span className="feed-narrowed-n">{entries.length}</span>{" "}
            {entries.length === 1 ? "line" : "lines"}
          </span>
          <Button variant="quiet" onClick={props.onEveryScope}>
            Show every scope
          </Button>
        </p>
      )}

      {empty ? (
        <Quiet
          says={
            ownerFiltered
              ? `Nothing from ${owner ?? "this owner"} in this window.`
              : `The machine was quiet in this window — nothing wrote a line.${
                  newestElsewhere !== undefined
                    ? ` The last line was written ${stamp(Date.parse(newestElsewhere.created_at), now)}.`
                    : ""
                }`
          }
          action={props.onWiden === null ? undefined : <Button variant="quiet" onClick={props.onWiden}>Widen to 7 days</Button>}
        >
          <p>
            A job, a run or an errand would appear here as a row of its own, drawn from its first line to
            its last; a failure, held work or something that asks for you as a solid mark on it and a
            full row in the list below.
          </p>
        </Quiet>
      ) : (
        <>
          <FeedTrace
            sequences={sequences}
            start={range.start}
            now={now}
            title={traceTitle(range)}
            at={props.at}
            onAt={props.onAt}
            selected={selected?.key ?? null}
            onSelect={props.onSelect}
            filter={props.filter}
          />

          {selected !== null && <SequenceDetail sequence={selected} time={props.at ?? now} now={now} />}

          <h2 className="feed-every">Every line</h2>
          <FeedDays days={days} now={now} onFilter={props.onFilter} label="Lines in this window" level={3} />
          <p className="feed-end">
            {data.truncated
              ? `That is the newest ${FEED_TIMELINE_CAP.toLocaleString(UI_LOCALE)} lines in this window.`
              : "That is everything in this window."}
            {oldest !== null && <span className="feed-end-oldest">oldest line · {stamp(oldest, now)}</span>}
            {props.onWiden !== null && (
              <Button variant="quiet" onClick={props.onWiden}>
                Widen to 7 days
              </Button>
            )}
          </p>
        </>
      )}
    </>
  );
}

/** The trace's name for its window: the preset, as the reader chose it. */
function traceTitle(range: ResolvedWindow): string {
  switch (range.basis) {
    case "marker":
    case "min":
      return "Since you looked";
    case "max":
      return "Last 7 days";
    case "unmarked":
      return "Last 24 h";
    default:
      return range.preset === "week" ? "Last 7 days" : "Last 24 h";
  }
}

/* ----------------------------------------------------------------- detail -- */

/**
 * The sequence selected in the trace, read line by line.
 *
 * Its own lines in the order they were written, with the kind badged only where the line was an
 * exception — the routine ones say their kind in muted words, so the badge column is a column of
 * the moments that mattered. While the replay stands in the past, the lines not yet written are
 * faded rather than hidden: the detail is where a person reads what happened next.
 *
 * Beside it, the facts a row cannot hold: whose it was, how long it took, how many attempts, and
 * the one door to where it can be acted on.
 */
function SequenceDetail({ sequence, time, now }: { sequence: FeedSequence; time: number; now: number }) {
  const headingId = useId();
  const lane = FEED_LANES.find((row) => row.id === sequence.lane)?.label ?? sequence.lane;
  const n = sequence.lines.length;
  const newest = sequence.lines[n - 1];
  const until = n > 1 || sequence.open ? ` → ${sequence.open ? "still open" : stamp(sequence.last, now)}` : "";

  return (
    <section className="feed-detail" aria-labelledby={headingId}>
      <div className="feed-detail-main">
        <h2 className="feed-detail-title" id={headingId}>
          <span className="feed-detail-name">{sequence.name}</span>
          <KindBadge kind={newest.kind} />
        </h2>
        <p className="feed-detail-sub">
          {lane} · {n} {n === 1 ? "line" : "lines"} · {stamp(sequence.start, now)}
          {until}
        </p>
        <ol className="feed-detail-lines">
          {sequence.lines.map((line) => {
            const written = Date.parse(line.created_at);
            const routine = feedGravityOf(line.kind) === "routine";
            return (
              <li key={line.id} className={written > time ? "feed-detail-line feed-detail-later" : "feed-detail-line"}>
                <time className="feed-detail-time" dateTime={line.created_at}>
                  {clock(written)}
                </time>
                <span className="feed-detail-kind">
                  {routine ? (
                    <span className="feed-detail-kind-words">{readFeedKind(line.kind)?.label ?? line.kind}</span>
                  ) : (
                    <KindBadge kind={line.kind} />
                  )}
                </span>
                <span className="feed-detail-summary">{line.summary}</span>
              </li>
            );
          })}
        </ol>
      </div>
      <aside className="feed-detail-side" aria-label={`About ${sequence.name}`}>
        <dl className="feed-detail-facts">
          <dt>Owner</dt>
          <dd className="feed-detail-mono">{sequence.owner ?? "the machine itself"}</dd>
          <dt>{sequence.family === "series" ? "Lines" : "Lasted"}</dt>
          <dd>{sequenceOpenPhrase(sequence, now)}</dd>
          {sequence.attempts !== null && (
            <>
              <dt>Attempts</dt>
              <dd className="feed-detail-mono">{sequence.attempts}</dd>
            </>
          )}
          <SequenceDoor sequence={sequence} />
        </dl>
      </aside>
    </section>
  );
}

function sequenceOpenPhrase(sequence: FeedSequence, now: number): string {
  const said = duration(sequence, now);
  return sequence.open ? `${said} so far` : said;
}

/** Where the sequence can be acted on, when that place exists — the same doors a line carries. */
function SequenceDoor({ sequence }: { sequence: FeedSequence }) {
  const asks = [...sequence.lines].reverse().find((line) => line.kind === "promotion_ready" || line.kind === "email_urgent");
  const run = sequence.family === "run" ? Number(sequence.key.slice(4)) : ([...sequence.lines].reverse().find((line) => line.run_id !== null)?.run_id ?? null);
  const door =
    asks !== undefined ? (
      <LineLink entry={asks} />
    ) : run !== null && Number.isFinite(run) ? (
      <Link to={`/runs/${run}`} className="feed-link feed-link-run">
        run {run}
      </Link>
    ) : null;
  if (door === null) return null;
  return (
    <>
      <dt>Go</dt>
      <dd>{door}</dd>
    </>
  );
}

/* -------------------------------------------------------------------- list -- */

function FeedDays({
  days,
  now,
  onFilter,
  label,
  level,
}: {
  days: FeedDay[];
  now: number;
  onFilter: (patch: Partial<FeedSearch>) => void;
  label: string;
  /** A day heading's rank: under "Every line" in the live view, the page's own sections in a search. */
  level: 2 | 3;
}) {
  const Heading = level === 2 ? "h2" : "h3";
  return (
    <div className="feed-days" aria-label={label} role="region">
      {days.map((day) => {
        const heading = dayHeading(day.at, now);
        const headingId = `feed-day-${day.key}`;
        return (
          <section key={day.key} className="feed-day" aria-labelledby={headingId}>
            <Heading className="feed-day-head" id={headingId}>
              <span className="feed-day-title">{heading.title}</span>
              <span className="feed-day-date">{heading.date}</span>
            </Heading>
            <ol className="feed-day-lines">
              {day.items.map((item) => {
                switch (item.type) {
                  case "line":
                    return (
                      <FeedLine key={item.entry.id} entry={item.entry} onFilter={onFilter} />
                    );
                  case "routine":
                    return (
                      <RoutineFold key={item.key} entries={item.entries} onFilter={onFilter} />
                    );
                  case "gap":
                    return (
                      <li key={`gap-${item.from}`} className="feed-gap">
                        quiet for {spokenSpan(item.to - item.from)} · {clock(item.from)} → {clock(item.to)}
                      </li>
                    );
                  case "seen":
                    return (
                      <li key="seen" className="feed-seen">
                        <span className="feed-seen-text">
                          you looked here{item.at === null ? "" : ` · ${stamp(item.at, now)}`}
                        </span>
                      </li>
                    );
                }
              })}
            </ol>
          </section>
        );
      })}
    </div>
  );
}

/**
 * Consecutive routine lines, folded into one row.
 *
 * The kinds are named in the fold so a reader can decide whether to open it without opening it —
 * "5 routine · job started, worktree released" says what kind of ordinary it was.
 */
function RoutineFold({
  entries,
  onFilter,
}: {
  entries: FeedEntry[];
  onFilter: (patch: Partial<FeedSearch>) => void;
}) {
  const [shown, setOpen] = useState(false);
  const bodyId = useId();
  const kinds = [...new Set(entries.map((entry) => readFeedKind(entry.kind)?.label ?? entry.kind))];
  const named = kinds.length > 4 ? `${kinds.slice(0, 4).join(", ")} and ${kinds.length - 4} more` : kinds.join(", ");
  const newest = Date.parse(entries[0].created_at);

  return (
    <li className="feed-routine">
      <time className="feed-line-time" dateTime={entries[0].created_at}>
        {clock(newest)}
      </time>
      <button
        type="button"
        className="feed-routine-toggle"
        aria-expanded={shown}
        aria-controls={bodyId}
        onClick={() => setOpen(!shown)}
      >
        <ChevronRight className="feed-routine-chevron" size={14} aria-hidden="true" />
        <span className="feed-routine-n">{entries.length} routine</span>
        <span className="feed-routine-kinds">{named}</span>
      </button>
      <ol className="feed-routine-lines" id={bodyId} hidden={!shown}>
        {shown &&
          entries.map((entry) => (
            <FeedLine key={entry.id} entry={entry} onFilter={onFilter} />
          ))}
      </ol>
    </li>
  );
}

function FeedLine({
  entry,
  onFilter,
}: {
  entry: FeedEntry;
  onFilter: (patch: Partial<FeedSearch>) => void;
}) {
  const waiting = entry.kind === "job_waiting" ? waitReasonFromSummary(entry.summary) : null;
  const efficiency = entry.kind === "token_efficiency" ? readEfficiencySignal(entry.summary) : null;
  const routine = feedGravityOf(entry.kind) === "routine";
  return (
    <li className={routine ? "feed-line feed-line-routine" : "feed-line"}>
      <time className="feed-line-time" dateTime={entry.created_at}>
        {clock(Date.parse(entry.created_at))}
      </time>
      <div className="feed-line-kind">
        <KindBadge kind={entry.kind} />
        {/* One kind, five situations — and budget and slot contention ask for
            opposite answers. The reading comes from the one non-collapsing map
            rather than from this page. */}
        {waiting !== null && <StateBadge domain="wait_reason" state={waiting} />}
      </div>
      <div className="feed-line-body">
        <p className="feed-line-summary">{entry.summary}</p>
        <p className="feed-line-meta">
          <Owner entry={entry} onFilter={onFilter} />
          <LineLink entry={entry} />
        </p>
        {efficiency !== null && (
          <p className="feed-line-observation">
            <span className="feed-line-signal">{efficiency.signal}</span>
            <span className="feed-line-cause">{efficiency.cause}</span>
          </p>
        )}
      </div>
    </li>
  );
}

/**
 * Where a line can be acted on, when that place exists.
 *
 * A run is a route of its own. The two lines that ask something of you each have a page where
 * the asking is answered — a promotion is decided in Autopilot, an urgent e-mail is read in Mail —
 * and a row that named the question without the door would send somebody hunting for it.
 */
function LineLink({ entry }: { entry: FeedEntry }) {
  if (entry.kind === "promotion_ready") {
    return (
      <Link to="/autopilot" className="feed-link">
        Review in Autopilot
      </Link>
    );
  }
  if (entry.kind === "email_urgent") {
    return (
      <Link to="/mail" className="feed-link">
        Open Mail
      </Link>
    );
  }
  if (entry.run_id !== null) {
    return (
      <Link to={`/runs/${entry.run_id}`} className="feed-link feed-link-run">
        run {entry.run_id}
      </Link>
    );
  }
  return null;
}

/**
 * The kind, or the kind's own literal.
 *
 * The same posture as `ui/StateBadge`: a kind this shell has no reading for is
 * shown as itself through the shared `ui-state-unmapped` device, with the ignorance said out loud in the
 * tooltip. The núcleo grows kinds faster than the table does, and a plausible
 * guess would be the shell asserting a meaning it does not have.
 *
 * Exported because the drawer is the feed's second surface and must not answer
 * the same question differently. It is not a design system primitive — it knows
 * about feed kinds specifically — so it stays with the slice that owns them
 * rather than moving into `ui/`.
 *
 * **A badge does not restate its row.** It survived the 2026-09-09 critique, which saw twelve
 * pills over twelve sentences saying the same thing, and the reason is that the restatement was
 * the FIXTURE's: its summaries had been written from these labels, while the núcleo writes ids,
 * paths and branches (`worktree.rs:1327`, `vcs.rs:2475`, `job.rs:4522`). With the fixture in the
 * núcleo's shapes the pill carries the one thing the sentence does not — the class, which is also
 * this page's filter facet (`FEED_KIND_NAMES` feeds the `kind` datalist) and the only carrier of
 * the tone that separates "went wrong" from "happened". `preview/fixtures.test.ts` holds the rule
 * as a property: no fixture row's summary may equal its badge's label.
 */
export function KindBadge({ kind }: { kind: string }) {
  const reading = readFeedKind(kind);
  if (reading !== null) {
    return (
      <Badge tone={reading.tone} title={kind}>
        {reading.label}
      </Badge>
    );
  }
  return (
    <Badge tone="off" className="ui-state-unmapped" title={`this shell has no reading for the feed kind "${kind}"`}>
      {kind}
    </Badge>
  );
}

/**
 * Whose line this is, and a way to see only theirs.
 *
 * A filter and not a link, because that is what the row's ids can honestly
 * carry: `errand_id` and `project_id` narrow *this* feed through the route,
 * while a deep link would have to invent a destination — the shell has no
 * errand detail route and no job page at all. `run_id` is the exception and it
 * *is* a link: `/runs/{id}` exists.
 *
 * Neither owner set is the machine acting by itself (`Global` in the núcleo:
 * `project_id IS NULL AND errand_id IS NULL`), which is a fact worth stating
 * rather than an empty cell.
 */
function Owner({
  entry,
  onFilter,
}: {
  entry: FeedEntry;
  onFilter: (patch: Partial<FeedSearch>) => void;
}) {
  if (entry.project_id !== null) {
    return (
      <button
        type="button"
        className="feed-line-owner"
        aria-label={`Show only ${entry.project_id}`}
        onClick={() => onFilter({ project: entry.project_id ?? undefined, errand: undefined })}
      >
        {entry.project_id}
      </button>
    );
  }
  if (entry.errand_id !== null) {
    return (
      <button
        type="button"
        className="feed-line-owner"
        aria-label={`Show only errand ${entry.errand_id}`}
        onClick={() => onFilter({ errand: String(entry.errand_id), project: undefined })}
      >
        errand {entry.errand_id}
      </button>
    );
  }
  return <span className="feed-line-owner-none">the machine itself</span>;
}

/* ------------------------------------------------------------------ errors -- */

function FeedError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          bad_request:
            "the núcleo would not accept that question — a date bound must be a full timestamp, a limit a number, and a window at most 31 days",
          internal: "the núcleo could not read the feed — its database, not your search",
        }}
      />
    );
  }
  return <ErrorNote>the núcleo did not answer — nothing is known about the feed</ErrorNote>;
}

/* ----------------------------------------------------------------- helpers -- */

/** `4 h 05 min` in the list, where there is room for the unit the axis leaves off. */
function spokenSpan(ms: number): string {
  const said = span(ms);
  return /h \d\d$/.test(said) ? `${said} min` : said;
}

function ownsLine(entry: FeedEntry, project: string | undefined, errand: string | undefined): boolean {
  if (project !== undefined) return entry.project_id === project;
  if (errand !== undefined) return entry.errand_id !== null && String(entry.errand_id) === errand;
  return true;
}

function gravityCounts(entries: FeedEntry[]): Record<FeedGravity, number> {
  const counts: Record<FeedGravity, number> = { wrong: 0, held: 0, asks: 0, routine: 0 };
  for (const entry of entries) counts[feedGravityOf(entry.kind)] += 1;
  return counts;
}

/**
 * The clock the axis's right edge and the "now" rule read, advanced every half minute.
 *
 * A poll re-renders the page every three seconds while lines arrive, but a quiet night renders
 * nothing — and a "now" rule frozen at the moment the page opened would draw the silence since as
 * not having happened yet.
 */
function useNow(pulse: unknown): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);
  // And whenever new data lands, so a mark written a second ago is never drawn right of "now".
  useEffect(() => setNow(Date.now()), [pulse]);
  return now;
}
