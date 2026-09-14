import { useEffect, useId, useRef, useState, type ReactNode, type RefObject } from "react";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  RUN_LIST_LIMIT,
  RUN_MODE_FILTERS,
  RUN_STATUSES,
  runIsAlive,
  useCreateRun,
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
import { useKillSwitch, useProjects, type ProjectSummary } from "../data/system";
import {
  Button,
  ConfirmButton,
  ErrorNote,
  Field,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  Section,
  RelativeTime,
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

/** The four filters, as the route spells them. */
export interface RunSearch {
  project?: string;
  status?: string;
  mode?: string;
  q?: string;
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

  const runs = useRuns(filters);
  const projects = useProjects();
  const [composing, setComposing] = useState(false);
  const panelId = useId();
  const openerId = useId();

  /**
   * The list on screen is no longer the daemon's.
   *
   * A query that succeeded once keeps its rows when a later refetch fails, which
   * is what this page wants — a list that blanks reads as *nothing has run*.
   * The note is how the page says the rows are old.
   */
  const stale = runs.isError && runs.data !== undefined;
  const rows = runs.data;

  function applyFilters(patch: Partial<RunSearch>) {
    void navigate({
      to: "/runs",
      search: validateRunSearch({ ...filters, ...patch }),
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
        // A status region, so a failure that lands while somebody is reading
        // the page is said rather than only drawn. It is always mounted: a live
        // region that appears together with its first sentence is not
        // announced.
        headline={
          <span role="status" aria-live="polite">
            {headline(rows, filters)}
          </span>
        }
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
        projects={projects.data}
        onChange={applyFilters}
      />

      {stale && <StaleNote dataUpdatedAt={runs.dataUpdatedAt} />}
      {runs.isError && rows === undefined && <ListError error={runs.error} />}

      <RunList
        rows={rows}
        filtered={isFiltered(filters)}
        onCompose={composing ? undefined : () => setComposing(true)}
      />
    </>
  );
}

/* -------------------------------------------------------------- filters -- */

/**
 * The scopes a person reaches for, as one control.
 *
 * Four, because that is the whole of the verification read: everything, what
 * is moving, what awaits approval, what went wrong. `failed` alone and not
 * "failed, timed out or interrupted": `GET /runs` binds `status` to exactly one
 * value (`core/src/runs.rs`, `search` — `AND status = ?`), so a scope that
 * promised three would be a request the daemon cannot answer. The exact
 * statuses are one disclosure away.
 */
const SCOPES: readonly { label: string; status: string | undefined }[] = [
  { label: "All", status: undefined },
  { label: "Running", status: "running" },
  { label: "Awaiting approval", status: "awaiting_approval" },
  { label: "Failed", status: "failed" },
];

const SCOPE_STATUSES = new Set(
  SCOPES.map((scope) => scope.status).filter((status) => status !== undefined),
);

/**
 * The filters, bound to the route.
 *
 * The selects and the scope commit on change and the text box on Enter, which
 * is the split a person expects: choosing from a list is a decision, typing is
 * not, and a navigation per keystroke would put a history entry behind every
 * letter. The Search button sits against the box it submits so it cannot read
 * as the button that applies everything.
 */
function RunFilterBar({
  filters,
  projects,
  onChange,
}: {
  filters: RunFilters;
  projects: ProjectSummary[] | undefined;
  onChange: (patch: Partial<RunSearch>) => void;
}) {
  const moreId = useId();
  // Open on arrival when a filter it holds is in force: a mode, or a status the
  // scope control has no segment for. Otherwise the list would be narrowed by a
  // control nobody can see.
  const needsMore =
    filters.mode !== undefined ||
    (filters.status !== undefined && !SCOPE_STATUSES.has(filters.status));
  const [more, setMore] = useState(needsMore);
  // Adjusted during render rather than in an effect, so there is no frame in
  // which a filter that just came into force is hidden.
  const [sawNeed, setSawNeed] = useState(needsMore);
  if (needsMore !== sawNeed) {
    setSawNeed(needsMore);
    if (needsMore) setMore(true);
  }

  return (
    <form
      className="runs-filters"
      role="search"
      aria-label="Filter runs"
      onSubmit={(event) => {
        event.preventDefault();
        const typed = new FormData(event.currentTarget).get("q");
        onChange({ q: typeof typed === "string" ? typed : undefined });
      }}
    >
      <div className="ui-switch runs-scope" role="group" aria-label="Show">
        {SCOPES.map((scope) => (
          <button
            key={scope.label}
            type="button"
            className="ui-switch-seg"
            aria-pressed={filters.status === scope.status}
            onClick={() => onChange({ status: scope.status })}
          >
            {scope.label}
          </button>
        ))}
      </div>

      <div className="runs-search">
        <Field label="Search prompts">
          <input name="q" defaultValue={filters.q ?? ""} key={filters.q ?? ""} />
        </Field>
        <Button type="submit">Search</Button>
      </div>

      <Field label="Project">
        <select
          value={filters.project ?? ""}
          aria-label="Filter by project"
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

      <span className="runs-filters-aside">
        <Button
          variant="quiet"
          aria-expanded={more}
          aria-controls={moreId}
          onClick={() => setMore(!more)}
        >
          More filters
        </Button>
        {isFiltered(filters) && (
          <Button
            variant="quiet"
            onClick={() =>
              onChange({
                project: undefined,
                status: undefined,
                mode: undefined,
                q: undefined,
              })
            }
          >
            Clear filters
          </Button>
        )}
      </span>

      <div id={moreId} className="runs-more" hidden={!more}>
        <Field label="Mode">
          {/* `assistant` is offered here and not in the new-run form: the daemon
              writes that mode, so runs carry it, but nobody may ask for one. */}
          <select
            value={filters.mode ?? ""}
            aria-label="Filter by mode"
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

        <Field label="Status">
          {/* The badge's own words, from the one table: a filter that says
              `timed_out` above a list that says "timed out" is two vocabularies
              for one fact. */}
          <select
            value={filters.status ?? ""}
            aria-label="Filter by status"
            onChange={(event) => onChange({ status: event.target.value })}
          >
            <option value="">any status</option>
            {RUN_STATUSES.map((status) => (
              <option key={status} value={status}>
                {readState("run", status)?.label ?? status}
              </option>
            ))}
          </select>
        </Field>
      </div>
    </form>
  );
}

function isFiltered(filters: RunFilters): boolean {
  return Object.values(filters).some(
    (value) => value !== undefined && value !== "",
  );
}

/* ----------------------------------------------------------------- list -- */

function RunList({
  rows,
  filtered,
  onCompose,
}: {
  rows: RunSearchResult[] | undefined;
  filtered: boolean;
  /** Opens the New run panel; absent while it is already open. */
  onCompose: (() => void) | undefined;
}) {
  if (rows === undefined)
    return <p className="runs-loading">reading the index…</p>;

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
            The núcleo has runs, but none of them match. Clear the filters above
            to see the whole index — an empty filtered list is not an empty
            machine.
          </p>
        ) : (
          <p>
            A run is one call to the CLI. Start one with New run at the top of
            the page, or save a request as a preset and start it from there.
            Jobs and the autopilot create runs too, and they appear here
            alongside the ones you ask for.
          </p>
        )}
      </Teach>
    );
  }

  return (
    <>
      <ul className="ui-rows" aria-label="Runs">
        {rows.map((row) => (
          <li key={row.id} className="ui-rows-row runs-row">
            <span className="runs-row-state">
              <RunState status={row.status} />
            </span>
            <Link to={`/runs/${row.id}`} className="runs-row-name">
              {row.prompt_excerpt}
            </Link>
            <p className="runs-row-meta">
              <span className="runs-row-id">run {row.id}</span>
              <span className="runs-row-mode">{row.mode}</span>
              <span className="runs-row-project">
                {row.project_id ?? "no project"}
              </span>
              <RelativeTime at={row.created_at} />
              <span className="runs-row-cost">{costOf(row)}</span>
            </p>
          </li>
        ))}
      </ul>
      {rows.length >= RUN_LIST_LIMIT && (
        <p className="runs-ceiling">
          showing the newest {RUN_LIST_LIMIT} — there may be more behind these;
          narrow the filters to reach them
        </p>
      )}
    </>
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
        }}
      >
        <Field
          label="Prompt"
          helper={<span id={hintId}>Start run waits until this says what the run should do.</span>}
        >
          {/* Named outright, and with the visible word: `Field` renders its
              helper inside the `<label>`, so without this the helper would be
              read as part of the name instead of as the description. */}
          <textarea
            ref={promptRef}
            className="runs-prompt"
            rows={4}
            value={draft.prompt}
            aria-label="Prompt"
            aria-describedby={hintId}
            onChange={(event) => patch({ prompt: event.target.value })}
          />
        </Field>

        <div className="runs-new-where">
          <Field label="Project">
            <select
              value={draft.project}
              aria-label="Project for the new run"
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
              placeholder="the daemon's default"
              aria-label="Working directory for the new run"
              onChange={(event) => patch({ cwd: event.target.value })}
            />
          </Field>
        </div>

        <ModePicker
          name={modeName}
          value={draft.mode}
          onChange={(mode) => patch({ mode })}
        />

        {unwatched && (
          <p className="runs-hold">
            {chosen.project_id} is off, and a {draft.mode} run is one nobody is
            watching — the núcleo refuses it until the project is in shadow or
            active.
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
            The kill switch is engaged — the núcleo will refuse this run until
            it is released.
          </p>
        )}

        <div className="runs-new-actions">
          <Button
            type="submit"
            intent="go"
            disabled={blank || create.isPending}
            aria-describedby={describedBy}
          >
            Start run
          </Button>
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
      {saved === undefined && (
        <p className="runs-loading">reading the saved requests…</p>
      )}
      {saved !== undefined && saved.length === 0 && (
        <Quiet says="no saved requests">
          <p>
            A preset is a run request with a name — fill the form in and use{" "}
            <em>Save as preset</em> to keep one.
          </p>
        </Quiet>
      )}
      {saved !== undefined && saved.length > 0 && (
        <ul className="ui-rows runs-presets" aria-label="Presets">
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
        </ul>
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
    <li className="ui-rows-row runs-preset">
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
          <ConfirmButton
            label={`Run ${preset.name}`}
            confirmLabel="Run in real mode"
            subject={preset.name}
            sayAs={`runs in real mode, acting on the files in ${preset.cwd ?? "the daemon's default directory"} as they are`}
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
    </li>
  );
}

/* ------------------------------------------------------------- refusals -- */

/**
 * Why the núcleo would not start this.
 *
 * **The codes here were read out of `core/src/runs.rs`, not out of the old
 * shell's comment, which named 503 for the kill switch and 429 for the budget
 * and was wrong on both counts.** `create_run` answers 409 when the switch is
 * engaged and 503 when the switch could not be *read* — it fails closed, which
 * is why an unreadable switch stops a run rather than starting one. 422 is the
 * project's own autopilot being `off` for a shadow or worktree run.
 *
 * There is deliberately **no 429 branch**. The daemon does not brake a
 * person-requested run on the budget: the ceilings pace proactive autonomy, and
 * somebody sitting at the window asking for a run is not that. A sentence about
 * a budget here would send people to raise a ceiling that is not holding
 * anything.
 */
const START_SENTENCES: Record<string, string> = {
  conflict:
    "the kill switch is engaged — nothing autonomous starts until it is released; a worktree run meets the same answer when its project is already full, and the núcleo sends no body that tells the two apart",
  unavailable:
    "the kill switch could not be read, so the núcleo refused rather than start something the switch may have forbidden",
  unprocessable:
    "that project is off, and a shadow or worktree run is one nobody is watching — put the project in shadow or active first",
  bad_request:
    "the núcleo would not accept that request — check the mode against the directory",
  internal:
    "the núcleo failed to prepare the run — a worktree or the database, not your request",
};

function StartRefusal({ error, what }: { error: unknown; what: string }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — {what}</ErrorNote>;
  }
  return <RefusalNote refusal={error} sentences={START_SENTENCES} />;
}

/** `POST /presets` has exactly one refusal worth a sentence of its own. */
function PresetRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>
        the núcleo did not answer — the preset was not saved
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

function ListError({ error }: { error: unknown }) {
  if (isApiRefusal(error))
    return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return (
    <ErrorNote>
      the núcleo did not answer — nothing is known about the index
    </ErrorNote>
  );
}

function MutationNote({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error))
    return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — {what}</ErrorNote>;
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
  // At the ceiling the count is the page size, not the size of the index, and
  // "50 in the index" said the second.
  const count =
    rows.length >= RUN_LIST_LIMIT
      ? `the newest ${rows.length}`
      : `${rows.length}`;
  const scope = filtered ? "matching these filters" : "in the index";
  const said =
    rows.length >= RUN_LIST_LIMIT && !filtered
      ? count
      : `${count} ${scope}`;
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
