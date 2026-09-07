import { useState } from "react";
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
} from "../data/fleet";
import { useBudget, useProjects, type BudgetView } from "../data/system";
import { useTeams } from "../data/teams";
import {
  FleetActionsProvider,
  FleetCanvas,
  SlotCard,
  type FleetActions,
} from "../canvas/FleetCanvas";
import { buildFleet, loadLayout, saveLayout, type FleetColumn, type Layout } from "../canvas/model";
import { Button, ErrorNote, PageHeader, Panel, RefusalNote, StaleNote, StatCard } from "../ui";
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
 * **The kill switch is not duplicated here.** It lives in the frame, on every
 * page; a second copy is a second thing to keep in step and a second thing to
 * be wrong.
 */
export function Fleet() {
  const concurrency = useConcurrency();
  const jobs = useLiveJobs();
  const runs = useLiveRuns();
  const exclusions = useExclusions();
  const requests = useExclusionRequests();
  const projects = useProjects();
  const budget = useBudget();

  const [view, setView] = useState<FleetView>("columns");
  /** One item list open at a time, page-wide: two open cards is two polls. */
  const [openJob, setOpenJob] = useState<number | null>(null);
  /**
   * Read once, at mount, and held here rather than in the canvas — an
   * arrangement made on the surface has to survive a switch to the columns and
   * back, and the canvas unmounts when it is not the open view.
   */
  const [layout, setLayout] = useState<Layout>(loadLayout);

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
  const waiting = projects.data?.reduce((total, project) => total + project.open_proposals, 0);

  const actions: FleetActions = {
    openJob,
    toggleJob: (jobId) => setOpenJob((current) => (current === jobId ? null : jobId)),
    cancel: (owner) => cancel.mutate(owner),
    lift: (exclusionId) => revoke.mutate(exclusionId),
  };

  return (
    <FleetActionsProvider actions={actions}>
      <PageHeader
        title="Fleet"
        headline={headline(capacity, waiting)}
        actions={<ViewSwitch view={view} onChange={setView} />}
      />

      <div className="fleet-meter">
        <StatCard
          label="In flight"
          value={capacity === undefined ? undefined : `${capacity.house.held}/${capacity.house.limit}`}
          detail="slots held across the house"
        />
        <StatCard
          label="Window spend"
          value={budget.data === undefined ? undefined : `$${budget.data.window_spend_usd.toFixed(2)}`}
          detail={ceiling(budget.data)}
        />
        <StatCard
          label="Waiting on you"
          value={waiting}
          detail="proposals open across the roster"
        />
      </div>

      {stale && <StaleNote dataUpdatedAt={concurrency.dataUpdatedAt} />}
      {concurrency.isError && capacity === undefined && <CapacityError error={concurrency.error} />}
      {cancel.isError && <MutationNote error={cancel.error} what="that could not be cancelled" />}
      {revoke.isError && <MutationNote error={revoke.error} what="that rule could not be lifted" />}
      {propose.isError && <MutationNote error={propose.error} what="that pair could not be asked about" />}

      {capacity !== undefined && capacity.projects.length === 0 && (
        <Panel title="Nothing to run yet">
          <p className="fleet-empty">
            No project is registered with the núcleo. Add one on Projects, and its column appears
            here — empty, at <code>0/N</code>, which is a different fact from not being there.
          </p>
        </Panel>
      )}

      {view === "canvas" ? (
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
        <div className="fleet-columns">
          {model.columns.map((column) => (
            <ProjectColumn key={column.project.project_id} column={column} canStart={!stale} />
          ))}
        </div>
      )}
    </FleetActionsProvider>
  );
}

/** Which of the two ways of looking at the fleet is open. */
type FleetView = "columns" | "canvas";

/**
 * `aria-pressed` and not a shade alone: which of the two is open has to be
 * announced, not only coloured.
 */
function ViewSwitch({ view, onChange }: { view: FleetView; onChange: (next: FleetView) => void }) {
  return (
    <div className="fleet-views" role="group" aria-label="How to look at the fleet">
      <Button aria-pressed={view === "columns"} onClick={() => onChange("columns")}>
        Columns
      </Button>
      <Button aria-pressed={view === "canvas"} onClick={() => onChange("canvas")}>
        Canvas
      </Button>
    </div>
  );
}

/**
 * One project's capacity, its cards, and the action that fills a slot.
 *
 * The header counts `project.slots.length` and **not** the cards. A project can
 * read `1/2` with no card for one daemon tick, and that is the right behaviour:
 * capacity is what the core says it is, and counting the cards would make the
 * header agree with the screen and disagree with reality.
 */
function ProjectColumn({ column, canStart }: { column: FleetColumn; canStart: boolean }) {
  const { project, cards } = column;
  return (
    <section className="fleet-column" aria-label={`${project.project_id} column`}>
      <header className="fleet-column-head">
        <h2 className="fleet-column-title">{project.project_id}</h2>
        <span className="fleet-column-count">
          {project.slots.length}/{project.limit}
        </span>
      </header>
      {cards.length === 0 && <p className="fleet-column-empty">nothing in flight</p>}
      {cards.map((card) => (
        <SlotCard key={card.key} card={card} />
      ))}
      {/* Gone rather than disabled while the view is stale: the capacity on
          screen is not the daemon's, so this control would be asking for room
          nobody can vouch for. `StaleNote` above says why it went. */}
      {canStart && <NewJobForm projectId={project.project_id} />}
    </section>
  );
}

/**
 * Ask this project for a job.
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
 * queue in a single checkout it has always been. The daemon has taken `team_id`
 * on this route since the parallel work landed and nothing sent it, so the
 * feature was reachable only by writing JSON by hand or a `graph:` rule into a
 * config file.
 */
function NewJobForm({ projectId }: { projectId: string }) {
  const create = useCreateJob();
  const teams = useTeams();
  const [prompt, setPrompt] = useState("");
  const [budget, setBudget] = useState("");
  const [rounds, setRounds] = useState("");
  const [team, setTeam] = useState("");
  const roster = teams.data ?? [];

  return (
    <form
      className="fleet-new-job"
      onSubmit={(event) => {
        event.preventDefault();
        if (prompt.trim() === "" || create.isPending) return;
        create.mutate(
          {
            project_id: projectId,
            prompt: prompt.trim(),
            budget_usd: optionalNumber(budget),
            max_rounds: optionalNumber(rounds),
            // Blank is *no team*, which on this field is a real choice and not
            // an unfilled one — it is how every job in the product has always
            // run. `null` and never `""`: the daemon reads an unknown team as a
            // 422, and an empty string is an unknown team.
            team_id: team === "" ? null : team,
          },
          { onSuccess: () => setPrompt("") },
        );
      }}
    >
      <input
        className="fleet-new-job-prompt"
        value={prompt}
        placeholder="what should it work on"
        aria-label={`What to work on in ${projectId}`}
        onChange={(event) => setPrompt(event.target.value)}
      />
      {/* Gone rather than disabled in a house with no teams, for the reason
          `max_items` has no field at all: a control whose only option is the
          default is a control that does nothing, and it would advertise a
          feature whose first step is on another page. */}
      {roster.length > 0 && (
        <label className="fleet-new-job-field fleet-new-job-team">
          <span>Team</span>
          <select
            value={team}
            aria-label={`Team to direct the job in ${projectId}`}
            onChange={(event) => setTeam(event.target.value)}
          >
            <option value="">nobody — one checkout, one item at a time</option>
            {roster.map((row) => (
              <option key={row.id} value={row.id}>
                {row.name} — up to {row.max_parallel} at once
              </option>
            ))}
          </select>
        </label>
      )}
      <div className="fleet-new-job-fields">
        <label className="fleet-new-job-field">
          <span>Budget $</span>
          <input
            value={budget}
            inputMode="decimal"
            placeholder="none"
            aria-label={`Budget in dollars for ${projectId}`}
            onChange={(event) => setBudget(event.target.value)}
          />
        </label>
        <label className="fleet-new-job-field">
          <span>Rounds</span>
          <input
            value={rounds}
            inputMode="numeric"
            placeholder="default"
            aria-label={`Rounds for ${projectId}`}
            onChange={(event) => setRounds(event.target.value)}
          />
        </label>
        <Button type="submit" disabled={create.isPending} intent="go">
          New job
        </Button>
      </div>
      {create.isError && <NewJobRefusal error={create.error} />}
    </form>
  );
}

/**
 * A blank field is **not** a zero.
 *
 * `budget_usd: null` is "no job budget, use the house's"; `budget_usd: 0` would
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
 * The third is the 422 for a team that does not exist, and it needs nothing
 * added here for the same reason: the daemon names the team it could not find,
 * and `daemonProse` puts that sentence on screen whatever the code.
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

/** The capacity reading never arrived at all — so there is nothing to draw. */
function CapacityError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the fleet</ErrorNote>;
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
  return `of $${spend.limit_usd.toFixed(2)} · ${spend.period}`;
}

/** One derived sentence about how full the house is. */
function headline(capacity: Concurrency | undefined, waiting: number | undefined): string | undefined {
  if (capacity === undefined) return undefined;
  const room = capacity.house.limit - capacity.house.held;
  const held =
    capacity.house.held === 0
      ? "nothing in flight"
      : `${capacity.house.held} in flight of ${capacity.house.limit}`;
  const left = room <= 0 ? "the house is full" : `room for ${room} more`;
  if (waiting === undefined) return `${held}; ${left}`;
  return waiting === 0 ? `${held}; ${left}` : `${held}; ${left}; ${waiting} waiting on you`;
}
