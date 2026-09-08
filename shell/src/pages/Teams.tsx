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
  Meter,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  Sparkline,
  StaleNote,
  StateBadge,
  Teach,
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
 * **A strip and a table, and not a gallery of cards.** The first shape this
 * took was one card per department, and it failed for a reason only visible on
 * screen: a card carried a `work in flight` block only when that department had
 * work in flight, so every section below it — occupancy, ceilings, pulse — sat
 * at a different height in every card. Six cards side by side became six
 * documents to read one at a time rather than one thing to scan across, which
 * is the opposite of what a console is for. A table cannot have that defect:
 * the columns line up because they are columns.
 *
 * So the live work, which is what made the heights unequal, is lifted out into
 * a strip above the table — where it also belongs, because "what is running
 * right now" is a question about the whole organisation and never about one
 * department at a time. It appears only when something is running.
 *
 * **One shape per class of thing.** The cards drew a specialist's name, a
 * per-task ceiling, a granted power, a department's state and an armed routine
 * as five near-identical bordered pills, so no pill could be told from another
 * without reading it. Now: a filled `Badge` is a state and nothing else; a pill
 * outline is a person (`ui-who`, on the bench); a ceiling is plain tabular text
 * (`ui-limit`); a power is a mark plus a word; a routine is a diamond.
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
            {creating ? "Close" : "New team"}
          </Button>
        }
      />

      {stale && <StaleNote dataUpdatedAt={teams.dataUpdatedAt} />}
      {teams.isError && teams.data === undefined && <ListError error={teams.error} />}

      {/* Closed by default, and that is the point: the old page opened an
          eleven-field form above a list you had not read yet. */}
      {creating && (
        <Panel title="New team">
          {/* The very form the Charter tab is, minus the drift guard — there is
              nothing to have drifted from yet. One editor, one place. */}
          <NewDepartment />
        </Panel>
      )}

      {rows.length === 0 ? (
        <Teach title="No team yet">
          <p>
            A team is a permanent unit — Finance, Marketing, Security — not a task. It has a
            director, a roster of specialists, what it may do on its own, and the ceilings every
            task of its runs under. Tasks and routines belong to it and are set up inside it.
          </p>
        </Teach>
      ) : (
        <>
          <InFlight teams={rows} runs={allRuns} />
          <DepartmentTable
            teams={rows}
            runs={allRuns}
            triggers={allTriggers}
            actions={openActions}
          />
          <Panel title="Who works where">
            <RosterMatrix teams={rows} />
          </Panel>
        </>
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
  if (rows.length === 0) return "no team yet";

  const parts = [`${rows.length} ${rows.length === 1 ? "team" : "teams"}`];
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
  return <ErrorNote>the núcleo did not answer — nothing is known about the teams</ErrorNote>;
}

/* ------------------------------------------------------------ in flight -- */

/**
 * Everything running right now, across every department.
 *
 * Above the table and not inside it, for two reasons. The shallow one is
 * alignment: a live task is the one block a department either has or has not,
 * and while it lived in a card it made every card a different height. The real
 * one is that this is a different question — "what is my organisation doing"
 * does not decompose per department, and you want the running tasks together
 * rather than found by reading six rows.
 *
 * Nothing renders when nothing runs. The headline already says "none at work",
 * so an empty strip would be a second way of saying it and a permanent hole in
 * the page.
 */
function InFlight({ teams, runs }: { teams: TeamView[]; runs: TeamRun[] }) {
  const live = runs.filter((run) => teamRunIsAlive(run.state));
  if (live.length === 0) return null;

  return (
    <section className="teams-flight" aria-label="In flight">
      <h2 className="teams-flight-title">
        In flight <span className="teams-flight-count">{live.length}</span>
      </h2>
      <ul className="ui-rows teams-flight-list">
        {live.map((run) => (
          <li key={run.id} className="ui-rows-row">
            <LiveTask run={run} team={teams.find((row) => row.id === run.team_id) ?? null} />
          </li>
        ))}
      </ul>
    </section>
  );
}

/**
 * A task that is running, and what it has cost so far.
 *
 * Its own component so it owns its own `useTeamRun(id)` — the row-scoped hook
 * pattern. This is the one N+1 on the page and it is bounded by
 * `max_live_runs`, which the daemon caps at 4, and only for tasks that are
 * still alive. `cost_usd` exists nowhere else: the run LIST does not carry it,
 * which is why the old page had no cost column and was right not to invent one.
 *
 * `team` can be null. The run list is the newest hundred across every
 * department, so it can carry a run whose department is not in `GET /teams` —
 * deleted while it ran, most plainly. Drawn as a task with no department rather
 * than dropped: a running task nobody can attribute is worth seeing more, not
 * less.
 */
function LiveTask({ run, team }: { run: TeamRun; team: TeamView | null }) {
  const detail = useTeamRun(run.id);

  return (
    <article className="teams-task">
      <div className="teams-task-head">
        {team === null ? (
          <span className="teams-task-dept teams-task-orphan">no team</span>
        ) : (
          <Link className="teams-task-dept" to={`/teams/${team.id}`}>
            {team.name}
          </Link>
        )}
        <Link className="teams-task-what" to={`/team-runs/${run.id}`}>
          {run.request}
        </Link>
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
          ceiling={team?.budget_usd ?? null}
          format={usd}
          tone="pending"
        />
      )}
    </article>
  );
}

/* ---------------------------------------------------------------- table -- */

/**
 * Every department, one per row.
 *
 * A real `<table>` for the reason `RosterMatrix` is one: a department is a row,
 * the readings are columns, and a screen reader that lands in a cell is told
 * both. Built from `div`s it would be a picture of a table.
 *
 * Its own horizontal scroller, so a narrow window slides the table and never
 * the whole document.
 */
function DepartmentTable({
  teams,
  runs,
  triggers,
  actions,
}: {
  teams: TeamView[];
  runs: TeamRun[];
  triggers: TeamTrigger[];
  actions: TeamAction[];
}) {
  return (
    <div className="teams-table-scroller">
      <table className="teams-table">
        <caption className="teams-said">Every team, with what it is doing now</caption>
        <thead>
          <tr>
            <th scope="col">Team</th>
            <th scope="col">State</th>
            <th scope="col" className="teams-col-num">
              Staff
            </th>
            <th scope="col" className="teams-col-num">
              At work
            </th>
            <th scope="col" className="teams-col-num">
              Waiting
            </th>
            <th scope="col">On its own</th>
            {/* What the marks are, said once in the header rather than nowhere. A pulse
                with no key is a shape a reader has to guess the unit of; the guess is
                free to be wrong and nothing on the page corrects it. */}
            <th scope="col">
              Pulse<span className="teams-col-key">per day</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {teams.map((team) => (
            <DepartmentRow
              key={team.id}
              team={team}
              runs={runs.filter((run) => run.team_id === team.id)}
              triggers={triggers.filter((rule) => rule.team_id === team.id)}
              waiting={waitingFor(team.id, actions, runs)}
            />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function DepartmentRow({
  team,
  runs,
  triggers,
  waiting,
}: {
  team: TeamView;
  runs: TeamRun[];
  triggers: TeamTrigger[];
  waiting: number;
}) {
  const live = runs.filter((run) => teamRunIsAlive(run.state));

  return (
    <tr>
      <th scope="row" className="teams-row-name">
        <Link to={`/teams/${team.id}`}>{team.name}</Link>
        {/* Drawn whether or not there is one: a remit that appeared on some rows
            and not others would start the next line at two different heights,
            which is the defect the cards had. */}
        <span
          className={team.mission === "" ? "teams-row-remit teams-row-unwritten" : "teams-row-remit"}
        >
          {team.mission === "" ? "no remit written" : team.mission}
        </span>
      </th>
      <td>
        <RowState live={live.length} waiting={waiting} />
      </td>
      <td className="teams-col-num">
        <Headcount team={team} />
      </td>
      <td className="teams-col-num">
        <Ratio value={live.length} ceiling={team.max_live_runs} />
      </td>
      {/* Upper bound — see the module header. The column says "Waiting", never a
          count the daemon would recognise. */}
      <td className="teams-col-num">
        <Ratio value={waiting} ceiling={team.max_open_actions} />
      </td>
      <td>
        <OnItsOwn grants={team.grants} triggers={triggers} />
      </td>
      <td className="teams-row-pulse">
        <Sparkline
          values={pulseOf(runs)}
          label={`${team.name}: ${pulseLabel(runs.length)}`}
          labelHidden
          titles={pulseTitles(runs)}
          width={96}
          height={20}
        />
      </td>
    </tr>
  );
}

/**
 * What this department is doing, in one word.
 *
 * Derived, because a department has no state column of its own — it is a
 * standing unit, not a state machine. Working beats waiting: a department can
 * be both, and the one that is spending money is the one worth the badge.
 */
function RowState({ live, waiting }: { live: number; waiting: number }) {
  if (live > 0) return <Badge tone="active">at work</Badge>;
  if (waiting > 0) return <Badge tone="pending">waiting on you</Badge>;
  return <Badge tone="off">idle</Badge>;
}

/**
 * How many people, and a mark when that is nobody.
 *
 * Zero is not a small number here, it is a department that cannot work: the
 * daemon will not start a task without a roster. It is marked rather than left
 * to look like any other figure in the column.
 */
function Headcount({ team }: { team: TeamView }) {
  const count = headcountOf(team);
  if (count > 0) return <span className="teams-figure">{count}</span>;
  return (
    <span
      className="teams-figure teams-figure-none"
      title="nobody yet — a task cannot start without a roster"
    >
      0<span className="teams-said"> — nobody yet, so no task can start</span>
    </span>
  );
}

/**
 * A reading against its ceiling, as a figure and not a bar.
 *
 * The cards drew these as meters, two per card, twelve on a screen. In a column
 * the comparison the bar was making is already made — the figures sit under each
 * other, tabular, and the eye does it. What a bar adds at that point is ink.
 *
 * At the ceiling it is marked, because `1 / 1` and `0 / 1` are one glyph apart
 * and mean entirely different things.
 */
function Ratio({ value, ceiling }: { value: number; ceiling: number }) {
  const full = ceiling > 0 && value >= ceiling;
  return (
    <span className={full ? "teams-figure teams-figure-full" : "teams-figure"}>
      {value}
      <span className="teams-figure-of"> / {ceiling}</span>
      {full && <span className="teams-said"> — at the ceiling</span>}
    </span>
  );
}

/**
 * What happens without you: the three grantable actions, and whether a routine
 * is armed.
 *
 * One column, because it is one question. The absence of a grant row IS the
 * denial — there is no `deny` mode (`core/src/team.rs:375`) — so a kind with
 * nothing is drawn as "asks you", not as a fourth state and not as an error.
 *
 * Marks and not boxes. Filled acts, half drafts and waits, hollow cannot; the
 * shape carries it, so none of this is colour-only, and every mark has its
 * sentence beside it for anything that does not render.
 */
function OnItsOwn({ grants, triggers }: { grants: TeamGrant[]; triggers: TeamTrigger[] }) {
  const armed = triggers.filter((rule) => rule.enabled !== 0);

  return (
    <ul className="teams-alone">
      {GRANTABLE_ACTIONS.map((kind) => {
        const mode = modeOf(grants, kind);
        const said = MODE_SAID[mode];
        return (
          <li
            className={`teams-alone-item teams-alone-${mode}`}
            key={kind}
            title={`${kind}: ${said}`}
          >
            <span aria-hidden="true">{MODE_MARK[mode]}</span>
            <span aria-hidden="true">{POWER_WORD[kind]}</span>
            <span className="teams-said">
              {kind}: {said}
            </span>
          </li>
        );
      })}
      <Routines armed={armed.length} total={triggers.length} />
    </ul>
  );
}

/**
 * What this department may do with one kind of action.
 *
 * `TeamGrant.mode` is a bare `string` on the wire, so a mode this shell has
 * never heard of is possible and is its own answer — the same treatment
 * `StateBadge` gives an unmapped state. It used to fall through to "asks you",
 * which reads as a decision the daemon made rather than as a shell that is
 * behind its núcleo.
 */
type Mode = "allow" | "propose" | "none" | "unmapped";

function modeOf(grants: TeamGrant[], kind: string): Mode {
  const raw = grants.find((grant) => grant.kind === kind)?.mode;
  if (raw === undefined) return "none";
  if (raw === "allow" || raw === "propose") return raw;
  return "unmapped";
}

/** Filled acts, half drafts, hollow cannot, and a question mark is this shell's own gap. */
const MODE_MARK: Record<Mode, string> = {
  allow: "●",
  propose: "◐",
  none: "○",
  unmapped: "?",
};

const MODE_SAID: Record<Mode, string> = {
  allow: "does it",
  propose: "asks first",
  none: "asks you",
  unmapped: "this shell has no reading for that mode",
};

/** The short word for each grantable kind. The full name is in the title and beside it. */
const POWER_WORD: Record<(typeof GRANTABLE_ACTIONS)[number], string> = {
  calendar_event: "cal",
  file_document: "doc",
  send_email: "mail",
};

/**
 * Whether a clock can start work here without you.
 *
 * A diamond, so it is not read as a fourth power. It says how many are armed
 * and never when the next one fires: the daemon answers that one rule at a time
 * (`GET /team-triggers/{id}/next`), so the soonest across a department would be
 * a query per rule per department, on a page that already carries one N+1. The
 * bench answers it, per rule, where there is room.
 */
function Routines({ armed, total }: { armed: number; total: number }) {
  if (total === 0) return null;
  if (armed === 0) {
    return (
      <li
        className="teams-alone-item teams-alone-rule"
        title={`${total} routine${total === 1 ? "" : "s"}, none armed`}
      >
        <span aria-hidden="true">◇</span>
        <span className="teams-said">
          {total} {total === 1 ? "routine" : "routines"}, none armed
        </span>
      </li>
    );
  }
  return (
    <li
      className="teams-alone-item teams-alone-rule teams-alone-armed"
      title={`${armed} armed routine${armed === 1 ? "" : "s"}`}
    >
      <span aria-hidden="true">◆</span>
      <span aria-hidden="true">{armed}</span>
      <span className="teams-said">
        {armed} armed {armed === 1 ? "routine" : "routines"}
      </span>
    </li>
  );
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
  return pulseDays(runs).map(([, count]) => count);
}

/**
 * What each bar of the pulse is, one string per bar.
 *
 * The same buckets, said in words: the chart is drawn without an axis on purpose —
 * there is no fixed span to label — so the only honest way to answer "which day is
 * that mark" is per mark, where the pointer already is.
 *
 * Off the same `pulseDays` the heights come from, and not a second copy of the
 * bucketing. Two loops that must agree on an ORDER are two loops free to stop
 * agreeing, and nothing on the screen would say which bar had been mislabelled.
 */
export function pulseTitles(runs: TeamRun[]): string[] {
  return pulseDays(runs).map(([day, count]) => `${count} run${count === 1 ? "" : "s"} on ${day}`);
}

/** The days this department has a run in, oldest first, with how many. */
function pulseDays(runs: TeamRun[]): [string, number][] {
  const perDay = new Map<string, number>();
  for (const run of runs) {
    const day = run.created_at.slice(0, 10);
    perDay.set(day, (perDay.get(day) ?? 0) + 1);
  }
  return [...perDay.entries()].sort(([a], [b]) => a.localeCompare(b));
}

export function pulseLabel(count: number): string {
  if (count === 0) return "no run in the window";
  return `the ${count} ${count === 1 ? "run" : "runs"} in the window`;
}
