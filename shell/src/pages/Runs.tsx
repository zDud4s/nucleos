import { useEffect, useId, useRef, useState, type ReactNode, type RefObject } from "react";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { DAEMON_NAME } from "../copy";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  RUN_MODE_FILTERS,
  RUN_STATUSES,
  runIsAlive,
  useCreateRun,
  useRunPages,
  useRuns,
  type RUN_MODES,
  type RunFilters,
  type RunSearchResult,
} from "../data/runs";
import {
  useCreatePreset,
  useDeletePreset,
  usePresets,
  useRunPreset,
  type Preset,
} from "../data/presets";
import { routeTriple } from "../data/route";
import { dayHeading, dayKey } from "../lib/timeline";
import { useKillSwitch, useProjects, type ProjectSummary } from "../data/system";
import {
  Button,
  ConfirmButton,
  Disclosure,
  ErrorNote,
  Field,
  ListFilter,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  Section,
  Skeleton,
  RelativeTime,
  Row,
  Rows,
  money,
  readState,
  StaleNote,
  StateBadge,
  Teach,
} from "../ui";
import "./runs.css";

/**
 * The run index: what has run, what is running, and the two ways to start one.
 *
 * The filters live in the **route**, not in component state, and that is the
 * one structural decision on this page. A filtered list whose filters are local
 * state cannot be linked to, cannot be returned to after opening a run, and
 * loses itself on every remount — and the shell's whole posture is that where
 * you are is a fact about the app rather than about a component that happens to
 * still be mounted. The query key is built from the same object the route
 * validated, so the URL and the cache entry cannot disagree.
 *
 * The list is the page. Starting a run is a gesture made now and then, so it
 * lives behind the header's **New run** rather than in a column that sat beside
 * the list permanently and weighed more than it — an empty form was the heaviest
 * thing on a screen whose job is to say what has happened.
 */

/** The four filters, as the route spells them, and whether chat turns are shown. */
export interface RunSearch {
  project?: string;
  status?: string;
  mode?: string;
  q?: string;
  /**
   * `show` puts conversation turns back in the list. Absent hides them — the
   * owner's decision of 2026-10-11: a turn per message would drown the work
   * that runs on its own, while the runs a chat starts stay, tagged with it.
   */
  chats?: "show";
}

/**
 * What `/runs` accepts in its search params.
 *
 * Nothing is trusted: this runs on whatever is in the location, which on a
 * desktop shell is whatever the last navigation wrote and, one day, whatever a
 * deep link carries. A non-string is dropped rather than coerced, and a blank
 * is dropped too — `?status=` would ask the daemon for runs whose status is the
 * empty string and get back a list that reads on screen as *there are no runs*.
 */
export function validateRunSearch(search: Record<string, unknown>): RunSearch {
  return {
    project: searchText(search.project),
    status: searchText(search.status),
    mode: searchText(search.mode),
    q: searchText(search.q),
    chats: search.chats === "show" ? "show" : undefined,
  };
}

function searchText(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed === "" ? undefined : trimmed;
}

export function Runs() {
  const search = useSearch({ strict: false }) as RunSearch;
  const navigate = useNavigate();
  const filters: RunFilters = {
    project: search.project,
    status: search.status,
    mode: search.mode,
    q: search.q,
  };

  // Kept out of `filters`, which is what "Clear filters" and the empty state
  // read: hiding the turns is the page's default, not a filter somebody set.
  // Asking for the `assistant` mode is asking for turns, so it shows them.
  const showChats = search.chats === "show" || search.mode === "assistant";
  const runs = useRunPages({
    ...filters,
    group: "chat",
    chatTurns: showChats ? undefined : "hide",
  });
  const projects = useProjects();
  const [composing, setComposing] = useState(false);
  const panelId = useId();
  const openerId = useId();

  /**
   * The list on screen is no longer the daemon's.
   *
   * A query that succeeded once keeps its rows when a later refetch fails, which
   * is what this page wants — a list that blanks reads as *nothing has run*.
   * The note is how the page says the rows are old. A failed "Load older" is
   * not that — the rows on screen are current — and says so beside its button.
   */
  const stale = runs.isRefetchError && runs.data !== undefined;
  const rows = runs.data === undefined ? undefined : joinPages(runs.data.pages);
  const older: OlderPages = {
    more: runs.hasNextPage,
    loading: runs.isFetchingNextPage,
    failed: runs.isFetchNextPageError,
    load: () => void runs.fetchNextPage(),
  };

  function applyFilters(patch: Partial<RunSearch>) {
    void navigate({
      to: "/runs",
      search: validateRunSearch({ ...filters, chats: search.chats, ...patch }),
    });
  }

  // Focus goes back to the control that opened the panel, which is the standard
  // the project switcher set: a panel that closes and drops focus on `<body>`
  // leaves a keyboard user at the top of the document.
  function closeComposer() {
    setComposing(false);
    document.getElementById(openerId)?.focus();
  }

  const started = (id: number) => void navigate({ to: `/runs/${id}` });

  return (
    <>
      <PageHeader
        title="Runs"
        // Plain text, not a live region: the index refetches on a timer and a
        // region around this sentence read every changed count aloud. What is
        // announced is the failure itself, through the stale and error notes
        // below, which carry their own roles.
        headline={headline(rows, filters, older.more)}
        actions={
          <Button
            id={openerId}
            intent="go"
            aria-expanded={composing}
            aria-controls={panelId}
            onClick={() => setComposing(!composing)}
          >
            New run
          </Button>
        }
      />

      <NewRunPanel
        id={panelId}
        open={composing}
        projects={projects.data}
        onClose={closeComposer}
        onStarted={started}
      />

      <RunFilterBar
        filters={filters}
        showChats={showChats}
        projects={projects.data}
        counts={rows === undefined ? undefined : scopeCounts(rows, older.more)}
        onChange={applyFilters}
      />

      {stale && <StaleNote dataUpdatedAt={runs.dataUpdatedAt} />}
      {runs.isError && rows === undefined && (
        <ListError error={runs.error} onRetry={() => void runs.refetch()} />
      )}

      {(rows !== undefined || !runs.isError) && (
        <RunList
          rows={rows}
          older={older}
          filtered={isFiltered(filters)}
          filters={filters}
          onCompose={composing ? undefined : () => setComposing(true)}
        />
      )}
    </>
  );
}

/* -------------------------------------------------------------- filters -- */

/**
 * The status segments a person reaches for: what is moving, what awaits approval,
 * what went wrong. With none on, every run shows. `failed` alone and not
 * "failed, timed out or interrupted": `GET /runs` binds `status` to exactly one
 * value (`core/src/runs.rs`, `search` — `AND status = ?`), so a segment that
 * promised three would be a request the daemon cannot answer. The exact
 * statuses are one disclosure away.
 */
const SCOPES: readonly { id: string; label: string }[] = [
  { id: "running", label: "Running" },
  { id: "awaiting_approval", label: "Awaiting approval" },
  { id: "failed", label: "Failed" },
];

const SCOPE_STATUSES = new Set(SCOPES.map((scope) => scope.id));

/** How long the search box rests before it is written to the route. */
const SEARCH_DEBOUNCE_MS = 350;

/**
 * What each status segment holds, as the text beside its label.
 *
 * Read off the rows already loaded and not asked of the daemon: `GET /runs` has
 * no count route, and a second query per segment for a figure beside a label is
 * not worth a poll. So it is a reading of *this list* — it follows the search
 * and the project, it is the loaded page's, and a `+` says an older page may
 * hold more. Zero is left unsaid: an empty segment is just its label, and while
 * another status is pressed the other segments are empty by construction, not
 * because nothing ran. A conversation counts as running when any turn of it is
 * (the same reading its state cell gives), so the number matches the rows.
 */
function scopeCounts(rows: RunSearchResult[], more: boolean): Record<string, string> {
  const counts: Record<string, string> = {};
  for (const scope of SCOPES) {
    const n = rows.filter((row) => stateOf(row) === scope.id).length;
    if (n > 0) counts[scope.id] = more ? `${n}+` : String(n);
  }
  return counts;
}

/**
 * The filters, bound to the route, on one line.
 *
 * The search box (a `ListFilter`, which owns `/` and Escape), then the status
 * scope as one joined control, then **More filters** pinned to the far end. The
 * scope is `ModeSwitch`'s own track and segment (`.ui-switch`), with
 * `aria-pressed` as a chip has: none pressed is "all". Pressing a segment and
 * the selects commit on change; the text commits once typing rests, since a
 * navigation per keystroke would put a history entry behind every letter.
 * Project, mode, the other statuses and the chat turns sit behind one
 * `Disclosure`, open on arrival when any of them is in force.
 *
 * Below 60rem the groups wrap whole — the search takes a line, the scope and the
 * disclosure share the next — and never mid-group (`runs.css`).
 */
function RunFilterBar({
  filters,
  showChats,
  projects,
  counts,
  onChange,
}: {
  filters: RunFilters;
  showChats: boolean;
  projects: ProjectSummary[] | undefined;
  /** Per-status text beside each segment; absent while the list is loading. */
  counts: Record<string, string> | undefined;
  onChange: (patch: Partial<RunSearch>) => void;
}) {
  // Open on arrival when a filter it holds is in force: a project, a mode, chat
  // turns shown, or a status the chips have no entry for. Otherwise the list
  // would be narrowed by a control nobody can see.
  const otherStatus =
    filters.status !== undefined && !SCOPE_STATUSES.has(filters.status)
      ? filters.status
      : undefined;
  const needsMore =
    filters.project !== undefined ||
    filters.mode !== undefined ||
    showChats ||
    otherStatus !== undefined;
  const [more, setMore] = useState(needsMore);
  // Adjusted during render rather than in an effect, so there is no frame in
  // which a filter that just came into force is hidden.
  const [sawNeed, setSawNeed] = useState(needsMore);
  if (needsMore !== sawNeed) {
    setSawNeed(needsMore);
    if (needsMore) setMore(true);
  }

  // The box is typed into locally and written to the route when it rests; the
  // route stays the source of truth, so a change that comes from elsewhere
  // (Clear, back button) replaces the draft.
  const routeQ = filters.q ?? "";
  const [draft, setDraft] = useState(routeQ);
  const [sawQ, setSawQ] = useState(routeQ);
  if (routeQ !== sawQ) {
    setSawQ(routeQ);
    setDraft(routeQ);
  }
  useEffect(() => {
    if (draft.trim() === routeQ) return;
    const timer = setTimeout(() => onChange({ q: draft }), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
    // `onChange` is a fresh closure every render; the draft and the route's
    // value are what decide whether a write is due.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft, routeQ]);

  function clearAll() {
    setDraft("");
    onChange({ project: undefined, status: undefined, mode: undefined, q: undefined });
  }

  // Every filter in force besides the text, so the search's Clear says how many
  // it will turn off: it is `clearAll`, which also drops a project and a mode.
  // (One Clear, here; the old second "Clear filters" for a project or a mode on
  // its own is no longer needed.)
  const inForce = [filters.status, filters.project, filters.mode].filter(
    (value): value is string => value !== undefined,
  );

  return (
    <div className="runs-bar">
      <div className="runs-search">
        <ListFilter
          noun="runs"
          query={draft}
          onQueryChange={setDraft}
          selected={inForce}
          onClear={clearAll}
        />
      </div>

      <div className="ui-switch runs-scope" role="group" aria-label="Status">
        {SCOPES.map((scope) => (
          <button
            key={scope.id}
            type="button"
            className="ui-switch-seg"
            aria-pressed={filters.status === scope.id}
            onClick={() => onChange({ status: filters.status === scope.id ? undefined : scope.id })}
          >
            {scope.label}
            {counts?.[scope.id] !== undefined && (
              <>
                {" "}
                <span className="ui-count">{counts[scope.id]}</span>
              </>
            )}
          </button>
        ))}
      </div>

      {otherStatus !== undefined && (
        // A status the chips cannot show, kept in sight and removable, so the
        // list is never narrowed by something that is not on screen.
        <Button
          variant="quiet"
          aria-label={`Remove status filter: ${readState("run", otherStatus)?.label ?? otherStatus}`}
          onClick={() => onChange({ status: undefined })}
        >
          status: {readState("run", otherStatus)?.label ?? otherStatus}{" "}
          <span aria-hidden="true">×</span>
        </Button>
      )}

      <Disclosure label="More filters" open={more} onOpenChange={setMore}>
        <div className="runs-more">
          <Field label="Project">
            <select
              value={filters.project ?? ""}
              onChange={(event) => onChange({ project: event.target.value })}
            >
              <option value="">any project</option>
              {(projects ?? []).map((project) => (
                <option key={project.project_id} value={project.project_id}>
                  {project.project_id}
                </option>
              ))}
            </select>
          </Field>

          <Field label="Mode">
            {/* `assistant` is offered here and not in the new-run form: the daemon
                writes that mode, so runs carry it, but nobody may ask for one. */}
            <select
              value={filters.mode ?? ""}
              onChange={(event) => onChange({ mode: event.target.value })}
            >
              <option value="">any mode</option>
              {RUN_MODE_FILTERS.map((mode) => (
                <option key={mode} value={mode}>
                  {mode}
                </option>
              ))}
            </select>
          </Field>

          <Field label="Other status">
            {/* Only the statuses the segments above do not already offer, in the
                badge's own words from the one table: choosing "failed" has one
                place to be done, not two. */}
            <select
              value={otherStatus ?? ""}
              onChange={(event) => onChange({ status: event.target.value })}
            >
              <option value="">none</option>
              {RUN_STATUSES.filter((status) => !SCOPE_STATUSES.has(status)).map((status) => (
                <option key={status} value={status}>
                  {readState("run", status)?.label ?? status}
                </option>
              ))}
            </select>
          </Field>

          <label className="runs-check">
            <input
              type="checkbox"
              checked={showChats}
              disabled={filters.mode === "assistant"}
              onChange={(event) =>
                onChange({ chats: event.target.checked ? "show" : undefined })
              }
            />
            Show chat turns
          </label>
        </div>
      </Disclosure>
    </div>
  );
}

function isFiltered(filters: RunFilters): boolean {
  return Object.values(filters).some(
    (value) => value !== undefined && value !== "",
  );
}

/* ----------------------------------------------------------------- list -- */

/** The index's "Load older" state, read off the paged query. */
interface OlderPages {
  /** The last page came back full, so an older one may exist. */
  more: boolean;
  loading: boolean;
  /** The last "Load older" failed; the pages already on screen are still current. */
  failed: boolean;
  load: () => void;
}

/**
 * The loaded pages as one list. Each row once, by id: the pages are contiguous
 * by construction (keyset cursor, refetched in order), and this only keeps a
 * row that a poll moved between two pages from being drawn twice in between.
 */
function joinPages(pages: RunSearchResult[][]): RunSearchResult[] {
  const seen = new Set<number>();
  const rows: RunSearchResult[] = [];
  for (const page of pages) {
    for (const row of page) {
      if (seen.has(row.id)) continue;
      seen.add(row.id);
      rows.push(row);
    }
  }
  return rows;
}

function RunList({
  rows,
  older,
  filtered,
  filters,
  onCompose,
}: {
  rows: RunSearchResult[] | undefined;
  older: OlderPages;
  filtered: boolean;
  filters: RunFilters;
  /** Opens the New run panel; absent while it is already open. */
  onCompose: (() => void) | undefined;
}) {
  if (rows === undefined) return <Skeleton label="Loading runs" lines={3} />;

  if (rows.length === 0) {
    return (
      <Teach
        title={filtered ? "Nothing matches those filters" : "No runs yet"}
        action={
          !filtered && onCompose !== undefined ? (
            <Button intent="go" onClick={onCompose}>
              New run
            </Button>
          ) : undefined
        }
      >
        {filtered ? (
          <p>
            The {DAEMON_NAME} has runs, but none of them match. Clear the filters
            above to see the whole index — an empty filtered list is not an empty
            machine.
          </p>
        ) : (
          <p>
            A run is one task handed to the AI tool. Start one with New run, or
            save a request as a preset and start it from there. Jobs and the
            autopilot create runs too, and they appear here alongside the ones
            you ask for.
          </p>
        )}
      </Teach>
    );
  }

  // Attention first, then history. A run that waits on a person and a run that
  // is still moving are the two things on this page that can change under the
  // reader's eyes or need them, so they lift out of the pile into rows of their
  // own, and the table under them is what has already happened, by day.
  //
  // Only on the unfiltered index. A filter is an answer to one question, and
  // splitting its answer into "Needs you / Moving / the rest" would answer a
  // different one: pressing Failed wants the failures in one table, and a search
  // wants its matches in one. So a filtered list is the history table alone —
  // still by day, since the day is still the thing that dates a row — and every
  // run, whatever its state, is a row of it. Nothing is ever drawn in both.
  const split = filtered ? { waiting: [], moving: [], rest: rows } : partition(rows);
  const attention = split.waiting.length + split.moving.length > 0;
  const now = Date.now();

  return (
    <div className="runs-index">
      {split.waiting.length > 0 && (
        <Section label="Needs you">
          <Rows label="Runs awaiting approval">
            {split.waiting.map((row) => runRowOf(row, filters, "attention"))}
          </Rows>
        </Section>
      )}
      {split.moving.length > 0 && (
        <Section label="Moving">
          <Rows label="Runs still moving">
            {split.moving.map((row) => runRowOf(row, filters, "attention"))}
          </Rows>
        </Section>
      )}
      {split.rest.length > 0 &&
        (attention ? (
          <Section label="History">
            <HistoryTable rows={split.rest} filters={filters} now={now} level={3} />
          </Section>
        ) : (
          <HistoryTable rows={split.rest} filters={filters} now={now} level={2} />
        ))}
      {older.failed ? (
        <ErrorNote onRetry={older.load} retryLabel="Load older">
          Could not load older runs — the ones above are still current.
        </ErrorNote>
      ) : (
        older.more && (
          <div className="runs-older">
            <Button
              onClick={older.load}
              disabled={older.loading}
              aria-busy={older.loading}
            >
              {older.loading ? "Loading older…" : "Load older"}
            </Button>
            <span className="runs-older-count">
              showing the newest {rows.length}
            </span>
          </div>
        )
      )}
    </div>
  );
}

/**
 * The state a row is *read* as: a conversation with a turn still running is
 * running, whatever its latest turn settled as (the same reading `ConversationRow`
 * draws its badge from), so the segments' counts, the attention region and the
 * badge cannot disagree about one row.
 */
function stateOf(row: RunSearchResult): string {
  return isConversation(row) && (row.running_turns ?? 0) > 0 ? "running" : row.status;
}

function isConversation(row: RunSearchResult): boolean {
  return row.chat_id != null && row.turns != null;
}

/**
 * The unfiltered index split by what it asks of the reader: runs awaiting
 * approval, runs moving, and the rest. Order within each is the daemon's
 * (newest first), which `Array.filter` keeps.
 */
function partition(rows: RunSearchResult[]): {
  waiting: RunSearchResult[];
  moving: RunSearchResult[];
  rest: RunSearchResult[];
} {
  const waiting: RunSearchResult[] = [];
  const moving: RunSearchResult[] = [];
  const rest: RunSearchResult[] = [];
  for (const row of rows) {
    const state = stateOf(row);
    if (state === "awaiting_approval") waiting.push(row);
    else if (runIsAlive(state)) moving.push(row);
    else rest.push(row);
  }
  return { waiting, moving, rest };
}

/** How a row is laid out: a line of the table, a richer line of the attention region, or a turn nested in a conversation. */
type RowKind = "table" | "attention" | "turn";

function rowClass(kind: RowKind): string {
  return kind === "table"
    ? "runs-row runs-row-table"
    : kind === "attention"
      ? "runs-row runs-row-attn"
      : "runs-row runs-row-compact";
}

function runRowOf(row: RunSearchResult, filters: RunFilters, kind: RowKind): ReactNode {
  return isConversation(row) ? (
    <ConversationRow key={`chat-${row.chat_id}`} row={row} filters={filters} kind={kind} />
  ) : (
    <RunRow key={row.id} row={row} kind={kind} />
  );
}

/** Consecutive rows of one local calendar day, under the heading that dates them. */
interface DayGroup {
  key: string;
  title: string;
  date: string;
  rows: RunSearchResult[];
}

/**
 * Groups the list by the day each run was created. Consecutive, not bucketed:
 * the daemon sends newest first, so a day is already contiguous, and a bucket
 * map would only reorder the day a polled row slid across midnight into. A
 * timestamp that does not parse gets a group of its own rather than a heading
 * that says "Invalid Date".
 */
function groupByDay(rows: RunSearchResult[], now: number): DayGroup[] {
  const groups: DayGroup[] = [];
  for (const row of rows) {
    const at = Date.parse(row.created_at);
    const key = Number.isNaN(at) ? "undated" : dayKey(at);
    const last = groups[groups.length - 1];
    if (last?.key === key) {
      last.rows.push(row);
      continue;
    }
    const heading = Number.isNaN(at) ? { title: "Undated", date: "" } : dayHeading(at, now);
    groups.push({ key, ...heading, rows: [row] });
  }
  return groups;
}

/**
 * The history: one column header, then a box of rows per day.
 *
 * The header and every row share one grid (`--runs-cols`), which is the point of
 * the redesign: the old rows were two-line blocks whose title spanned the
 * columns above it, so the header named columns the rows did not have. Now
 * each row's cells sit in the header's columns, and the run id is the second
 * line of the title cell, where it is a handle for the run named above it.
 *
 * `level` is the day heading's rank: under the "History" section when there is an
 * attention region above it, directly under the page's title when there is not.
 */
function HistoryTable({
  rows,
  filters,
  now,
  level,
}: {
  rows: RunSearchResult[];
  filters: RunFilters;
  now: number;
  level: 2 | 3;
}) {
  const Heading = level === 3 ? "h3" : "h2";
  const idPrefix = useId();
  return (
    <div className="runs-history">
      <RunsHead />
      {groupByDay(rows, now).map((day) => {
        const headingId = `${idPrefix}-${day.key}`;
        return (
          <section key={day.key} className="runs-day" aria-labelledby={headingId}>
            <Heading className="runs-day-head" id={headingId}>
              <span className="runs-day-title">{day.title}</span>
              {day.date !== "" && <span className="runs-day-date">{day.date}</span>}
              <span className="ui-count">{day.rows.length}</span>
            </Heading>
            <Rows label={`Runs, ${day.title}${day.date === "" ? "" : ` ${day.date}`}`}>
              {day.rows.map((row) => runRowOf(row, filters, "table"))}
            </Rows>
          </section>
        );
      })}
    </div>
  );
}

/**
 * Names the table's columns once, above the first day.
 *
 * Presentation only (`aria-hidden`): each cell carries a `title` for the same
 * word. Below the width where the columns collapse the header is hidden and the
 * cells name themselves with a short prefix instead (`runs.css`).
 */
function RunsHead() {
  return (
    <div className="runs-head" aria-hidden="true">
      <div className="runs-head-grid">
        <span>State</span>
        <span>Run</span>
        <span>Project</span>
        <span>Mode</span>
        <span>Model</span>
        <span className="runs-head-end">When</span>
        <span className="runs-head-end">Cost</span>
      </div>
    </div>
  );
}

/**
 * The state cell, the title cell and the facts cell(s) every row is made of.
 *
 * The facts are one wrapper so that the same row is two layouts without a second
 * markup: `display: contents` hands each fact to the table grid when the row is
 * wide, and the wrapper is a wrapping line under the title when it is not
 * (`runs.css`).
 */
function RunCells({
  state,
  main,
  facts,
}: {
  state: ReactNode;
  main: ReactNode;
  facts: ReactNode;
}) {
  return (
    <>
      <span className="runs-row-state">{state}</span>
      <div className="runs-row-main">{main}</div>
      <div className="runs-row-facts">{facts}</div>
    </>
  );
}

/** One run as a row: also what a conversation's turns are drawn with (`kind="turn"`). */
function RunRow({ row, kind }: { row: RunSearchResult; kind: RowKind }) {
  // What ran is a cell of the facts, not a line of its own: it is a fact every
  // routed row has, and a third line under every row made the whole list a
  // third taller to say it.
  const ran = routeTriple([row.model, row.effort]);
  const cost = costOf(row);
  return (
    <Row className={rowClass(kind)}>
      <RunCells
        state={<RunState status={row.status} />}
        main={
          <>
            <Link to={`/runs/${row.id}`} className="runs-row-name">
              {row.prompt_excerpt}
            </Link>
            <p className="runs-row-sub">
              <span className="runs-row-id" title="Run">run {row.id}</span>
              {row.launched_by_chat_id != null && (
                <Link to={`/chats/${row.launched_by_chat_id}`} className="runs-row-origin">
                  from chat {row.launched_by_chat_title ?? row.launched_by_chat_id}
                </Link>
              )}
            </p>
            <RouteAdvice row={row} />
          </>
        }
        facts={
          <>
            <span className="runs-row-project" title="Project">
              {row.project_id ?? "no project"}
            </span>
            <span className="runs-row-mode" data-mode={row.mode} title="Mode">{row.mode}</span>
            {ran !== null && <span className="runs-row-ran" title="Model">{ran}</span>}
            <When at={row.created_at} since={kind === "attention"} />
            <span
              className="runs-row-cost"
              title="Cost"
              data-unrecorded={cost === "cost not recorded" ? "" : undefined}
            >
              {cost}
            </span>
          </>
        }
      />
    </Row>
  );
}

/**
 * The time cell: the bare relative time, or "started …" on the attention rows,
 * where it is how long the run has been going and not a column of dates.
 */
function When({ at, since }: { at: string; since: boolean }) {
  if (!since) return <RelativeTime at={at} />;
  return (
    <span className="runs-row-when">
      started <RelativeTime at={at} />
    </span>
  );
}

/**
 * One row per conversation, its turns one click away (owner's open question 5).
 * The row is the folded `GET /runs?group=chat` row; the turns are not fetched
 * until it is opened.
 */
function ConversationRow({
  row,
  filters,
  kind,
}: {
  row: RunSearchResult;
  filters: RunFilters;
  kind: RowKind;
}) {
  const [open, setOpen] = useState(false);
  const turnsId = useId();
  const chatId = row.chat_id ?? "";
  const turns = row.turns ?? 0;
  const running = row.running_turns ?? 0;
  const name = row.chat_title ?? row.prompt_excerpt;
  return (
    <Row className={rowClass(kind)}>
      <RunCells
        state={<RunState status={stateOf(row)} />}
        main={
          <>
            <Link to={`/chats/${chatId}`} className="runs-row-name">
              {name}
            </Link>
            <p className="runs-row-sub">
              <span className="runs-row-id">{turns === 1 ? "1 turn" : `${turns} turns`}</span>
              {running > 0 && <span className="runs-row-running">{`${running} running`}</span>}
              <span className="runs-row-toggle">
                <Button
                  variant="quiet"
                  aria-expanded={open}
                  aria-controls={turnsId}
                  onClick={() => setOpen(!open)}
                >
                  {open ? "Hide turns" : "Show turns"}
                </Button>
              </span>
            </p>
          </>
        }
        facts={
          <>
            <span className="runs-row-project">{row.project_id ?? "no project"}</span>
            <span className="runs-row-mode" data-mode="chat">chat</span>
            <When at={row.created_at} since={kind === "attention"} />
            <span className="runs-row-cost" data-unrecorded={row.cost_usd === null ? "" : undefined}>
              {row.cost_usd === null ? "cost not recorded" : money(row.cost_usd)}
            </span>
          </>
        }
      />
      <div id={turnsId} className="runs-turns" hidden={!open}>
        {open && <ConversationTurns chatId={chatId} filters={filters} name={name} total={turns} />}
      </div>
    </Row>
  );
}

/**
 * The turns of one conversation, fetched with the page's own filters so the
 * count on the row and the list under it agree.
 */
function ConversationTurns({
  chatId,
  filters,
  name,
  total,
}: {
  chatId: string;
  filters: RunFilters;
  name: string;
  total: number;
}) {
  const turns = useRuns({ ...filters, chat: chatId });
  if (turns.data === undefined) {
    return turns.isError ? <ListError error={turns.error} /> : <Quiet says="reading the turns…" />;
  }
  return (
    <>
      <Rows label={`Turns of ${name}`}>
        {turns.data.map((turn) => (
          <RunRow key={turn.id} row={turn} kind="turn" />
        ))}
      </Rows>
      {total > turns.data.length && (
        <p className="runs-ceiling">
          showing the newest {turns.data.length} of {total} turns
        </p>
      )}
    </>
  );
}

/**
 * Where a shadow router disagreed with what ran.
 *
 * What ran is already a column of the row (`RunRow`), so this is only the extra
 * line under the title, and only when the advice differs: a matching one is the
 * normal case and would be a line on every row. The "→" prefix and the italic
 * (see `.runs-row-route-advice`) are what set it apart from the facts beside it
 * now that it is no longer a quieter *colour* — faint text failed the contrast
 * floor.
 *
 * The router's report across all runs lives on System → Health, not here: it
 * ignores this page's filters and serves a config decision (shadow to apply, per
 * surface, in `router.yaml`) rather than reading the list.
 */
function RouteAdvice({ row }: { row: RunSearchResult }) {
  const differs =
    row.route_mode === "shadow" &&
    routeTriple([row.advised_runner, row.advised_model, row.advised_effort]) !== null &&
    (row.advised_runner !== row.runner || row.advised_model !== row.model || row.advised_effort !== row.effort);
  if (!differs) return null;
  const advised = routeTriple([
    row.advised_runner !== row.runner ? row.advised_runner : null,
    row.advised_model,
    row.advised_effort,
  ]);
  return (
    <p className="runs-row-route">
      <span className="runs-row-route-advice">{`→ router: ${advised}`}</span>
    </p>
  );
}

/**
 * A row's state, and the one state that gets no badge.
 *
 * Exceptions dominate and the normal recedes (PRODUCT.md, principle 2). A run
 * that completed is what nearly every row is, and a column of blue pills made
 * the failure among them one more pill. So `completed` is the map's own word in
 * quiet text, and the badge column is coloured only where something is
 * happening or went wrong. The map itself is untouched — other surfaces show a
 * single run, where its outcome is the point.
 */
function RunState({ status }: { status: string }) {
  if (status === "completed") {
    return (
      <span className="runs-row-settled">
        {readState("run", "completed")?.label ?? status}
      </span>
    );
  }
  return <StateBadge domain="run" state={status} />;
}

/**
 * What the cost cell says.
 *
 * Absent is not zero: a run the ledger never priced has no cost recorded, which
 * is a different fact from one that was free. But a run that has not finished
 * has not been priced *yet*, and "cost not recorded" on it read as a gap in the
 * ledger — so an unsettled run's cell is empty until there is something to say.
 */
function costOf(row: RunSearchResult): string {
  if (runIsAlive(row.status) || row.completed_at === null) return "";
  return row.cost_usd === null ? "cost not recorded" : money(row.cost_usd);
}

/* ------------------------------------------------------------- new run -- */

type AskableMode = (typeof RUN_MODES)[number];

/**
 * The three modes, least consequential first, each with what it does.
 *
 * Every sentence was read out of `core/src/runs.rs`, not out of intent:
 * `create_run_with` launches `shadow` with `Permission::Plan` (`runner.rs`:
 * "how a run is made unable to act"), refuses `worktree` without both a project
 * and a cwd and provisions a checkout of its own that claims one of the
 * project's slots (`concurrency::room_for`), and launches `real` with the
 * default permission rung in whatever directory it was given.
 */
const MODE_CHOICES: readonly { mode: AskableMode; says: string }[] = [
  {
    mode: "shadow",
    says: "The CLI starts in plan mode and answers with a plan instead of acting.",
  },
  {
    mode: "worktree",
    says: "Works in its own checkout of the project, never in your files, and runs the project's gate after if it has one. Needs a project and its root as the working directory; holds one of the project's slots.",
  },
  {
    mode: "real",
    says: "Acts on the files in the working directory — or the daemon's default — as they are. Nothing isolates it.",
  },
];

interface RunDraft {
  prompt: string;
  project: string;
  cwd: string;
  mode: AskableMode;
  steerable: boolean;
  name: string;
}

/**
 * `shadow` by default: the least consequential mode the núcleo accepts from
 * this form as it opens.
 *
 * Verified in `core/src/runs.rs` rather than assumed. With no project and no
 * cwd, `create_run` asks a project's autopilot mode only when there IS a
 * project (`runs_unattended(&req.mode) && let Some(project)`), and
 * `create_run_with` rejects only `worktree` for a missing project or cwd — so
 * a shadow run from the empty form is accepted, and it cannot act. `real` was
 * the old default, which made the one mode with nothing between the run and a
 * checkout the one a person got by not choosing.
 */
const EMPTY_DRAFT: RunDraft = {
  prompt: "",
  project: "",
  cwd: "",
  mode: "shadow",
  steerable: false,
  name: "",
};

/**
 * The New run panel, opened from the header.
 *
 * Not a modal: starting a run needs neither an interruption nor a trap for
 * focus, and the list under it stays readable while somebody writes. It stays
 * mounted while closed so a half-written request survives closing it, which is
 * also what keeps the header button's `aria-controls` pointing at something.
 */
function NewRunPanel({
  id,
  open,
  projects,
  onClose,
  onStarted,
}: {
  id: string;
  open: boolean;
  projects: ProjectSummary[] | undefined;
  onClose: () => void;
  onStarted: (id: number) => void;
}) {
  const prompt = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (open) prompt.current?.focus();
  }, [open]);

  return (
    <div
      id={id}
      className="runs-compose"
      hidden={!open}
      // Escape anywhere inside closes it. Not stopped: an armed interlock in a
      // preset row disarms on the same key, and both are the "no" it means.
      onKeyDown={(event) => {
        if (event.key === "Escape") onClose();
      }}
    >
      <Panel
        title="New run"
        aside={
          <Button variant="quiet" onClick={onClose}>
            Close
          </Button>
        }
      >
        <div className="runs-compose-frame">
          <div className="runs-compose-grid">
            <NewRunForm
              projects={projects}
              promptRef={prompt}
              onStarted={onStarted}
            />
            <PresetsList onStarted={onStarted} />
          </div>
        </div>
      </Panel>
    </div>
  );
}

/**
 * One draft, two destinations: start it now, or save it under a name.
 *
 * The same body goes to `POST /runs` and to `POST /presets` — a preset *is* the
 * run request with a name attached — so a second form for saving one would be
 * the same fields twice, drifting apart the first time a field is added.
 */
function NewRunForm({
  projects,
  promptRef,
  onStarted,
}: {
  projects: ProjectSummary[] | undefined;
  promptRef: RefObject<HTMLTextAreaElement | null>;
  onStarted: (id: number) => void;
}) {
  const create = useCreateRun();
  const save = useCreatePreset();
  const kill = useKillSwitch();
  const [draft, setDraft] = useState<RunDraft>(EMPTY_DRAFT);
  const hintId = useId();
  const killId = useId();
  const realId = useId();
  const modeName = useId();

  function patch(next: Partial<RunDraft>) {
    setDraft((current) => ({ ...current, ...next }));
  }

  const body = {
    prompt: draft.prompt.trim(),
    project_id: draft.project === "" ? null : draft.project,
    cwd: draft.cwd.trim() === "" ? null : draft.cwd.trim(),
    mode: draft.mode,
  };
  const blank = body.prompt === "";

  // Only a reading of `true` is said. The daemon is the authority and checks
  // the switch itself on every start, so an unknown or unreadable switch says
  // nothing here rather than guessing — and an engaged one leaves Start run
  // pressable, because this reading may be a poll behind.
  const engaged = kill.data?.engaged === true;

  // `create_run` refuses an unattended mode for a project whose autopilot is
  // `off` (`core/src/runs.rs`, 422). Said before the press, from the project's
  // own reading, so the default mode is never a refusal waiting to happen.
  const chosen = projects?.find((project) => project.project_id === draft.project);
  const unwatched =
    chosen !== undefined && chosen.mode === "off" && draft.mode !== "real";

  const isReal = draft.mode === "real";

  function start() {
    if (blank || create.isPending) return;
    create.mutate(
      { ...body, steerable: draft.steerable },
      {
        onSuccess: (answer) => {
          setDraft(EMPTY_DRAFT);
          onStarted(answer.id);
        },
      },
    );
  }

  const describedBy =
    [blank ? hintId : null, engaged ? killId : null]
      .filter((part) => part !== null)
      .join(" ") || undefined;

  return (
    <div className="runs-compose-form">
      <form
        className="runs-new"
        onSubmit={(event) => {
          event.preventDefault();
          // A real run is started by the hold below and by nothing else, so an
          // Enter in a field cannot be the one click that acts on files.
          if (isReal) return;
          start();
        }}
      >
        {/* The inner id is Start run's: the button points at the same
            sentence to say why it is not pressable yet. `Field` itself names
            the box by its label and describes it by the helper. */}
        <Field
          label="Prompt"
          helper={<span id={hintId}>Describe what the run should do.</span>}
        >
          <textarea
            ref={promptRef}
            className="runs-prompt"
            rows={4}
            value={draft.prompt}
            onChange={(event) => patch({ prompt: event.target.value })}
          />
        </Field>

        <div className="runs-new-where">
          <Field label="Project for this run">
            <select
              value={draft.project}
              onChange={(event) => patch({ project: event.target.value })}
            >
              <option value="">no project</option>
              {(projects ?? []).map((project) => (
                <option key={project.project_id} value={project.project_id}>
                  {project.project_id}
                </option>
              ))}
            </select>
          </Field>

          <Field label="Working directory">
            <input
              value={draft.cwd}
              placeholder="the default directory"
              onChange={(event) => patch({ cwd: event.target.value })}
            />
          </Field>
        </div>

        <ModePicker
          name={modeName}
          value={draft.mode}
          onChange={(mode) => patch({ mode })}
        />

        {isReal && (
          <p className="runs-hold runs-real" id={realId}>
            A real run acts on the files in{" "}
            {draft.cwd.trim() === "" ? "the default directory" : draft.cwd.trim()}{" "}
            as they are, with nothing to undo it. Hold Start run to confirm.
          </p>
        )}

        {unwatched && (
          <p className="runs-hold">
            {chosen.project_id} is off, and a {draft.mode} run is one nobody is
            watching — the {DAEMON_NAME} refuses it until the project is in
            shadow or active.
          </p>
        )}

        {/* Opt-in, exactly as the daemon has it: a run listens only because
            somebody asked for one that would. */}
        <label className="runs-check">
          <input
            type="checkbox"
            checked={draft.steerable}
            onChange={(event) => patch({ steerable: event.target.checked })}
          />
          <span>Let me speak to this run after it starts</span>
        </label>

        {engaged && (
          <p className="runs-hold" id={killId}>
            {`The kill switch is engaged — the ${DAEMON_NAME} will refuse this run until it is released.`}
          </p>
        )}

        <div className="runs-new-actions">
          {isReal ? (
            <ConfirmButton
              label="Start run"
              confirmLabel="Start in real mode"
              sayAs="starts a real run, acting on the files in the working directory as they are"
              variant="approve"
              intent="go"
              disabled={blank || create.isPending}
              describedBy={[describedBy, realId].filter(Boolean).join(" ")}
              onConfirm={start}
            />
          ) : (
            <Button
              type="submit"
              variant="primary"
              intent="go"
              disabled={blank || create.isPending}
              aria-describedby={describedBy}
            >
              Start run
            </Button>
          )}
        </div>

        {create.isError && (
          <StartRefusal error={create.error} what="the run was not started" />
        )}
      </form>

      {/* The secondary gesture on the same draft: quiet and its own width, so
          it can never outweigh Start run. */}
      <form
        className="runs-save"
        onSubmit={(event) => {
          event.preventDefault();
          if (draft.name.trim() === "" || blank || save.isPending) return;
          save.mutate(
            { name: draft.name.trim(), ...body },
            { onSuccess: () => patch({ name: "" }) },
          );
        }}
      >
        <Field label="Preset name">
          <input
            value={draft.name}
            onChange={(event) => patch({ name: event.target.value })}
          />
        </Field>
        <Button
          type="submit"
          variant="quiet"
          disabled={draft.name.trim() === "" || blank || save.isPending}
        >
          Save as preset
        </Button>
        {save.isError && <PresetRefusal error={save.error} />}
      </form>
    </div>
  );
}

/**
 * The mode, as three visible choices with their consequences.
 *
 * A select hid the one decision on this form that changes what a run may touch
 * behind a word nobody had explained. Native radios, so the arrow keys, the
 * group's name and the global focus ring all come from the platform.
 */
function ModePicker({
  name,
  value,
  onChange,
}: {
  name: string;
  value: AskableMode;
  onChange: (mode: AskableMode) => void;
}) {
  const baseId = useId();
  return (
    <fieldset className="runs-modes">
      <legend className="runs-modes-legend">Mode</legend>
      {MODE_CHOICES.map((choice) => {
        const inputId = `${baseId}-${choice.mode}`;
        const saysId = `${inputId}-says`;
        return (
          <div key={choice.mode} className="runs-mode">
            <input
              type="radio"
              id={inputId}
              name={name}
              value={choice.mode}
              checked={value === choice.mode}
              aria-describedby={saysId}
              onChange={() => onChange(choice.mode)}
            />
            <span className="runs-mode-head">
              <label htmlFor={inputId} className="runs-mode-name">
                {/* Shadow wears Deliberating Violet here as it does on every
                    project, and through the map rather than a tone written at
                    this call site — the autopilot's `shadow` is the same
                    promise, work that proposes and does not act. */}
                {choice.mode === "shadow" ? (
                  <StateBadge domain="autopilot" state="shadow" />
                ) : (
                  choice.mode
                )}
              </label>
              {/* Said beside the name, in Held Ember: the one mode with
                  nothing between the run and the files. Not in the label, so
                  the radio keeps its name. */}
              {choice.mode === "real" && (
                <span className="runs-mode-tag" aria-hidden="true">
                  acts on your files
                </span>
              )}
            </span>
            <p id={saysId} className="runs-mode-says">
              {choice.says}
            </p>
          </div>
        );
      })}
    </fieldset>
  );
}

/* -------------------------------------------------------------- presets -- */

function PresetsList({ onStarted }: { onStarted: (id: number) => void }) {
  const presets = usePresets();
  const run = useRunPreset();
  const remove = useDeletePreset();
  const saved = presets.data;

  return (
    <Section label="Start from a preset">
      {saved === undefined && <Quiet says="reading the saved requests…" />}
      {saved !== undefined && saved.length === 0 && (
        <Quiet says="no saved requests">
          <p>
            A preset is a run request with a name — fill the form in and use{" "}
            <em>Save as preset</em> to keep one.
          </p>
        </Quiet>
      )}
      {saved !== undefined && saved.length > 0 && (
        <Rows label="Presets" className="runs-presets">
          {saved.map((preset) => (
            <PresetRow
              key={preset.id}
              preset={preset}
              busy={run.isPending}
              onRun={() =>
                run.mutate(preset.id, {
                  onSuccess: (answer) => onStarted(answer.id),
                })
              }
              onDelete={() => remove.mutate(preset.id)}
            />
          ))}
        </Rows>
      )}
      {run.isError && (
        <StartRefusal error={run.error} what="the preset was not started" />
      )}
      {remove.isError && (
        <MutationNote
          error={remove.error}
          what="that preset could not be deleted"
        />
      )}
    </Section>
  );
}

/**
 * One saved request, with everything it will do before it does it.
 *
 * A `real` preset asks twice. It acts on a directory with nothing isolating
 * it, and one click on a name was the only thing between a person and that —
 * while deleting the saved string beside it took two. Now the order of cost is
 * the order of care: a real run is interlocked, a shadow or worktree run starts
 * at once, and deleting a preset is a quiet interlock because it destroys a
 * name, not work.
 */
function PresetRow({
  preset,
  busy,
  onRun,
  onDelete,
}: {
  preset: Preset;
  busy: boolean;
  onRun: () => void;
  onDelete: () => void;
}) {
  return (
    <Row className="runs-preset">
      <div className="runs-preset-head">
        <span className="runs-preset-name">{preset.name}</span>
        <span className="runs-preset-fact">{preset.mode}</span>
        <span className="runs-preset-fact">
          {preset.project_id ?? "no project"}
        </span>
      </div>
      {preset.cwd !== null && <p className="runs-preset-cwd">{preset.cwd}</p>}
      <p className="runs-preset-prompt">{preset.prompt}</p>
      <div className="runs-preset-actions">
        {preset.mode === "real" ? (
          // No `subject`: the interlock reserves the armed label's width from
          // the first paint, and "Run in real mode · memory-upkeep" made this
          // the widest control in the list. The name is already on the row, in
          // its own head a line above, so the eye keeps its anchor; the ear
          // gets it in `sayAs`.
          <ConfirmButton
            label={`Run ${preset.name}`}
            confirmLabel="Run in real mode"
            sayAs={`runs ${preset.name} in real mode, acting on the files in ${preset.cwd ?? "the daemon's default directory"} as they are`}
            variant="ghost"
            intent="go"
            disabled={busy}
            onConfirm={onRun}
          />
        ) : (
          <Button intent="go" disabled={busy} onClick={onRun}>
            Run {preset.name}
          </Button>
        )}
        <ConfirmButton
          label={`Delete ${preset.name}`}
          confirmLabel={`Delete ${preset.name} for good`}
          variant="quiet"
          onConfirm={onDelete}
        />
      </div>
    </Row>
  );
}

/* ------------------------------------------------------------- refusals -- */

/**
 * Why the núcleo would not start this — in its own words, when it sent any.
 *
 * `POST /runs` writes a sentence for every refusal it makes (`create_run` in
 * `core/src/runs.rs`, and `create_run_reason` in `core/src/http.rs`), and on
 * this route the sentence says more than a code can: the two 409s — the kill
 * switch engaged, a worktree run's project with no free slot — share a status
 * and arrive as different prose. So the daemon's prose wins, and the sentences
 * below are the floor for a body that came back empty.
 *
 * **The codes here were read out of `core/src/runs.rs`, not out of the old
 * shell's comment, which named 503 for the kill switch and 429 for the budget
 * and was wrong on both counts.** 409 is the switch engaged, or a worktree
 * project full. 503 is something `create_run` reads before every start that
 * could not be read: the kill switch, or — for a shadow or worktree run in a
 * project — that project's autopilot mode. It fails closed on both, which is
 * why an unreadable switch or mode stops a run rather than starting one. 422
 * is the project's own autopilot being `off` for a shadow or worktree run.
 *
 * There is deliberately **no 429 branch**. The daemon does not brake a
 * person-requested run on the budget: the ceilings pace proactive autonomy, and
 * somebody sitting at the window asking for a run is not that. A sentence about
 * a budget here would send people to raise a ceiling that is not holding
 * anything.
 */
const START_SENTENCES: Record<string, string> = {
  conflict:
    "the kill switch is engaged, or this worktree run's project has no free slot — nothing was started",
  unavailable: `the kill switch or the project's autopilot mode could not be read, so the ${DAEMON_NAME} refused rather than start something either may have forbidden`,
  unprocessable:
    "that project is off, and a shadow or worktree run is one nobody is watching — put the project in shadow or active first",
  bad_request:
    `the ${DAEMON_NAME} would not accept that request — check the mode against the directory`,
  internal:
    `the ${DAEMON_NAME} failed to prepare the run — a worktree or the database, not your request`,
};

function StartRefusal({ error, what }: { error: unknown; what: string }) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>
        Could not reach the {DAEMON_NAME} — {what}.
      </ErrorNote>
    );
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{ ...START_SENTENCES, ...daemonProse(error) }}
    />
  );
}

/** `POST /presets` has exactly one refusal worth a sentence of its own. */
function PresetRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>
        Could not reach the {DAEMON_NAME} — the preset was not saved.
      </ErrorNote>
    );
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        conflict:
          "that name is taken — presets are named uniquely, so choose another",
      }}
    />
  );
}

function ListError({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  if (isApiRefusal(error))
    return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return (
    <ErrorNote onRetry={onRetry}>
      Could not reach the {DAEMON_NAME} — nothing is known about the index.
    </ErrorNote>
  );
}

function MutationNote({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error))
    return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return (
    <ErrorNote>
      Could not reach the {DAEMON_NAME} — {what}.
    </ErrorNote>
  );
}

function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const prose = refusal.detail.trim();
  return prose === "" || prose === refusal.code
    ? {}
    : { [refusal.code]: prose };
}

/** One derived sentence about what this list is showing. */
function headline(
  rows: RunSearchResult[] | undefined,
  filters: RunFilters,
  more: boolean,
): ReactNode | undefined {
  if (rows === undefined) return undefined;
  const live = rows.filter((row) => runIsAlive(row.status)).length;
  const waiting = rows.filter(
    (row) => row.status === "awaiting_approval",
  ).length;
  const failed = rows.filter((row) => row.status === "failed").length;
  const filtered = isFiltered(filters);
  if (rows.length === 0)
    return filtered ? "nothing matching these filters" : "nothing in the index";
  // While an older page may exist the count is what is loaded, not the size
  // of the index, and "50 in the index" said the second.
  const count = more ? `the newest ${rows.length}` : `${rows.length}`;
  const scope = filtered ? "matching these filters" : "in the index";
  const said = more && !filtered ? count : `${count} ${scope}`;
  const moving =
    live === 0 ? "none of them still moving" : `${live} still moving`;
  // Failures are counted because they are what the verification read is
  // looking for; zero is not worth a clause.
  const wrong = failed === 0 ? "" : `; ${failed} failed`;
  // A run waiting on a person is not moving, and the header must not hide it
  // either: it is the one count on this page somebody can act on.
  if (waiting === 0) return `${said}; ${moving}${wrong}`;
  return (
    <>
      {said}; {moving};{" "}
      <Link to="/waiting">
        {waiting} run{waiting === 1 ? "" : "s"} awaiting approval
      </Link>
      {wrong}
    </>
  );
}
