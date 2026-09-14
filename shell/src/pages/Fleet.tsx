import { Fragment, useEffect, useId, useRef, useState, type ReactNode, type RefObject } from "react";
import { Link } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  useCancelSlotOwner,
  useConcurrency,
  useCreateJob,
  useExclusionRequests,
  useExclusions,
  useLiveJobs,
  useLiveRuns,
  useProposeExclusion,
  useRevokeExclusion,
  type Concurrency,
  type ProjectConcurrency,
} from "../data/fleet";
import {
  useBudget,
  useKillSwitch,
  useProjects,
  type BudgetView,
  type ProjectSummary,
} from "../data/system";
import { useTeams } from "../data/teams";
import {
  FleetActionsProvider,
  FleetCanvas,
  SlotCard,
  type FleetActions,
} from "../canvas/FleetCanvas";
import {
  buildFleet,
  loadLayout,
  saveLayout,
  type Exceptions,
  type FleetColumn,
  type Layout,
} from "../canvas/model";
import {
  Button,
  ErrorNote,
  Field,
  Meter,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  StaleNote,
  StatCard,
  StateBadge,
  Teach,
  usd,
} from "../ui";
import "./fleet.css";

/**
 * The fleet: a card per **slot**, in two arrangements of the same model.
 *
 * Per slot and not per job, because an autonomous worktree run holds a slot
 * too: a page that counted jobs would say `0/2` about a project that is going
 * to refuse the next one with a 409, and would offer the button that asks for
 * it.
 *
 * The columns are the view that carries `n/limit` — the only thing on screen
 * that says *there is no more room* — and the canvas is the view where two jobs
 * of the same project can be joined, which is a question about a pair and not
 * about a column. Both are drawn from one derivation (`canvas/model.ts`) so
 * they cannot disagree about the same job.
 *
 * **What is wrong comes first, everywhere on the page.** The headline says the worst fact before
 * the capacity, and the columns are ordered by the same ladder, so a leaked slot in a quiet
 * project is read before three busy projects that are fine. Idle projects come after the columns
 * as a list of their own, one row each: quieter than a column, because the normal recedes, and
 * never less than content, because on a quiet day they are the whole page.
 *
 * **The kill switch control is not duplicated here.** It lives in the frame, on every page; a
 * second copy is a second thing to keep in step and a second thing to be wrong. What this page
 * does say is what the switch means for it — no new job starts — in the headline and beside the
 * one control it disables.
 */
export function Fleet() {
  const concurrency = useConcurrency();
  const jobs = useLiveJobs();
  const runs = useLiveRuns();
  const exclusions = useExclusions();
  const requests = useExclusionRequests();
  const projects = useProjects();
  const budget = useBudget();
  const kill = useKillSwitch();

  const [view, setView] = useState<FleetView>("columns");
  /** One item list open at a time, page-wide: two open cards is two polls. */
  const [openJob, setOpenJob] = useState<number | null>(null);
  /**
   * Read once, at mount, and held here rather than in the canvas — an
   * arrangement made on the surface has to survive a switch to the columns and
   * back, and the canvas unmounts when it is not the open view.
   */
  const [layout, setLayout] = useState<Layout>(loadLayout);
  const [composing, setComposing] = useState(false);
  /**
   * The project the panel's select holds — here and not in the form, so an idle row can open the
   * panel with its own project already chosen. Empty is "nobody picked", which the form reads as
   * the roomiest.
   */
  const [picked, setPicked] = useState("");
  /** One more on every request to open the panel, so focus goes in even when it is already open. */
  const [asked, setAsked] = useState(0);
  /** The control that opened the panel — the header's New job or a row's — and so where focus goes back. */
  const returnTo = useRef<HTMLElement | null>(null);
  const panelId = useId();
  const openerId = useId();

  const cancel = useCancelSlotOwner();
  const propose = useProposeExclusion();
  const revoke = useRevokeExclusion();

  const model = buildFleet({
    concurrency: concurrency.data,
    jobs: jobs.data,
    runs: runs.data,
    exclusions: exclusions.data,
    requests: requests.data,
    layout,
  });

  /**
   * The capacity on screen is no longer the daemon's.
   *
   * A query that has succeeded once keeps its data when a later refetch fails —
   * which is the behaviour this page wants, because a blank fleet reads as
   * *there is room*. This is how the page says the view it is showing is old,
   * and it is what takes the *new job* action away rather than letting it fail
   * after the click.
   */
  const stale = concurrency.isError && concurrency.data !== undefined;
  const capacity = concurrency.data;
  // Only a reading of `true` is said. An unread switch says nothing rather than guessing, and
  // `POST /jobs` checks the switch itself before anything else (`core/src/http.rs`,
  // `create_job`), failing closed — so the daemon stays the authority either way.
  const engaged = kill.data?.engaged === true;
  const hasProjects = capacity !== undefined && capacity.projects.length > 0;
  const waiting = projects.data?.reduce((total, project) => total + project.open_proposals, 0);

  const actions: FleetActions = {
    openJob,
    toggleJob: (jobId) => setOpenJob((current) => (current === jobId ? null : jobId)),
    cancel: (owner) => cancel.mutate(owner),
    lift: (exclusionId) => revoke.mutate(exclusionId),
    propose: (a, b) => propose.mutate({ job_a: a, job_b: b, paths: [] }),
    stale,
  };

  function openComposer(from: HTMLElement, project?: string) {
    returnTo.current = from;
    if (project !== undefined) setPicked(project);
    setComposing(true);
    setAsked((count) => count + 1);
  }

  // Focus goes back to the control that opened the panel, which is the standard the project
  // switcher set: a panel that closes and drops focus on `<body>` leaves a keyboard user at the
  // top of the document. The row that opened it can be gone by the time it closes — its project
  // took a slot meanwhile and left the idle list — and then the header's New job is the one
  // control left that means the same thing.
  function closeComposer() {
    setComposing(false);
    const back = returnTo.current;
    (back !== null && back.isConnected ? back : document.getElementById(openerId))?.focus();
  }

  return (
    <FleetActionsProvider actions={actions}>
      <PageHeader
        title="Fleet"
        // A status region, so a fact that lands while somebody is reading is said rather than
        // only drawn. Always mounted: a live region that appears together with its first
        // sentence is not announced.
        headline={
          <span role="status" aria-live="polite">
            {headline({ capacity, totals: model.totals, engaged, stale, waiting })}
          </span>
        }
        actions={
          // Neither control means anything without a project to look at or to start work in,
          // so an empty or unread fleet offers neither. New job is also gone — not disabled —
          // while the view is stale: the room it would ask for is room nobody can vouch for,
          // and `StaleNote` below is what says why it went.
          hasProjects ? (
            <>
              <ViewSwitch view={view} onChange={setView} />
              {!stale && (
                <Button
                  id={openerId}
                  intent="go"
                  aria-expanded={composing}
                  aria-controls={panelId}
                  onClick={(event) =>
                    composing ? setComposing(false) : openComposer(event.currentTarget)
                  }
                >
                  New job
                </Button>
              )}
            </>
          ) : undefined
        }
      />

      {stale && <StaleNote dataUpdatedAt={concurrency.dataUpdatedAt} />}

      {hasProjects && !stale && (
        <NewJobPanel
          id={panelId}
          open={composing}
          asked={asked}
          columns={model.columns}
          projects={projects.data}
          engaged={engaged}
          picked={picked}
          onPick={setPicked}
          onClose={closeComposer}
        />
      )}

      <div className="fleet-meter">
        <StatCard
          label="In flight"
          value={capacity === undefined ? undefined : `${capacity.house.held}/${capacity.house.limit}`}
          detail="slots held across all projects"
        />
        <StatCard
          label="Window spend"
          value={budget.data === undefined ? undefined : usd(budget.data.window_spend_usd)}
          detail={ceiling(budget.data)}
          // The bar only where there is a ceiling to fill. With none, the line above already
          // says "no ceiling", and an open rail beside it would say it a second time.
          bar={
            budget.data !== undefined && budget.data.limit_usd !== null ? (
              <Meter
                label="Window spend"
                value={budget.data.window_spend_usd}
                ceiling={budget.data.limit_usd}
                tone="quantity"
                format={usd}
                head={false}
              />
            ) : undefined
          }
        />
        <StatCard label="Proposals open" value={waiting} detail="across the roster" />
      </div>

      {concurrency.isError && capacity === undefined && (
        <CapacityError error={concurrency.error} at={concurrency.errorUpdatedAt} />
      )}
      {cancel.isError && <MutationNote error={cancel.error} what="that could not be cancelled" />}
      {revoke.isError && <MutationNote error={revoke.error} what="that rule could not be lifted" />}
      {propose.isError && <MutationNote error={propose.error} what="that pair could not be asked about" />}

      {capacity !== undefined && capacity.projects.length === 0 && (
        <Teach title="No projects yet">
          <p>
            The fleet is every project the núcleo looks after and the work each one has in flight.
            Add one on <Link to="/projects">Projects</Link> and it appears here with how much it
            may run at once — empty to begin with, which is a different fact from not being here.
          </p>
        </Teach>
      )}

      {hasProjects &&
        (view === "canvas" ? (
          <FleetCanvas
            nodes={model.nodes}
            edges={model.edges}
            ends={model.ends}
            onPropose={(a, b) => propose.mutate({ job_a: a, job_b: b, paths: [] })}
            onLayoutChange={(next) => {
              saveLayout(next);
              setLayout(next);
            }}
          />
        ) : (
          <Columns
            columns={model.columns}
            projects={projects.data}
            stale={stale}
            panelId={panelId}
            onNewJob={openComposer}
          />
        ))}
    </FleetActionsProvider>
  );
}

/** Which of the two ways of looking at the fleet is open. */
type FleetView = "columns" | "canvas";

/**
 * `aria-pressed` and not a shade alone: which of the two is open has to be announced, not only
 * coloured. The app's one segmented control (`.ui-switch`), the same the Runs scope uses, so
 * this page no longer draws a pill-shaped one of its own.
 */
function ViewSwitch({ view, onChange }: { view: FleetView; onChange: (next: FleetView) => void }) {
  return (
    <div className="ui-switch" role="group" aria-label="How to look at the fleet">
      <button
        type="button"
        className="ui-switch-seg"
        aria-pressed={view === "columns"}
        onClick={() => onChange("columns")}
      >
        Columns
      </button>
      <button
        type="button"
        className="ui-switch-seg"
        aria-pressed={view === "canvas"}
        onClick={() => onChange("canvas")}
      >
        Canvas
      </button>
    </div>
  );
}

/* ---------------------------------------------------------------- columns -- */

/**
 * The busy projects as columns, and the idle ones as a list after them.
 *
 * An idle project used to keep a full column at `0/N`, on the argument that a column vanishing
 * makes the layout jump. With the columns ordered by what is wrong, position is no longer the
 * thing a reader memorises — the headline is — and four empty columns were pushing the one with
 * a problem in it off the right of the screen. So only a busy project is a column.
 */
function Columns({
  columns,
  projects,
  stale,
  panelId,
  onNewJob,
}: {
  columns: FleetColumn[];
  projects: ProjectSummary[] | undefined;
  stale: boolean;
  panelId: string;
  onNewJob: (from: HTMLElement, project: string) => void;
}) {
  const busy = columns.filter((column) => !column.idle);
  const idle = columns.filter((column) => column.idle);
  return (
    <>
      {busy.length > 0 && (
        <div className="fleet-columns">
          {busy.map((column) => (
            <ProjectColumn
              key={column.project.project_id}
              column={column}
              summary={projects?.find((project) => project.project_id === column.project.project_id)}
            />
          ))}
        </div>
      )}
      {idle.length > 0 && (
        <IdleProjects
          columns={idle}
          projects={projects}
          stale={stale}
          panelId={panelId}
          onNewJob={onNewJob}
        />
      )}
    </>
  );
}

/**
 * The projects with nothing held: a list after the busy columns, or the page's body when nothing
 * is busy anywhere.
 *
 * It was a column each, which is the paragraph above; then one line at the foot of the page, and
 * on a quiet day that line was the page — five projects reduced to a footnote of underlined names.
 * A list is neither. It is the rows idiom (`.ui-rows`), read by scanning down it, with the two
 * facts the column head carried — the autopilot mode and the capacity — the room that capacity
 * leaves, and the one thing an idle project is for.
 *
 * **The row's New job is the header's, aimed.** It opens the same panel with this project already
 * chosen rather than being a second form: one form per page is the rule the panel exists for. It
 * is offered only where `POST /jobs` would take the job, and `whyNoJob` is that rule — the select's
 * rule too, so a row and the list of options can never disagree. Where it would not, the row says
 * why in the select's own words. Gone while the view is stale, like every control on this page
 * that acts on capacity; kept while the kill switch is engaged, like the header's, because the
 * panel is where that refusal is said.
 */
function IdleProjects({
  columns,
  projects,
  stale,
  panelId,
  onNewJob,
}: {
  columns: FleetColumn[];
  projects: ProjectSummary[] | undefined;
  stale: boolean;
  panelId: string;
  onNewJob: (from: HTMLElement, project: string) => void;
}) {
  const titleId = useId();
  return (
    <section className="fleet-idle-group" aria-labelledby={titleId}>
      <h2 id={titleId} className="fleet-idle-title">
        Idle <span className="fleet-idle-count">· {columns.length}</span>
      </h2>
      <ul className="fleet-idle ui-rows">
        {columns.map(({ project }) => {
          const summary = projects?.find((row) => row.project_id === project.project_id);
          const why = whyNoJob(project, summary);
          const room = project.limit - project.slots.length;
          return (
            <li key={project.project_id} className="ui-rows-row fleet-idle-row">
              <Link to={`/projects/${project.project_id}/state`} className="fleet-idle-name">
                {project.project_id}
              </Link>
              {/* The cell stays when `/projects` has not answered, so the capacity still lines up
                  under the others — and stays empty, because a guessed mode on a safety control
                  is worse than none. */}
              <span className="fleet-idle-mode">
                {summary !== undefined && (
                  <>
                    <span className="sr-only">autopilot </span>
                    <StateBadge domain="autopilot" state={summary.mode} />
                  </>
                )}
              </span>
              <span className="fleet-idle-capacity">
                {project.slots.length} of {project.limit}
              </span>
              <span className="fleet-idle-room">{room > 0 ? `room for ${room}` : "no room"}</span>
              <span className="fleet-idle-act">
                {why !== null ? (
                  <span className="fleet-idle-why">{why}</span>
                ) : (
                  !stale && (
                    // Named with the project: five buttons all called "New job" are five of the
                    // same name to anybody not looking at the row they sit in.
                    <Button
                      variant="quiet"
                      intent="go"
                      aria-label={`New job in ${project.project_id}`}
                      aria-controls={panelId}
                      onClick={(event) => onNewJob(event.currentTarget, project.project_id)}
                    >
                      New job
                    </Button>
                  )
                )}
              </span>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

/**
 * One project's capacity, its safety posture, and its cards.
 *
 * The header counts `project.slots.length` and **not** the cards. A project can
 * read `1/2` with no card for one daemon tick, and that is the right behaviour:
 * capacity is what the core says it is, and counting the cards would make the
 * header agree with the screen and disagree with reality.
 *
 * The autopilot mode sits next to the name because it is the fact that decides what this project
 * may do on its own — the safety layer, where the work is. It comes from `/projects`, and when that
 * reading is absent the badge is absent: a guessed mode on a safety control is worse than none.
 */
function ProjectColumn({
  column,
  summary,
}: {
  column: FleetColumn;
  summary: ProjectSummary | undefined;
}) {
  const { project, cards, notes } = column;
  return (
    <section className="fleet-column" aria-label={`${project.project_id} column`}>
      <header className="fleet-column-head">
        <h2 className="fleet-column-title">{project.project_id}</h2>
        {summary !== undefined && (
          <span className="fleet-column-mode">
            <span className="sr-only">autopilot </span>
            <StateBadge domain="autopilot" state={summary.mode} />
          </span>
        )}
        <span className="fleet-column-count">
          {project.slots.length}/{project.limit}
        </span>
      </header>
      {/* A fact about the whole project, said once here rather than on every card. */}
      {notes.map((note) => (
        <p key={note.source} className="fleet-column-note">
          <StateBadge
            domain="collision_source"
            state={note.source === "predicted" ? "declared" : "observed"}
          />
          <StateBadge domain="collision" state={note.state} />
        </p>
      ))}
      {cards.map((card) => (
        <SlotCard key={card.key} card={card} />
      ))}
    </section>
  );
}

/* ---------------------------------------------------------------- new job -- */

/**
 * The New job panel, opened from the header.
 *
 * One form for the page, where there used to be one in every column. Four copies of the same
 * five fields were the heaviest thing on a screen whose job is to say what is in flight, and the
 * refusal they could earn landed a thousand pixels from the column it was about. One panel, a
 * project select, and the refusal beside the button that caused it.
 *
 * Not a modal: asking for a job needs neither an interruption nor a trap for focus, and the
 * columns under it stay readable while somebody writes. It stays mounted while closed so a
 * half-written prompt survives closing it, which is also what keeps the header button's
 * `aria-controls` pointing at something.
 */
function NewJobPanel({
  id,
  open,
  asked,
  columns,
  projects,
  engaged,
  picked,
  onPick,
  onClose,
}: {
  id: string;
  open: boolean;
  /** Changes on every request to open, so a second row's New job moves focus in again. */
  asked: number;
  columns: FleetColumn[];
  projects: ProjectSummary[] | undefined;
  engaged: boolean;
  picked: string;
  onPick: (project: string) => void;
  onClose: () => void;
}) {
  const prompt = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (open) prompt.current?.focus();
  }, [open, asked]);

  return (
    <div
      id={id}
      className="fleet-compose"
      hidden={!open}
      // Escape anywhere inside closes it, and focus goes back to whichever New job opened it.
      onKeyDown={(event) => {
        if (event.key === "Escape") onClose();
      }}
    >
      <Panel
        title="New job"
        aside={
          <Button variant="quiet" onClick={onClose}>
            Close
          </Button>
        }
      >
        <NewJobForm
          promptRef={prompt}
          columns={columns}
          projects={projects}
          engaged={engaged}
          picked={picked}
          onPick={onPick}
          onStarted={onClose}
        />
      </Panel>
    </div>
  );
}

/** One project as the select offers it: whether it can take a job now, and the words for why not. */
export interface ProjectChoice {
  id: string;
  room: number;
  available: boolean;
  label: string;
}

/**
 * Why `POST /jobs` would refuse a job in this project, or `null` when it would take one.
 *
 * The reasons are the route's own, read out of `core/src/job.rs` and asked before the press
 * rather than after it: `resolve_start` refuses a project whose autopilot is not `active` —
 * `shadow` included, because shadow is plan-only and a job writes to a worktree — and one with no
 * root recorded, and `concurrency::room_for` refuses a full one.
 *
 * One function, said in two places: an option in the New job select and an idle row on the page.
 * Two copies of these sentences would be two answers to one question the first time either was
 * reworded.
 *
 * When `/projects` has not answered, the mode and root are not known, and nothing is refused on
 * their account: guessing would be a claim, and the daemon still refuses what it must.
 */
export function whyNoJob(
  project: ProjectConcurrency,
  summary: ProjectSummary | undefined,
): string | null {
  const held = project.slots.length;
  if (summary?.mode === "off") return "autopilot off — jobs start only when it is active";
  if (summary?.mode === "shadow") return "in shadow — jobs start only when the autopilot is active";
  if (summary !== undefined && summary.project_root === null) return "no folder recorded";
  if (project.limit - held <= 0) return `full (${held} of ${project.limit})`;
  return null;
}

/**
 * Every project, each with the reason it cannot take a job, if there is one (`whyNoJob`). A
 * disabled option that says why is a refusal the reader never has to earn.
 */
export function projectChoices(
  columns: FleetColumn[],
  projects: ProjectSummary[] | undefined,
): ProjectChoice[] {
  return [...columns]
    .sort((left, right) => left.project.project_id.localeCompare(right.project.project_id))
    .map(({ project }) => {
      const held = project.slots.length;
      const room = project.limit - held;
      const reason = whyNoJob(
        project,
        projects?.find((row) => row.project_id === project.project_id),
      );
      return {
        id: project.project_id,
        room,
        available: reason === null,
        label:
          reason === null
            ? `${project.project_id} — room for ${room} (${held} of ${project.limit})`
            : `${project.project_id} — ${reason}`,
      };
    });
}

/** The project a new job goes to unless somebody picks: the one with the most room, then by name. */
function roomiest(choices: ProjectChoice[]): ProjectChoice | undefined {
  return choices
    .filter((choice) => choice.available)
    .sort((left, right) => right.room - left.room || left.id.localeCompare(right.id))[0];
}

/**
 * Ask a project for a job.
 *
 * `max_items` is deliberately absent: fan-out per round has a hard ceiling in
 * the daemon that nobody may raise, and a field for it would be a control that
 * silently does nothing. Budget and rounds *are* the caller's to choose, and
 * both are optional — blank means "the daemon's default", which is a different
 * request from zero.
 *
 * **The team is the fourth, and it is the only one of the four that changes how
 * the job runs rather than how long it may run for.** With one, the job gets a
 * director, a checkout per item and items that go at once; without one it is the
 * queue in a single checkout it has always been.
 */
function NewJobForm({
  promptRef,
  columns,
  projects,
  engaged,
  picked,
  onPick,
  onStarted,
}: {
  promptRef: RefObject<HTMLTextAreaElement | null>;
  columns: FleetColumn[];
  projects: ProjectSummary[] | undefined;
  engaged: boolean;
  /** The project somebody chose — in the select or on an idle row — or empty for the roomiest. */
  picked: string;
  onPick: (project: string) => void;
  onStarted: () => void;
}) {
  const create = useCreateJob();
  const teams = useTeams();
  const [prompt, setPrompt] = useState("");
  const [budget, setBudget] = useState("");
  const [rounds, setRounds] = useState("");
  const [team, setTeam] = useState("");
  const hintId = useId();
  const killId = useId();
  const whyId = useId();
  const roster = teams.data ?? [];

  const choices = projectChoices(columns, projects);
  // The pick stands even if that project fills up under it — the option turns disabled and the
  // line below says so — rather than silently moving the job to a project nobody chose.
  const choice = choices.find((one) => one.id === picked) ?? roomiest(choices);
  const blank = prompt.trim() === "";
  const unavailable = choice === undefined || !choice.available;
  // Nothing is offered while the switch is engaged: `create_job` refuses every job then, and a
  // button that can only earn a 409 is a refusal the reader has to press to hear.
  const canStart = !engaged && !unavailable;

  const describedBy =
    [blank ? hintId : null, engaged ? killId : null, unavailable ? whyId : null]
      .filter((part) => part !== null)
      .join(" ") || undefined;

  return (
    <form
      className="fleet-new-job"
      onSubmit={(event) => {
        event.preventDefault();
        if (blank || !canStart || choice === undefined || create.isPending) return;
        create.mutate(
          {
            project_id: choice.id,
            prompt: prompt.trim(),
            budget_usd: optionalNumber(budget),
            max_rounds: optionalNumber(rounds),
            // Blank is *no team*, which on this field is a real choice and not
            // an unfilled one — it is how every job in the product has always
            // run. `null` and never `""`: the daemon reads an unknown team as a
            // 422, and an empty string is an unknown team.
            team_id: team === "" ? null : team,
          },
          {
            onSuccess: () => {
              setPrompt("");
              onStarted();
            },
          },
        );
      }}
    >
      {/* The inner id is Start job's: the button points at the same sentence to say why it is not
          pressable yet. `Field` itself names the box by its label and describes it by the helper. */}
      <Field
        label="Prompt"
        helper={<span id={hintId}>Start job waits until this says what the job should work on.</span>}
      >
        <textarea
          ref={promptRef}
          className="fleet-new-job-prompt"
          rows={3}
          value={prompt}
          onChange={(event) => setPrompt(event.target.value)}
        />
      </Field>

      <div className="fleet-new-job-where">
        <Field label="Project">
          <select
            value={choice?.id ?? ""}
            aria-label="Project for the new job"
            onChange={(event) => onPick(event.target.value)}
          >
            {choices.map((one) => (
              <option key={one.id} value={one.id} disabled={!one.available}>
                {one.label}
              </option>
            ))}
          </select>
        </Field>

        {/* Gone rather than disabled in a house with no teams, for the reason
            `max_items` has no field at all: a control whose only option is the
            default is a control that does nothing, and it would advertise a
            feature whose first step is on another page. */}
        {roster.length > 0 && (
          <Field label="Team">
            <select
              value={team}
              aria-label="Team to direct the job"
              onChange={(event) => setTeam(event.target.value)}
            >
              <option value="">nobody — one checkout, one item at a time</option>
              {roster.map((row) => (
                <option key={row.id} value={row.id}>
                  {row.name} — up to {row.max_parallel} at once
                </option>
              ))}
            </select>
          </Field>
        )}
      </div>

      <div className="fleet-new-job-limits">
        <Field label="Budget $">
          <input
            value={budget}
            inputMode="decimal"
            placeholder="none"
            aria-label="Budget in dollars"
            onChange={(event) => setBudget(event.target.value)}
          />
        </Field>
        <Field label="Rounds">
          <input
            value={rounds}
            inputMode="numeric"
            placeholder="default"
            aria-label="Rounds"
            onChange={(event) => setRounds(event.target.value)}
          />
        </Field>
      </div>

      {unavailable && (
        <p className="fleet-new-job-why" id={whyId}>
          {choice === undefined
            ? "No project can take a job right now — each one says why in the list above."
            : `${choice.label} — pick a project that has room.`}
        </p>
      )}

      {engaged && (
        <p className="fleet-hold" id={killId}>
          The kill switch is engaged — the núcleo refuses every new job until it is released.
        </p>
      )}

      {/* The daemon's answer beside the button that asked, not at the top of the page. */}
      <div className="fleet-new-job-actions">
        <Button
          type="submit"
          intent="go"
          disabled={blank || !canStart || create.isPending}
          aria-describedby={describedBy}
        >
          Start job
        </Button>
        {create.isError && <NewJobRefusal error={create.error} />}
      </div>
    </form>
  );
}

/**
 * A blank field is **not** a zero.
 *
 * `budget_usd: null` is "no job budget, use the global one"; `budget_usd: 0` would
 * be a job that may spend nothing and dies on its first turn. Anything that is
 * not a finite number reads as blank rather than as a value, because a typo
 * silently becoming `NaN` in a JSON body is a 400 nobody can explain.
 */
function optionalNumber(raw: string): number | null {
  const trimmed = raw.trim();
  if (trimmed === "") return null;
  const value = Number(trimmed);
  return Number.isFinite(value) ? value : null;
}

/**
 * Why `POST /jobs` said no.
 *
 * **Two of its refusals share a 409**, and they are different facts with
 * different remedies: the kill switch is engaged, or that project is already
 * full. The status cannot tell them apart, and neither can `client.ts`'s
 * status-derived code — so the daemon's own sentence is what is shown, because
 * on this route the daemon wrote a better one than we would. Mapping `conflict`
 * to a sentence of our own here would render both refusals identically, which
 * is the exact collapse this page is meant not to make.
 *
 * The 422s — a team that does not exist, a project that is not active — need
 * nothing added here for the same reason: the daemon names what it could not
 * accept, and `daemonProse` puts that sentence on screen whatever the code.
 */
function NewJobRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — the job was not started</ErrorNote>;
  }
  return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
}

function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const prose = refusal.detail.trim();
  return prose === "" || prose === refusal.code ? {} : { [refusal.code]: prose };
}

/**
 * The capacity reading never arrived at all — so there is nothing to draw.
 *
 * The owner reads an error here as a bug report, so it carries what one needs: the route that
 * failed, when it last failed, and what the transport said. Without the route "did not answer"
 * sends somebody to guess which of this page's six polls it was.
 */
function CapacityError({ error, at }: { error: unknown; at: number }) {
  // Wrapped so the time takes the note's own colour: the reading's faint grey is under the AA
  // floor on the error note's red fill.
  const when =
    at > 0 ? (
      <span className="fleet-error-time">
        {" "}
        · <RelativeTime at={new Date(at).toISOString()} />
      </span>
    ) : null;
  if (isApiRefusal(error)) {
    return (
      <div className="fleet-error">
        <RefusalNote refusal={error} sentences={daemonProse(error)} />
        <p className="fleet-error-where">
          <code>GET /concurrency</code>
          {when}
        </p>
      </div>
    );
  }
  const said = error instanceof Error && error.message.trim() !== "" ? error.message : null;
  // One span inside the note: `.ui-note` is a flex row, and loose inline pieces each became a
  // flex item with a gap between them, so the sentence broke apart at every `<code>`.
  return (
    <ErrorNote>
      <span>
        the núcleo did not answer <code>GET /concurrency</code> — nothing is known about the fleet
        {when}
        {said !== null && (
          <>
            {" "}
            · <code>{said}</code>
          </>
        )}
      </span>
    </ErrorNote>
  );
}

/** A write that did not take, said next to the page rather than in a corner. */
function MutationNote({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — {what}</ErrorNote>;
}

/**
 * The ceiling line under the spend.
 *
 * `limit_usd === null` is **no ceiling** and is never rendered as a zero: a
 * ceiling of `0.00` stops all autonomous work, no ceiling stops none of it.
 */
function ceiling(spend: BudgetView | undefined): string | undefined {
  if (spend === undefined) return undefined;
  if (spend.limit_usd === null) return `no ceiling · ${spend.period}`;
  return `of ${usd(spend.limit_usd)} · ${spend.period}`;
}

/* --------------------------------------------------------------- headline -- */

interface HeadlineFacts {
  capacity: Concurrency | undefined;
  totals: Exceptions;
  engaged: boolean;
  stale: boolean;
  waiting: number | undefined;
}

function counted(count: number, one: string, many: string): string {
  return `${count} ${count === 1 ? one : many}`;
}

/**
 * The page's answer to "is everything fine?", worst fact first.
 *
 * A ladder in the order of what it asks of the reader, the way Home's is. Faults lead — a slot
 * held by nothing, two trees measured writing the same file — and they are set in the wrong-fact
 * colour, as Home sets its own. Then an item that did not merge, in plain words and not in that
 * colour: `core/src/job.rs` puts a conflicted item down rather than failing it ("a conflict is a
 * question about two pieces of work, not a verdict on either") and the queue starts the
 * resolution run itself, so it is work that stopped, not a fault. Then what is waiting on a
 * decision, which is a door to Waiting; then what a rule is holding back; then the kill switch,
 * which is not a fault but is the reason nothing new starts; then a stale reading, because every
 * number after it is the last good one rather than the current one. The capacity comes last, and
 * when nothing above it is true it is the whole sentence: that is the "all is well".
 *
 * `ReactNode` and not `string` because of the colour and the door. Every clause is short and
 * counted; the page never says "attention" or "warning" — it says what is so.
 */
function headline({ capacity, totals, engaged, stale, waiting }: HeadlineFacts): ReactNode {
  if (capacity === undefined) return undefined;
  const clauses: ReactNode[] = [];

  if (totals.leaked > 0) {
    clauses.push(
      <span className="ui-wrong">
        {counted(totals.leaked, "slot held by nothing", "slots held by nothing")}, awaiting
        reconciliation
      </span>,
    );
  }
  if (totals.collided > 0) {
    clauses.push(
      <span className="ui-wrong">
        {counted(totals.collided, "observed overlap", "observed overlaps")} between trees
      </span>,
    );
  }
  if (totals.conflicted > 0) {
    clauses.push(`${counted(totals.conflicted, "item", "items")} did not merge`);
  }
  if (totals.awaiting > 0) {
    clauses.push(
      <Link to="/waiting">{counted(totals.awaiting, "job", "jobs")} awaiting approval</Link>,
    );
  }
  if (totals.excluded > 0) {
    clauses.push(`${counted(totals.excluded, "job", "jobs")} held by an exclusion`);
  }
  if (engaged) clauses.push("the kill switch is engaged — no new job starts");
  if (stale) clauses.push("the view is stale");

  const { held, limit } = capacity.house;
  const room = limit - held;
  clauses.push(
    held === 0
      ? `nothing in flight; room for ${limit}`
      : `${held} in flight of ${limit} across all projects; ${room <= 0 ? "no room for another" : `room for ${room} more`}`,
  );
  if (waiting !== undefined && waiting > 0) {
    clauses.push(`${counted(waiting, "proposal", "proposals")} open across the roster`);
  }

  return clauses.map((clause, index) => (
    <Fragment key={index}>
      {index > 0 && "; "}
      {clause}
    </Fragment>
  ));
}
