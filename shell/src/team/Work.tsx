import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  TEAM_RUN_LIST_LIMIT,
  teamRunIsAlive,
  useStartTeamRun,
  useTeamRun,
  type TeamItem,
  type TeamRun,
  type TeamView,
} from "../data/teams";
import { Button, ErrorNote, Meter, Panel, RefusalNote, RelativeTime, Section, StateBadge, usd } from "../ui";
import { daemonProse } from "./prose";

/**
 * `Work` — what this department has been asked to do, and what it is doing now.
 *
 * Reads and starts; it saves nothing. `GET /team-runs` and
 * `POST /teams/{id}/runs` are the whole surface.
 *
 * ## The handoff contract
 *
 * This tab and the task page (`/team-runs/$runId`) divide one subject, and the
 * line between them is drawn here so neither grows into the other:
 *
 * - **The bench** says which round a live task is in, who is working, and what
 *   it has cost. Three facts, at a glance, without leaving the department.
 * - **The task page** owns everything below that: the graph, the files
 *   produced, the actions one by one, and the parent→child chain.
 *
 * The rounds strip is drawn only for a task that is still alive, and it is free:
 * `GET /team-runs/{id}` has to be fetched anyway for `cost_usd`, which the list
 * does not carry. The number of those fetches is bounded by `max_live_runs`,
 * which the daemon caps at 4.
 */

export interface WorkProps {
  team: TeamView;
  /** Already filtered to this department by the bench. */
  runs: TeamRun[];
}

export function Work({ team, runs }: WorkProps) {
  const live = runs.filter((run) => teamRunIsAlive(run.state));
  const done = runs.filter((run) => !teamRunIsAlive(run.state));

  return (
    <div className="teams-work">
      <Composer teamId={team.id} />

      {live.length > 0 && (
        <Section label="In flight">
          <ul className="ui-rows" aria-label="In flight">
            {live.map((run) => (
              <li className="ui-rows-row" key={run.id}>
                <LiveTask run={run} ceiling={team.budget_usd} />
              </li>
            ))}
          </ul>
        </Section>
      )}

      <Panel title="Tasks">
        {runs.length === 0 && <p className="teams-empty">no task yet for this team.</p>}
        {done.length > 0 && (
          <ul className="ui-rows" aria-label="Tasks">
            {done.map((run) => (
              <li className="ui-rows-row" key={run.id}>
                <div className="teams-run-head">
                  <Link to={`/team-runs/${run.id}`}>{run.request}</Link>
                  <StateBadge domain="team_run" state={run.state} />
                  <RelativeTime at={run.created_at} />
                </div>
                {run.why !== null && <p className="teams-run-why">{run.why}</p>}
              </li>
            ))}
          </ul>
        )}
        {/*
          The honest footer. `GET /team-runs` is a hard LIMIT 100 across EVERY
          department with no paging, so this department's list is whatever
          survived that cut — not its history, and not even necessarily its
          hundred.
        */}
        <p className="teams-cap">
          showing this team&apos;s tasks from the newest {TEAM_RUN_LIST_LIMIT} runs across all teams
          — there is no paging past that cap.
        </p>
      </Panel>
    </div>
  );
}

/**
 * One line that grows when you mean it.
 *
 * It replaces a three-row textarea that stood permanently open on a page you
 * had come to read. Asking for work is one sentence most of the time, and the
 * box gets bigger the moment it has focus or anything typed in it — so the
 * common case costs one line of the page and the uncommon one loses nothing.
 */
function Composer({ teamId }: { teamId: string }) {
  const [request, setRequest] = useState("");
  const [open, setOpen] = useState(false);
  const start = useStartTeamRun();
  const grown = open || request !== "";

  return (
    <form
      className="teams-composer"
      onSubmit={(event) => {
        event.preventDefault();
        if (request.trim() === "" || start.isPending) return;
        start.mutate({ id: teamId, request: request.trim() }, { onSuccess: () => setRequest("") });
      }}
    >
      <label className="teams-composer-field">
        <span className="teams-label">Ask this team for something</span>
        <textarea
          className={grown ? "teams-textarea teams-composer-grown" : "teams-textarea"}
          rows={grown ? 3 : 1}
          value={request}
          placeholder="reconcile the October invoices"
          onFocus={() => setOpen(true)}
          onBlur={() => setOpen(false)}
          onChange={(event) => setRequest(event.target.value)}
        />
      </label>
      <div className="teams-actions">
        <Button type="submit" intent="go" disabled={request.trim() === "" || start.isPending}>
          Start
        </Button>
      </div>
      {start.isError && <StartRefusal error={start.error} />}
      {start.data !== undefined && (
        <p className="teams-note">
          Started — <Link to={`/team-runs/${start.data.id}`}>this task</Link> has nothing in it yet;
          the núcleo has not picked it up.
        </p>
      )}
    </form>
  );
}

/**
 * Why a task would not start.
 *
 * The 400s name the missing specialist, the empty roster, the deleted director
 * or the local model this machine has not got — the daemon's own sentence. The
 * 429 is the budget window, and it reads as a ceiling that reopens rather than
 * as a failure.
 */
function StartRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — no task was started</ErrorNote>;
  if (error.status === 429) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          too_many_requests: "the budget window is exhausted for now — it reopens",
          ...daemonProse(error),
        }}
      />
    );
  }
  return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
}

/**
 * A task that is running: which round, who is on it, what it has cost.
 *
 * Its own component so it owns its own `useTeamRun(id)` — the row-scoped hook
 * pattern, polled only while the task is alive.
 */
function LiveTask({ run, ceiling }: { run: TeamRun; ceiling: number | null }) {
  const detail = useTeamRun(run.id);

  return (
    <article className="teams-task" aria-label={run.request}>
      <div className="teams-task-head">
        <Link to={`/team-runs/${run.id}`}>{run.request}</Link>
        <StateBadge domain="team_run" state={run.state} />
      </div>
      <p className="teams-task-when">
        started <RelativeTime at={run.created_at} />
      </p>

      {detail.data === undefined ? (
        <p className="teams-loading">reading the rounds…</p>
      ) : (
        <>
          <Rounds items={detail.data.items} round={detail.data.round} />
          <Meter
            label="spent on this task"
            value={detail.data.cost_usd}
            ceiling={ceiling}
            format={usd}
            tone="pending"
          />
        </>
      )}
    </article>
  );
}

/**
 * The rounds strip — the bench's half of the handoff contract.
 *
 * One line per round, one chip per specialist. The round after the current one
 * says "not planned yet" rather than being left blank: the director plans a
 * round at a time, so an empty next round is a fact about how this works and
 * not a gap in the answer.
 */
function Rounds({ items, round }: { items: TeamItem[]; round: number }) {
  if (items.length === 0) {
    return <p className="teams-empty">nothing planned yet — the núcleo has not picked this up.</p>;
  }

  const rounds = [...new Set(items.map((item) => item.round))].sort((a, b) => a - b);

  return (
    <>
      <p className="teams-rounds-key"><span>✓ done</span><span>⋯ running</span><span>· not started</span><span>✗ failed</span></p>
      <ol className="teams-rounds" aria-label="Rounds">
      {rounds.map((number) => (
        <li className="teams-round-line" key={number}>
          <span className="teams-round-no">round {number}</span>
          <span className="teams-round-who">
            {items
              .filter((item) => item.round === number)
              .map((item) => (
                <span
                  className={`teams-round-item teams-round-${item.state}`}
                  key={item.ordinal}
                  title={item.description}
                >
                  {item.agent_id}
                  <span className="teams-round-mark" aria-hidden="true">
                    {MARK[item.state] ?? "·"}
                  </span>
                  <span className="teams-said">{item.state}</span>
                </span>
              ))}
          </span>
        </li>
      ))}
      <li className="teams-round-line">
        <span className="teams-round-no">round {Math.max(...rounds, round) + 1}</span>
        <span className="teams-round-who">
          <span className="teams-empty">not planned yet</span>
        </span>
      </li>
      </ol>
    </>
  );
}

/** A glyph per item state. The word travels beside it for anything that does not render. */
const MARK: Record<string, string> = {
  done: "✓",
  running: "⋯",
  pending: "·",
  failed: "✗",
};
