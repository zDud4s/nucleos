import { useCallback, useEffect, useRef, useState } from "react";

import {
  cancelJob,
  cancelRun,
  createJob,
  getBudget,
  getConcurrency,
  getJobs,
  getLiveRuns,
  getProjects,
  type Budget,
  type ConnectionState,
  type Concurrency,
  type HeldSlot,
  type Job,
  type ProjectConcurrency,
  type ProjectSummary,
  type RunSearchResult,
} from "./api";
// `fleet-derive` and not `fleet`: a module named `fleet.ts` beside this `Fleet.tsx` resolves to
// whichever the filesystem answers with on Windows, and the import silently picks the component.
// Same shape as the `Calendar.tsx` / `calendar-grid.ts` pair already in this directory.
import {
  collisionBadges,
  orderColumns,
  slotDetail,
  type CollisionBadge,
  type SlotDetail,
} from "./fleet-derive";
import { Button, ConfirmButton, ErrorNote } from "./ui";

interface FleetProps {
  token: string | null;
  connection: ConnectionState;
  killEngaged: boolean | null;
  /** A run's card has no graph — it leads to the Runs tab. */
  onOpenRuns: () => void;
}

/** Identifies an owner across ticks, so a cancelled card stays gone. */
function ownerKey(slot: HeldSlot): string {
  return `${slot.owner_kind}:${slot.owner_id}`;
}

/**
 * The fleet canvas: a column per project, a card per SLOT.
 *
 * Per slot and not per job, and the difference is not cosmetic. An autonomous worktree run holds a
 * slot too, so a column that counted jobs would say `0/2` about a project that is going to refuse
 * the next one with a 409 — and would offer the button that asks for it.
 *
 * The only component at this level that fetches. Five calls per 3-second tick, four of which other
 * tabs already make; if it ever hurts, the answer is for `GET /concurrency` to absorb the others,
 * not a new `/fleet`.
 */
export default function Fleet({ token, connection, killEngaged, onOpenRuns }: FleetProps) {
  const [concurrency, setConcurrency] = useState<Concurrency | null>(null);
  const [jobs, setJobs] = useState<Job[] | null>(null);
  const [runs, setRuns] = useState<RunSearchResult[] | null>(null);
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [budget, setBudget] = useState<Budget | null>(null);
  const [stale, setStale] = useState(false);
  const [lastGood, setLastGood] = useState<string | null>(null);
  /** Owners whose card the user sent away, keyed `"job:41"`. */
  const [cancelled, setCancelled] = useState<Set<string>>(new Set());

  const inFlight = useRef(0);
  const batchSeq = useRef(0);

  const refresh = useCallback(
    async (background = false) => {
      if (token === null || connection !== "connected") return;
      // One batch at a time.
      if (background && inFlight.current > 0) return;
      const batch = (batchSeq.current += 1);
      inFlight.current += 1;
      try {
        const [nextConcurrency, nextJobs, nextRuns, nextProjects, nextBudget] = await Promise.all([
          getConcurrency(token),
          getJobs(token, undefined, { live: true }),
          getLiveRuns(token),
          getProjects(token),
          getBudget(token),
        ]);
        if (batch !== batchSeq.current) return;
        // The authority is written down only when it answers. On a failure the cards from the last
        // good tick stay and the view marks itself stale — blank reads as "there is room".
        if (nextConcurrency !== null) {
          setConcurrency(nextConcurrency);
          setLastGood(new Date().toISOString());
        }
        setStale(nextConcurrency === null);
        // These may stay null: the card is drawn all the same, without a description.
        setJobs(nextJobs);
        setRuns(nextRuns);
        // Blank, never zero: zero is a claim a failed call did not make.
        setProjects(nextProjects);
        setBudget(nextBudget);
      } finally {
        inFlight.current -= 1;
      }
    },
    [connection, token],
  );

  useEffect(() => {
    void refresh();
    const id = setInterval(() => void refresh(true), 3000);
    return () => clearInterval(id);
  }, [refresh]);

  const cancel = useCallback(
    async (slot: HeldSlot) => {
      if (token === null) return;
      setCancelled((current) => new Set(current).add(ownerKey(slot)));
      const result =
        slot.owner_kind === "job"
          ? await cancelJob(token, slot.owner_id)
          : await cancelRun(token, slot.owner_id);
      // A 409 is the job having ended between the render and the click — not a failure, and the
      // card leaves all the same. Only a NETWORK failure brings it back, because then it is unknown
      // whether anything happened at all.
      //
      // `fault === "unreachable"` is the only way to tell the two apart: `faultForStatus` maps 409
      // to `"failed"`, and only the `catch` arm produces `"unreachable"` with `status: 0`.
      if (!result.ok && result.fault === "unreachable") {
        setCancelled((current) => {
          const next = new Set(current);
          next.delete(ownerKey(slot));
          return next;
        });
      }
      // In the foreground on purpose: it bumps `batchSeq` and drops the batch in flight.
      await refresh();
    },
    [refresh, token],
  );

  // Keeps the set from growing forever: once the daemon stops reporting the slot, the entry has no
  // work left to do.
  useEffect(() => {
    if (concurrency === null) return;
    const held = new Set(concurrency.projects.flatMap((project) => project.slots.map(ownerKey)));
    setCancelled((current) => {
      const next = new Set([...current].filter((key) => held.has(key)));
      return next.size === current.size ? current : next;
    });
  }, [concurrency]);

  const proposals =
    projects === null
      ? null
      : projects.reduce((total, project) => total + project.open_proposals, 0);
  // Stale hides the start action rather than letting it fail after the click: the capacity on
  // screen is no longer the daemon's.
  const canStart = !stale && killEngaged !== true && token !== null;

  return (
    <section className="fleet">
      <HouseMeter
        house={concurrency?.house ?? null}
        budget={budget}
        proposals={proposals}
        staleSince={stale ? lastGood : null}
      />
      {concurrency !== null && concurrency.projects.length === 0 ? (
        <p className="empty">
          No projects are registered yet. Add one on the Projects tab, and its column appears here.
        </p>
      ) : (
        <div className="fleet-columns">
          {orderColumns(concurrency?.projects ?? []).map((project) => (
            <ProjectColumn
              key={project.project_id}
              project={project}
              jobs={jobs}
              runs={runs}
              token={token ?? ""}
              canStart={canStart}
              cancelled={cancelled}
              onCancel={cancel}
              onOpenRuns={onOpenRuns}
              refresh={refresh}
            />
          ))}
        </div>
      )}
    </section>
  );
}

interface HouseMeterProps {
  house: { limit: number; held: number } | null;
  budget: Budget | null;
  /** Proposals to review, summed from the roster. `null` when the roster did not come: blank, not zero. */
  proposals: number | null;
  /** When the most recent good reading was taken, or `null` if the view is up to date. */
  staleSince: string | null;
}

/**
 * What the header says about money.
 *
 * The shape is checked rather than assumed, and that is not paranoia about our own types: this
 * header renders on every tick of the tab that is meant to be the fleet's authority, and a daemon
 * one version out of step — the same reason `ProjectSummary.withheld_classes_ready` is optional —
 * would otherwise take the whole canvas down over a number nobody was reading. Unreadable reads as
 * unavailable, the same way capacity does.
 */
function budgetLine(budget: Budget | null): string {
  if (budget === null || typeof budget.window_spend_usd !== "number") return "budget unavailable";
  const spent = `$${budget.window_spend_usd.toFixed(2)}`;
  return typeof budget.limit_usd === "number"
    ? `${spent} of $${budget.limit_usd.toFixed(2)}`
    : `${spent} spent`;
}

/** Occupancy across the house, the budget, and the proposals waiting. Pure. */
export function HouseMeter({ house, budget, proposals, staleSince }: HouseMeterProps) {
  return (
    <header className="fleet-meter">
      <span className="fleet-house">
        {house === null ? "—" : `${house.held}/${house.limit}`} in flight
      </span>
      <span className="fleet-budget">{budgetLine(budget)}</span>
      <span className="fleet-proposals">
        {proposals === null ? "waiting —" : `waiting ${proposals}`}
      </span>
      {staleSince !== null && (
        // `role="status"` is not decoration: it is how a screen reader learns the view stopped being
        // current without having to re-read it.
        <span className="fleet-stale" role="status">
          Stale — last read <time dateTime={staleSince}>{staleSince}</time>
        </span>
      )}
    </header>
  );
}

interface ProjectColumnProps {
  project: ProjectConcurrency;
  jobs: Job[] | null;
  runs: RunSearchResult[] | null;
  token: string;
  /** `false` hides the *new job* action: the view is stale, or the kill switch is engaged. */
  canStart: boolean;
  cancelled: Set<string>;
  onCancel: (slot: HeldSlot) => Promise<void>;
  onOpenRuns: () => void;
  refresh: () => Promise<void>;
}

/**
 * The `n/limit` header, the cards, and the action to start.
 *
 * The header counts `project.slots.length` and NOT the cards. A project can read `1/2` with zero
 * cards for one daemon tick, and that is the right behaviour: capacity is what the core says it is.
 * Counting the cards would make the header agree with the screen and disagree with reality.
 */
export function ProjectColumn({
  project,
  jobs,
  runs,
  token,
  canStart,
  cancelled,
  onCancel,
  onOpenRuns,
  refresh,
}: ProjectColumnProps) {
  const drawn = project.slots.filter((slot) => !cancelled.has(ownerKey(slot)));

  return (
    <section className="fleet-column">
      <header>
        <h2>{project.project_id}</h2>
        <span className="fleet-count">
          {project.slots.length}/{project.limit}
        </span>
      </header>
      {drawn.map((slot) => (
        <SlotCard
          key={ownerKey(slot)}
          slot={slot}
          detail={slotDetail(slot, jobs, runs)}
          badges={collisionBadges(project, { kind: slot.owner_kind, id: slot.owner_id })}
          token={token}
          onCancel={() => void onCancel(slot)}
          onOpenRuns={onOpenRuns}
        />
      ))}
      {canStart && <NewJob projectId={project.project_id} token={token} onStarted={refresh} />}
    </section>
  );
}

interface SlotCardProps {
  slot: HeldSlot;
  detail: SlotDetail;
  badges: CollisionBadge[];
  /** For the `JobGraph` this card mounts when it opens. */
  token: string;
  onCancel: () => void;
  onOpenRuns: () => void;
}

/**
 * ONE slot.
 *
 * `wait_reason` and `round`/`max_rounds` appear only when the owner is a job: they are columns of
 * `jobs`, and `RunSearchResult` has no equivalent. Showing them as zero for a run would be
 * inventing a number.
 *
 * Pure, **except** for the `JobGraph` it mounts when the card opens — hence the `token`.
 *
 * **It has no "cancelling" state.** Cancelling takes the owner out of the column at the instant of
 * the click, so a card halfway through a cancel does not exist to be drawn.
 */
export function SlotCard({ slot, detail, badges, onCancel, onOpenRuns }: SlotCardProps) {
  const modifier =
    detail.kind === "unknown" ? " is-unknown" : detail.kind === "orphaned" ? " is-orphaned" : "";

  return (
    <article className={`slot-card${modifier}`}>
      <header>
        <span className="slot-number">slot {slot.slot}</span>
        <span className="slot-owner">
          {slot.owner_kind} {slot.owner_id}
        </span>
      </header>
      {detail.kind === "job" && (
        <>
          <p className="slot-status">
            {detail.job.status}
            {detail.job.wait_reason !== null && ` — ${detail.job.wait_reason}`}
          </p>
          <p className="slot-rounds">
            round {detail.job.round + 1} of {detail.job.max_rounds}
          </p>
        </>
      )}
      {detail.kind === "run" && (
        <>
          <p className="slot-status">{detail.run.status}</p>
          <p className="slot-prompt">{detail.run.prompt_excerpt}</p>
          <Button size="sm" onClick={onOpenRuns}>
            Open in Runs
          </Button>
        </>
      )}
      {/* The same words for two different reasons — a listing that failed and a listing that came
          back full — because from here they are the same fact: nothing described this owner. Only
          the orphaned line changes its word, because that one is a sign of a defect. */}
      {detail.kind === "unknown" && <p className="slot-status">detail unavailable</p>}
      {detail.kind === "orphaned" && <p className="slot-status">slot awaiting reconciliation</p>}
      {badges.map((badge) => (
        <p
          key={badge.source}
          className={`collide-badge is-${badge.source}${badge.state === "not_measured" ? " is-unmeasured" : ""}`}
        >
          {/* A label and not colour alone: the two sources have to be distinguishable by anyone. */}
          <span className="collide-source">{badge.source}</span>{" "}
          {badge.state === "not_measured"
            ? "not measured"
            : `also touched by ${badge.others
                .map((other) => `${other.kind} ${other.id}`)
                .join(", ")}: ${badge.paths.join(", ")}`}
        </p>
      ))}
      <ConfirmButton
        variant="danger"
        size="sm"
        confirmLabel="Confirm cancel?"
        onConfirm={onCancel}
      >
        Cancel
      </ConfirmButton>
    </article>
  );
}

/**
 * The action that starts a job in this project.
 *
 * Traced from Autopilot's form: `createJob` answers with the daemon's own SENTENCE on a 409, and
 * that sentence is what is shown. `NoRoom` already separates full from kill switch; the screen does
 * not put them back together into a message of its own.
 */
function NewJob({
  projectId,
  token,
  onStarted,
}: {
  projectId: string;
  token: string;
  onStarted: () => Promise<void>;
}) {
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  async function start() {
    setBusy(true);
    setFailed(null);
    const outcome = await createJob(token, { projectId, prompt: prompt.trim() });
    setBusy(false);
    if (!outcome.ok) {
      setFailed(outcome.reason);
      return;
    }
    setPrompt("");
    await onStarted();
  }

  return (
    <form
      className="fleet-new-job"
      onSubmit={(event) => {
        event.preventDefault();
        if (prompt.trim() === "" || busy) return;
        void start();
      }}
    >
      <input
        value={prompt}
        placeholder="what should it work on"
        aria-label={`What to work on in ${projectId}`}
        onChange={(event) => setPrompt(event.target.value)}
      />
      <Button type="submit" size="sm" disabled={busy}>
        New job
      </Button>
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
    </form>
  );
}
