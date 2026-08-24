import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import { NewDepartment } from "../team/Charter";
import { RosterMatrix, headcountOf, specialistsOf } from "../team/RosterMatrix";
import {
  GRANTABLE_ACTIONS,
  teamRunIsAlive,
  useOpenTeamActions,
  useTeamRun,
  useTeams,
  useTeamRuns,
  useTeamTriggers,
  useTriggerNext,
  type TeamAction,
  type TeamGrant,
  type TeamRun,
  type TeamTrigger,
  type TeamView,
} from "../data/teams";
import {
  Badge,
  Button,
  ErrorNote,
  LimitChip,
  Meter,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  Sparkline,
  StaleNote,
  StateBadge,
  Teach,
  Who,
  usd,
} from "../ui";
import "./teams.css";

/**
 * Teams — the console. `/teams` and nothing else; `/teams/$teamId` is the bench
 * (`team/Bench.tsx`) and is a whole page rather than a panel added below this
 * one.
 *
 * The page this replaced served both routes at once in the `Council` pattern
 * and stacked six panels, three forms and two copies of the same eleven-field
 * editor — the editor opened before you could see which departments existed.
 * The split is the whole design: this altitude answers *what is there and how
 * is it doing*, and the bench answers *what do I do about this one*.
 *
 * Three readings this page deliberately does not draw, each because the daemon
 * does not answer it:
 *
 * - **Money per department.** There is none. `teams.budget_usd` is the ceiling
 *   of ONE task (`core/src/team.rs:2078` compares it against
 *   `spend_of(run.id)`), plus an equal one over a chain of tasks
 *   (`spend_of_tree`, `team.rs:2138`). Nothing accumulates per department over
 *   time, and the machine's own spend is already permanent in the sidebar.
 * - **An exact count of what is waiting.** `GET /team-actions` answers `pending`
 *   AND `working` (`team.rs:3570`), while the ceiling counts only `pending` with
 *   an undecided proposal (`open_actions_of`, `team.rs:1764`). Anything counted
 *   here is an upper bound, so the word is "waiting" and never "exactly N".
 * - **A time axis.** `GET /team-runs` is a hard `LIMIT 100` across every
 *   department with no paging, so every pulse says how many runs it is drawn
 *   from and never how many days.
 *
 * The matrix costs nothing extra: `GET /teams` already returns every department
 * with its roster and its grants (`list_teams`, `team.rs:445-462`).
 */
export function Teams() {
  const teams = useTeams();
  const runs = useTeamRuns();
  const triggers = useTeamTriggers();
  const actions = useOpenTeamActions();
  const [creating, setCreating] = useState(false);

  const rows = teams.data ?? [];
  const allRuns = runs.data ?? [];
  const allTriggers = triggers.data ?? [];
  const openActions = actions.data ?? [];
  const stale = teams.isError && teams.data !== undefined;

  return (
    <>
      <PageHeader
        title="Teams"
        headline={headlineFor(rows, allRuns, openActions, teams.data !== undefined)}
        actions={
          <Button intent="go" onClick={() => setCreating((open) => !open)} aria-expanded={creating}>
            {creating ? "Close" : "New department"}
          </Button>
        }
      />

      {stale && <StaleNote dataUpdatedAt={teams.dataUpdatedAt} />}
      {teams.isError && teams.data === undefined && <ListError error={teams.error} />}

      {/* Closed by default, and that is the point: the old page opened an
          eleven-field form above a list you had not read yet. */}
      {creating && (
        <Panel title="New department">
          {/* The very form the Charter tab is, minus the drift guard — there is
              nothing to have drifted from yet. One editor, one place. */}
          <NewDepartment />
        </Panel>
      )}

      <Panel title="Who works where">
        <RosterMatrix teams={rows} />
      </Panel>

      {rows.length === 0 ? (
        <Teach title="No department yet">
          <p>
            A department is a permanent unit — Finance, Marketing, Security — not a task. It has a
            director, a roster of specialists, what it may do on its own, and the ceilings every
            task of its runs under. Tasks and routines belong to it and are set up inside it.
          </p>
        </Teach>
      ) : (
        <ul className="teams-grid" aria-label="Departments">
          {rows.map((team) => (
            <li key={team.id}>
              <DepartmentCard
                team={team}
                teams={rows}
                runs={allRuns.filter((run) => run.team_id === team.id)}
                triggers={allTriggers.filter((rule) => rule.team_id === team.id)}
                waiting={waitingFor(team.id, openActions, allRuns)}
              />
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

/* -------------------------------------------------------------- readings -- */

/** One derived sentence about every department at once. */
function headlineFor(
  rows: TeamView[],
  runs: TeamRun[],
  actions: TeamAction[],
  answered: boolean,
): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0) return "no department yet";

  const parts = [`${rows.length} ${rows.length === 1 ? "department" : "departments"}`];
  const people = specialistsOf(rows).length;
  parts.push(`${people} ${people === 1 ? "specialist" : "specialists"}`);

  const live = runs.filter((run) => teamRunIsAlive(run.state)).length;
  parts.push(live === 0 ? "none at work" : `${live} at work`);

  // "waiting", never a count presented as the daemon's own — see the module header.
  const known = new Set(runs.map((run) => run.team_id));
  const waiting = actions.filter((action) => known.has(runTeam(action, runs) ?? "")).length;
  if (waiting > 0) parts.push(`${waiting} waiting on you`);

  return parts.join(" · ");
}

/**
 * Which department an action belongs to.
 *
 * `GET /team-actions` carries `team_run_id` and nothing else, so the department
 * has to come back through the run list — which is the newest hundred across
 * every department. An action whose run has fallen off the end of that window
 * cannot be attributed, and is counted nowhere rather than counted wrongly.
 */
function runTeam(action: TeamAction, runs: TeamRun[]): string | null {
  return runs.find((run) => run.id === action.team_run_id)?.team_id ?? null;
}

function waitingFor(teamId: string, actions: TeamAction[], runs: TeamRun[]): number {
  return actions.filter((action) => runTeam(action, runs) === teamId).length;
}

function ListError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the departments</ErrorNote>;
}

/* ------------------------------------------------------------ the card -- */

/**
 * One department as an instrument.
 *
 * It separates what the department **is** — remit, staff, powers, ceilings —
 * from what it is **doing** right now, because those are two different
 * questions and the old row answered neither: it gave the director's id and a
 * live-run count the same weight as five ceilings written as bare text.
 *
 * The powers appear here rather than only on the bench, because "what can this
 * one do without asking me" is a question you ask at a glance.
 */
function DepartmentCard({
  team,
  teams,
  runs,
  triggers,
  waiting,
}: {
  team: TeamView;
  teams: TeamView[];
  runs: TeamRun[];
  triggers: TeamTrigger[];
  waiting: number;
}) {
  const live = runs.filter((run) => teamRunIsAlive(run.state));
  const armed = triggers.filter((rule) => rule.enabled !== 0);
  const staff = [...new Set([team.director_agent_id, ...team.members])].filter((id) => id !== "");

  return (
    <article className="teams-card" aria-label={team.name}>
      <header className="teams-card-head">
        <Link className="teams-card-name" to={`/teams/${team.id}`}>
          {team.name}
        </Link>
        <CardState live={live.length} waiting={waiting} />
        <Powers grants={team.grants} />
      </header>

      <p className="teams-card-remit">{team.mission}</p>

      <div className="teams-card-staff">
        <p className="teams-card-label">
          Staff <span className="teams-card-count">{headcountOf(team)}</span>
        </p>
        {staff.length === 0 ? (
          <p className="teams-empty">nobody yet — a task cannot start without a roster.</p>
        ) : (
          <div className="teams-card-people">
            {staff.map((id) => (
              <Who
                key={id}
                id={id}
                leads={id === team.director_agent_id}
                shared={teams.filter((other) => other.members.includes(id) || other.director_agent_id === id).length > 1}
              />
            ))}
          </div>
        )}
      </div>

      {live.length > 0 && (
        <div className="teams-card-live">
          <p className="teams-card-label">Work in flight</p>
          {live.map((run) => (
            <LiveTask key={run.id} run={run} ceiling={team.budget_usd} />
          ))}
        </div>
      )}

      <div className="teams-card-occupancy">
        <p className="teams-card-label">Occupancy</p>
        <Meter label="at work" value={live.length} ceiling={team.max_live_runs} />
        {/* Upper bound, and the label says "waiting" rather than a count the
            daemon would recognise. See the module header. */}
        <Meter label="waiting on you" value={waiting} ceiling={team.max_open_actions} tone="pending" />
      </div>

      <div className="teams-card-rules">
        <p className="teams-card-label">Per task</p>
        <div className="teams-card-chips">
          <LimitChip name="rounds" ceiling={team.max_rounds} />
          <LimitChip name="parallel" ceiling={team.max_parallel} />
          <LimitChip name="spend" ceiling={team.budget_usd} format={usd} />
        </div>
      </div>

      <Sparkline values={pulseOf(runs)} label={pulseLabel(runs.length)} />

      <footer className="teams-card-foot">
        <RoutineLine armed={armed} total={triggers.length} />
        <Link className="teams-card-open" to={`/teams/${team.id}`}>
          open →
        </Link>
      </footer>
    </article>
  );
}

/**
 * What this department is doing, in one word.
 *
 * Derived, because a department has no state column of its own — it is a
 * standing unit, not a state machine. Working beats waiting: a department can
 * be both, and the one that is spending money is the one worth the badge.
 */
function CardState({ live, waiting }: { live: number; waiting: number }) {
  if (live > 0) return <Badge tone="active">at work</Badge>;
  if (waiting > 0) return <Badge tone="pending">waiting on you</Badge>;
  return <Badge tone="off">idle</Badge>;
}

/**
 * The three grantable actions, each in one of three states.
 *
 * The absence of a grant row IS the denial — there is no `deny` mode
 * (`core/src/team.rs:375`) — so a kind with nothing is drawn as "asks you", not
 * as a fourth state and not as an error.
 */
function Powers({ grants }: { grants: TeamGrant[] }) {
  return (
    <ul className="teams-powers" aria-label="Powers">
      {GRANTABLE_ACTIONS.map((kind) => {
        const mode = grants.find((grant) => grant.kind === kind)?.mode ?? null;
        const said = mode === "allow" ? "does it" : mode === "propose" ? "asks first" : "asks you";
        return (
          <li className={`teams-power teams-power-${mode ?? "none"}`} key={kind} title={`${kind}: ${said}`}>
            <span aria-hidden="true">{POWER_MARK[kind]}</span>
            <span className="teams-power-said">
              {kind}: {said}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

/** One glyph per grantable kind. The word is in the title and in the sr-only span. */
const POWER_MARK: Record<(typeof GRANTABLE_ACTIONS)[number], string> = {
  calendar_event: "cal",
  file_document: "doc",
  send_email: "mail",
};

/**
 * A task that is running, and what it has cost so far.
 *
 * Its own component so it owns its own `useTeamRun(id)` — the row-scoped hook
 * pattern. This is the one N+1 on the page and it is bounded by
 * `max_live_runs`, which the daemon caps at 4, and only for tasks that are
 * still alive. `cost_usd` exists nowhere else: the run LIST does not carry it,
 * which is why the old page had no cost column and was right not to invent one.
 */
function LiveTask({ run, ceiling }: { run: TeamRun; ceiling: number | null }) {
  const detail = useTeamRun(run.id);

  return (
    <div className="teams-task">
      <div className="teams-task-head">
        <Link to={`/team-runs/${run.id}`}>{run.request}</Link>
        <StateBadge domain="team_run" state={run.state} />
      </div>
      <p className="teams-task-when">
        round {run.round} · started <RelativeTime at={run.created_at} />
      </p>
      {detail.data === undefined ? (
        <p className="teams-loading">reading what it has spent…</p>
      ) : (
        // The money meter lives here and only here: this is the one place in
        // the pillar where a spend and the ceiling it runs against both exist.
        <Meter
          label="spent on this task"
          value={detail.data.cost_usd}
          ceiling={ceiling}
          format={usd}
          tone="pending"
        />
      )}
    </div>
  );
}

/**
 * The routines, and when the first armed one fires.
 *
 * It names the rule rather than claiming to be the department's soonest: the
 * daemon answers "when does THIS rule fire next" one rule at a time
 * (`GET /team-triggers/{id}/next`), and finding the earliest would mean a query
 * per rule per department on a page that already has one N+1.
 */
function RoutineLine({ armed, total }: { armed: TeamTrigger[]; total: number }) {
  if (total === 0) return <span className="teams-card-routine">no routine</span>;
  if (armed.length === 0) {
    return (
      <span className="teams-card-routine">
        {total} {total === 1 ? "routine" : "routines"}, none armed
      </span>
    );
  }
  return (
    <span className="teams-card-routine">
      <Badge tone="active">armed</Badge>
      <span className="teams-card-routine-name">{armed[0].name}</span>
      <NextFiring id={armed[0].id} />
      {armed.length > 1 && <span className="teams-card-count">+{armed.length - 1}</span>}
    </span>
  );
}

/**
 * When one rule fires next.
 *
 * A rule that does not fire on a clock answers 200 with a sentence, and a bad
 * cron answers 200 with the daemon's parse message. Neither is an error state,
 * and neither is rendered as one.
 */
function NextFiring({ id }: { id: number }) {
  const next = useTriggerNext(id);
  if (next.data === undefined) return null;
  if (next.data.error !== null) return <span className="teams-card-next">{next.data.error}</span>;
  if (next.data.next !== null) {
    return (
      <span className="teams-card-next">
        next <RelativeTime at={next.data.next} />
      </span>
    );
  }
  return null;
}

/* --------------------------------------------------------------- pulse -- */

/**
 * This department's activity, one bucket per day it has a run in.
 *
 * Days and not a fixed span, because there is no fixed span to be had: the run
 * list is the newest hundred across EVERY department, so a department that runs
 * often is represented by a few hours and one that runs rarely by months. The
 * buckets are the days the window actually contains for this department, which
 * is a shape that is true whatever the window turns out to be — and the label
 * says how many runs it is drawn from rather than naming a period.
 */
export function pulseOf(runs: TeamRun[]): number[] {
  const perDay = new Map<string, number>();
  for (const run of runs) {
    const day = run.created_at.slice(0, 10);
    perDay.set(day, (perDay.get(day) ?? 0) + 1);
  }
  return [...perDay.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([, count]) => count);
}

export function pulseLabel(count: number): string {
  if (count === 0) return "no run in the window";
  return `the ${count} ${count === 1 ? "run" : "runs"} in the window`;
}
