// §spec novo-frontend

import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  FEED_KIND_NAMES,
  FEED_LIMIT_MAX,
  FEED_LIST_LIMIT,
  boundToRfc3339,
  clampFeedLimit,
  feedIsFiltered,
  feedIsSearching,
  readEfficiencySignal,
  readFeedKind,
  rfc3339ToBoundInput,
  useFeed,
  waitReasonFromSummary,
  type FeedEntry,
  type FeedFilters,
} from "../data/feed";
import { useProjects, type ProjectSummary } from "../data/system";
import {
  Badge,
  Button,
  ErrorNote,
  PageHeader,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  StaleNote,
  StateBadge,
  Teach,
} from "../ui";
import "./feed.css";

/**
 * The feed: everything the núcleo did, in the order it did it.
 *
 * Design §6.19 — *a feed entry **is** a notification*. There is no second
 * channel and no toast: this page and the drawer beside it are where a line
 * goes, which is why the page is a record you can search rather than a stream
 * that scrolls past.
 *
 * The filters live in the **route**, like the run index's, so a narrowed feed
 * can be returned to and linked to. And the page has two modes rather than one,
 * because the daemon does: with none of the five search fields set it is a
 * listing and it polls; with any of them set the núcleo is answering a question
 * about the past, so the poll stops and the page says so. *Back to live* is the
 * way out — it clears the five and leaves the project or the errand alone,
 * since narrowing to an owner never froze anything.
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

export function Feed() {
  const search = useSearch({ strict: false }) as FeedSearch;
  const navigate = useNavigate();
  const filters: FeedFilters = {
    q: search.q,
    kind: search.kind,
    project: search.project,
    errand: search.errand,
    since: search.since,
    until: search.until,
    limit: search.limit,
  };

  const entries = useFeed(filters);
  const projects = useProjects();
  const searching = feedIsSearching(filters);
  const rows = entries.data;
  /**
   * The list on screen is no longer the daemon's. A query that succeeded once
   * keeps its rows when a later refetch fails, which is what this page wants —
   * a feed that blanks reads as *nothing has happened*.
   */
  const stale = entries.isError && rows !== undefined;

  function applyFilters(patch: Partial<FeedSearch>) {
    void navigate({ to: "/feed", search: validateFeedSearch({ ...filters, ...patch }) });
  }

  return (
    <>
      <PageHeader title="Feed" headline={headline(rows, filters, searching)} />

      <FeedFilterBar filters={filters} projects={projects.data} onChange={applyFilters} />

      {searching && (
        <p className="feed-frozen">
          <span className="feed-frozen-text">
            This is a search, so the núcleo is answering about the past and the page has stopped
            refreshing.
          </span>
          <Button
            intent="go"
            onClick={() =>
              applyFilters({
                q: undefined,
                kind: undefined,
                since: undefined,
                until: undefined,
                limit: undefined,
              })
            }
          >
            Back to live
          </Button>
        </p>
      )}

      {stale && <StaleNote dataUpdatedAt={entries.dataUpdatedAt} />}
      {entries.isError && rows === undefined && <FeedError error={entries.error} />}

      <FeedList
        rows={rows}
        filtered={feedIsFiltered(filters)}
        searching={searching}
        onFilter={applyFilters}
      />
    </>
  );
}

/* -------------------------------------------------------------- filters -- */

/**
 * The filters, bound to the route.
 *
 * Typed fields commit on Enter and chosen ones commit on change, which is the
 * split a person expects and also the one that matters here: every keystroke in
 * the text box would otherwise be a navigation *and* a new query key, and while
 * you typed `worktree` the page would ask the daemon eight questions and freeze
 * itself on the first letter.
 */
function FeedFilterBar({
  filters,
  projects,
  onChange,
}: {
  filters: FeedFilters;
  projects: ProjectSummary[] | undefined;
  onChange: (patch: Partial<FeedSearch>) => void;
}) {
  return (
    <form
      className="feed-filters"
      role="search"
      aria-label="Filter the feed"
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
      <label className="feed-filter feed-filter-text">
        <span>Contains</span>
        <input name="q" defaultValue={filters.q ?? ""} key={filters.q ?? ""} aria-label="Search the feed" />
      </label>

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
          value={rfc3339ToBoundInput(filters.since)}
          aria-label="Only lines after"
          onChange={(event) => onChange({ since: boundToRfc3339(event.target.value) })}
        />
      </label>

      <label className="feed-filter">
        <span>Until</span>
        <input
          type="datetime-local"
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

      <Button type="submit">Search</Button>
    </form>
  );
}

/* ----------------------------------------------------------------- list -- */

function FeedList({
  rows,
  filtered,
  searching,
  onFilter,
}: {
  rows: FeedEntry[] | undefined;
  filtered: boolean;
  searching: boolean;
  onFilter: (patch: Partial<FeedSearch>) => void;
}) {
  if (rows === undefined) return <p className="feed-loading">reading the feed…</p>;

  if (rows.length === 0) {
    return (
      <Teach title={filtered ? "Nothing matches those filters" : "The feed is empty"}>
        {filtered ? (
          <p>
            The núcleo has lines, but none of them match. Clear the filters above — an empty
            filtered feed is not a quiet machine.
          </p>
        ) : (
          <p>
            Every autonomous thing the núcleo does writes a line here: a job starting, a gate
            failing, a worktree released, a notification the calendar held back. A feed entry{" "}
            <em>is</em> a notification — nothing pops up elsewhere instead.
          </p>
        )}
      </Teach>
    );
  }

  return (
    <>
      {/* Hairline-ruled and not a column of cards: the feed is read by scanning
          down it, not by picking lines out of it — the argument `.ui-rows` now
          carries for all four lists that had grown it byte for byte. The label
          is what a screen reader gets instead of the rules, which are not
          announced. */}
      <Rows label="Feed">
        {rows.map((entry) => (
          <FeedRow key={entry.id} entry={entry} onFilter={onFilter} />
        ))}
      </Rows>
      {!searching && rows.length >= FEED_LIST_LIMIT && (
        <p className="feed-ceiling">
          showing the newest {FEED_LIST_LIMIT} — a listing is capped at that; search with a date or
          a limit to reach further back
        </p>
      )}
    </>
  );
}

function FeedRow({
  entry,
  onFilter,
}: {
  entry: FeedEntry;
  onFilter: (patch: Partial<FeedSearch>) => void;
}) {
  const waiting = entry.kind === "job_waiting" ? waitReasonFromSummary(entry.summary) : null;
  const efficiency = entry.kind === "token_efficiency" ? readEfficiencySignal(entry.summary) : null;

  return (
    /* The default stacking layout, not `layout="line"`: a line here is a head,
       a summary and sometimes an observation under one another. The one-baseline
       feed is the cockpit's `.ap-feed-line`, which is a different list. */
    <Row>
      <div className="feed-row-head">
        <KindBadge kind={entry.kind} />
        {/* One kind, five situations — and budget and slot contention ask for
            opposite answers. The reading comes from the one non-collapsing map
            rather than from this page. */}
        {waiting !== null && <StateBadge domain="wait_reason" state={waiting} />}
        <Owner entry={entry} onFilter={onFilter} />
        {entry.run_id !== null && (
          <Link to={`/runs/${entry.run_id}`} className="feed-row-run">
            run {entry.run_id}
          </Link>
        )}
        <RelativeTime at={entry.created_at} />
      </div>

      <p className="feed-row-summary">{entry.summary}</p>

      {efficiency !== null && (
        <p className="feed-row-observation">
          <span className="feed-row-signal">{efficiency.signal}</span>
          <span className="feed-row-cause">{efficiency.cause}</span>
        </p>
      )}
    </Row>
  );
}

/**
 * The kind, or the kind's own literal.
 *
 * The same posture as `ui/StateBadge`: a kind this shell has no reading for is
 * shown as itself in the neutral tone, with the ignorance said out loud in the
 * tooltip. The núcleo grows kinds faster than the table does, and a plausible
 * guess would be the shell asserting a meaning it does not have.
 *
 * Exported because the drawer is the feed's second surface and must not answer
 * the same question differently. It is not a design system primitive — it knows
 * about feed kinds specifically — so it stays with the slice that owns them
 * rather than moving into `ui/`.
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
    <Badge tone="info" title={`this shell has no reading for the feed kind "${kind}"`}>
      {kind}
    </Badge>
  );
}

/**
 * Whose line this is, and a way to see only theirs.
 *
 * A filter and not a link, because that is what the row's ids can honestly
 * carry: `errand_id` and `project_id` narrow *this* feed through the route the
 * daemon already accepts, while a deep link would have to invent a destination
 * — the shell has no errand detail route and no job page at all. `run_id` is
 * the exception and it *is* a link: `/runs/{id}` exists.
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
        className="feed-row-owner"
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
        className="feed-row-owner"
        aria-label={`Show only errand ${entry.errand_id}`}
        onClick={() => onFilter({ errand: String(entry.errand_id), project: undefined })}
      >
        errand {entry.errand_id}
      </button>
    );
  }
  return <span className="feed-row-owner-none">the machine itself</span>;
}

/* -------------------------------------------------------------- errors -- */

function FeedError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          bad_request:
            "the núcleo would not accept that search — a date bound must be a full timestamp and a limit must be a number",
          internal: "the núcleo could not read the feed — its database, not your search",
        }}
      />
    );
  }
  return <ErrorNote>the núcleo did not answer — nothing is known about the feed</ErrorNote>;
}

/** One derived sentence about what this list is showing. */
function headline(
  rows: FeedEntry[] | undefined,
  filters: FeedFilters,
  searching: boolean,
): string | undefined {
  if (rows === undefined) return undefined;
  const scope = feedIsFiltered(filters) ? "matching these filters" : "across every scope";
  const mode = searching ? "searched" : "live";
  if (rows.length === 0) return `nothing ${scope}`;
  return `${rows.length} ${scope} — ${mode}`;
}
