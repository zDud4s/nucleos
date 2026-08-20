import { useState } from "react";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  RUN_LIST_LIMIT,
  RUN_MODES,
  RUN_MODE_FILTERS,
  RUN_STATUSES,
  useCreateRun,
  useRuns,
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
import { useProjects, type ProjectSummary } from "../data/system";
import {
  Button,
  ConfirmButton,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
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
    void navigate({ to: "/runs", search: validateRunSearch({ ...filters, ...patch }) });
  }

  return (
    <>
      <PageHeader title="Runs" headline={headline(rows, filters)} />

      <RunFilterBar filters={filters} projects={projects.data} onChange={applyFilters} />

      {stale && <StaleNote dataUpdatedAt={runs.dataUpdatedAt} />}
      {runs.isError && rows === undefined && <ListError error={runs.error} />}

      <div className="runs-body">
        <div className="runs-main">
          <RunList rows={rows} filtered={isFiltered(filters)} />
        </div>
        <div className="runs-side">
          <NewRunForm projects={projects.data} onStarted={(id) => void navigate({ to: `/runs/${id}` })} />
          <PresetsRail onStarted={(id) => void navigate({ to: `/runs/${id}` })} />
        </div>
      </div>
    </>
  );
}

/* -------------------------------------------------------------- filters -- */

/**
 * The four filters, bound to the route.
 *
 * The selects commit on change and the text box on Enter, which is the split a
 * person expects: choosing from a list is a decision, typing is not, and a
 * navigation per keystroke would put a history entry behind every letter.
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
      <label className="runs-filter">
        <span>Project</span>
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
      </label>

      <label className="runs-filter">
        <span>Status</span>
        <select
          value={filters.status ?? ""}
          aria-label="Filter by status"
          onChange={(event) => onChange({ status: event.target.value })}
        >
          <option value="">any status</option>
          {RUN_STATUSES.map((status) => (
            <option key={status} value={status}>
              {status}
            </option>
          ))}
        </select>
      </label>

      <label className="runs-filter">
        <span>Mode</span>
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
      </label>

      <label className="runs-filter runs-filter-text">
        <span>Text</span>
        <input name="q" defaultValue={filters.q ?? ""} key={filters.q ?? ""} aria-label="Search prompts" />
      </label>

      <Button type="submit">Search</Button>
      {isFiltered(filters) && (
        <Button
          onClick={() => onChange({ project: undefined, status: undefined, mode: undefined, q: undefined })}
        >
          Clear filters
        </Button>
      )}
    </form>
  );
}

function isFiltered(filters: RunFilters): boolean {
  return Object.values(filters).some((value) => value !== undefined && value !== "");
}

/* ----------------------------------------------------------------- list -- */

function RunList({ rows, filtered }: { rows: RunSearchResult[] | undefined; filtered: boolean }) {
  if (rows === undefined) return <p className="runs-loading">reading the index…</p>;

  if (rows.length === 0) {
    return (
      <Teach title={filtered ? "Nothing matches those filters" : "No runs yet"}>
        {filtered ? (
          <p>
            The núcleo has runs, but none of them match. Clear the filters above to see the whole
            index — an empty filtered list is not an empty machine.
          </p>
        ) : (
          <p>
            A run is one call to the CLI. Start one with the form beside this list, or save a request
            as a preset and start it from there. Jobs and the autopilot create runs too, and they
            appear here alongside the ones you ask for.
          </p>
        )}
      </Teach>
    );
  }

  return (
    <>
      <ul className="runs-list" aria-label="Runs">
        {rows.map((row) => (
          <li key={row.id} className="runs-row">
            <div className="runs-row-head">
              <Link to={`/runs/${row.id}`} className="runs-row-link">
                run {row.id}
              </Link>
              <StateBadge domain="run" state={row.status} />
              <span className="runs-row-mode">{row.mode}</span>
              <span className="runs-row-project">{row.project_id ?? "no project"}</span>
              <RelativeTime at={row.created_at} />
              {/* Absent is not zero: a run the ledger never priced has no cost
                  recorded, which is a different fact from one that was free. */}
              <span className="runs-row-cost">
                {row.cost_usd === null ? "cost not recorded" : `$ ${row.cost_usd.toFixed(4)}`}
              </span>
            </div>
            <p className="runs-row-excerpt">{row.prompt_excerpt}</p>
          </li>
        ))}
      </ul>
      {rows.length >= RUN_LIST_LIMIT && (
        <p className="runs-ceiling">
          showing the newest {RUN_LIST_LIMIT} — there may be more behind these; narrow the filters to
          reach them
        </p>
      )}
    </>
  );
}

/* ------------------------------------------------------------- new run -- */

interface RunDraft {
  prompt: string;
  project: string;
  cwd: string;
  mode: string;
  steerable: boolean;
  name: string;
}

const EMPTY_DRAFT: RunDraft = {
  prompt: "",
  project: "",
  cwd: "",
  mode: "real",
  steerable: false,
  name: "",
};

/**
 * One draft, two destinations: start it now, or save it under a name.
 *
 * The same body goes to `POST /runs` and to `POST /presets` — a preset *is* the
 * run request with a name attached — so a second form for saving one would be
 * the same fields twice, drifting apart the first time a field is added.
 */
function NewRunForm({
  projects,
  onStarted,
}: {
  projects: ProjectSummary[] | undefined;
  onStarted: (id: number) => void;
}) {
  const create = useCreateRun();
  const save = useCreatePreset();
  const [draft, setDraft] = useState<RunDraft>(EMPTY_DRAFT);

  function patch(next: Partial<RunDraft>) {
    setDraft((current) => ({ ...current, ...next }));
  }

  const body = {
    prompt: draft.prompt.trim(),
    project_id: draft.project === "" ? null : draft.project,
    cwd: draft.cwd.trim() === "" ? null : draft.cwd.trim(),
    mode: draft.mode,
  };

  return (
    <Panel title="New run">
      <form
        className="runs-new"
        onSubmit={(event) => {
          event.preventDefault();
          if (body.prompt === "" || create.isPending) return;
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
        <label className="runs-field">
          <span>Prompt</span>
          <textarea
            className="runs-prompt"
            rows={4}
            value={draft.prompt}
            aria-label="What the run should do"
            onChange={(event) => patch({ prompt: event.target.value })}
          />
        </label>

        <label className="runs-field">
          <span>Project</span>
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
        </label>

        <label className="runs-field">
          <span>Working directory</span>
          <input
            value={draft.cwd}
            placeholder="the daemon's default"
            aria-label="Working directory for the new run"
            onChange={(event) => patch({ cwd: event.target.value })}
          />
        </label>

        <label className="runs-field">
          <span>Mode</span>
          <select
            value={draft.mode}
            aria-label="Mode for the new run"
            onChange={(event) => patch({ mode: event.target.value })}
          >
            {RUN_MODES.map((mode) => (
              <option key={mode} value={mode}>
                {mode}
              </option>
            ))}
          </select>
        </label>

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

        <div className="runs-new-actions">
          <Button type="submit" intent="go" disabled={create.isPending}>
            Start run
          </Button>
        </div>

        {create.isError && <StartRefusal error={create.error} what="the run was not started" />}
      </form>

      <form
        className="runs-save"
        onSubmit={(event) => {
          event.preventDefault();
          if (draft.name.trim() === "" || body.prompt === "" || save.isPending) return;
          save.mutate(
            { name: draft.name.trim(), ...body },
            { onSuccess: () => patch({ name: "" }) },
          );
        }}
      >
        <label className="runs-field">
          <span>Save this as</span>
          <input
            value={draft.name}
            placeholder="a name for the preset"
            aria-label="Name for the preset"
            onChange={(event) => patch({ name: event.target.value })}
          />
        </label>
        <Button type="submit" disabled={save.isPending}>
          Save as preset
        </Button>
        {save.isError && <PresetRefusal error={save.error} />}
      </form>
    </Panel>
  );
}

/* -------------------------------------------------------------- presets -- */

function PresetsRail({ onStarted }: { onStarted: (id: number) => void }) {
  const presets = usePresets();
  const run = useRunPreset();
  const remove = useDeletePreset();
  const saved = presets.data;

  return (
    <Panel title="Presets">
      {saved === undefined && <p className="runs-loading">reading the saved requests…</p>}
      {saved !== undefined && saved.length === 0 && (
        <Teach title="No saved requests">
          <p>
            A preset is a run request with a name — the same prompt, project, directory and mode you
            would type above. Fill the form in and use <em>Save as preset</em> to keep one.
          </p>
        </Teach>
      )}
      {saved !== undefined && saved.length > 0 && (
        <ul className="runs-presets" aria-label="Presets">
          {saved.map((preset) => (
            <PresetRow
              key={preset.id}
              preset={preset}
              busy={run.isPending}
              onRun={() => run.mutate(preset.id, { onSuccess: (answer) => onStarted(answer.id) })}
              onDelete={() => remove.mutate(preset.id)}
            />
          ))}
        </ul>
      )}
      {run.isError && <StartRefusal error={run.error} what="the preset was not started" />}
      {remove.isError && <MutationNote error={remove.error} what="that preset could not be deleted" />}
    </Panel>
  );
}

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
    <li className="runs-preset">
      <div className="runs-preset-head">
        <span className="runs-preset-name">{preset.name}</span>
        <span className="runs-preset-mode">{preset.mode}</span>
        <span className="runs-preset-project">{preset.project_id ?? "no project"}</span>
      </div>
      <p className="runs-preset-prompt">{preset.prompt}</p>
      <div className="runs-preset-actions">
        <Button intent="go" disabled={busy} onClick={onRun}>
          Run {preset.name}
        </Button>
        <ConfirmButton
          label={`Delete ${preset.name}`}
          confirmLabel={`Delete ${preset.name} for good`}
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
 * is why an unreadable switch stops a run rather than starting one.
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
  bad_request: "the núcleo would not accept that request — check the mode against the directory",
  internal: "the núcleo failed to prepare the run — a worktree or the database, not your request",
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
    return <ErrorNote>the núcleo did not answer — the preset was not saved</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{ conflict: "that name is taken — presets are named uniquely, so choose another" }}
    />
  );
}

function ListError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the index</ErrorNote>;
}

function MutationNote({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — {what}</ErrorNote>;
}

function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const prose = refusal.detail.trim();
  return prose === "" || prose === refusal.code ? {} : { [refusal.code]: prose };
}

/** One derived sentence about what this list is showing. */
function headline(rows: RunSearchResult[] | undefined, filters: RunFilters): string | undefined {
  if (rows === undefined) return undefined;
  const live = rows.filter((row) => row.status === "running" || row.status === "pending").length;
  const scope = isFiltered(filters) ? "matching these filters" : "in the index";
  if (rows.length === 0) return `nothing ${scope}`;
  const moving = live === 0 ? "none of them still moving" : `${live} still moving`;
  return `${rows.length} ${scope}; ${moving}`;
}
