import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  parseActionPayload,
  teamActionState,
  teamRunIsAlive,
  useCancelTeamRun,
  useDeleteTeamRun,
  useTeamRun,
  useTeamRunActions,
  type TeamAction,
  type TeamItem,
  type TeamRunView,
} from "../data/teams";
import { Button, ConfirmButton, ErrorNote, PageHeader, Panel, RefusalNote, StateBadge } from "../ui";
import "./teams.css";

/**
 * One team run, in full.
 *
 * The run advances by the núcleo reading its own database on a ten-second
 * tick, so this page polls at `POLL.queue` while the run is live and stops on
 * the tick that lands a terminal state (`useTeamRun`, `data/teams.ts`).
 *
 * The chain's spend is not a number any route answers, so this page shows
 * this run's own cost and links to the root rather than inventing a total.
 */
export function TeamRunDetail() {
  const params = useParams({ strict: false }) as { runId?: string };
  const id = params.runId ?? "";
  const navigate = useNavigate();

  const run = useTeamRun(id);
  const detail = run.data;
  const alive = detail !== undefined && teamRunIsAlive(detail.state);
  const actions = useTeamRunActions(id, alive);
  const cancel = useCancelTeamRun();
  const del = useDeleteTeamRun();

  if (detail === undefined) {
    return (
      <>
        <PageHeader title="Team run" />
        {run.isError ? <DetailError error={run.error} /> : <p className="teams-loading">reading the run…</p>}
        <Link to="/teams">Back to the departments</Link>
      </>
    );
  }

  return (
    <>
      <PageHeader
        title="Team run"
        headline={detail.request}
        actions={
          <div className="teams-actions">
            {/* Only while live: a run that has already ended has nothing left to stop. */}
            {alive && (
              <Button intent="stop" disabled={cancel.isPending} onClick={() => cancel.mutate(id)}>
                Cancel
              </Button>
            )}
            <ConfirmButton
              label="Delete run"
              confirmLabel="Delete it, and its folder"
              intent="stop"
              disabled={del.isPending}
              onConfirm={() => del.mutate(id, { onSuccess: () => void navigate({ to: "/teams" }) })}
            />
          </div>
        }
      />

      {cancel.isError && <CancelNote error={cancel.error} />}
      {del.isError && <DeleteNote error={del.error} />}

      <Panel title="This run">
        <StateBadge domain="team_run" state={detail.state} />
        {/* A stopped or expired run must not carry any word or tone of
            failure anywhere on this page — the badge already refuses to, and
            this note must not put it back. `why` renders exactly as the
            daemon wrote it. */}
        {alive && <p className="teams-note">the director is {directorNodeText(detail.director_node)}.</p>}
        {detail.why !== null && <p className="teams-note">{detail.why}</p>}
      </Panel>

      <OriginLine run={detail} />

      <RoundsPanel items={detail.items} />

      <ActionsPanel
        actions={actions.data ?? []}
        isError={actions.isError}
        error={actions.isError ? actions.error : undefined}
      />

      <CostPanel run={detail} />

      <Link to="/teams">Back to the departments</Link>
    </>
  );
}

function directorNodeText(node: string): string {
  return node === "none" ? "between rounds" : node;
}

function DetailError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return <RefusalNote refusal={error} sentences={{ not_found: "there is no run with that id", ...daemonProse(error) }} />;
  }
  return <ErrorNote>the núcleo did not answer — nothing is known about this run</ErrorNote>;
}

/**
 * A cancel that answers 404 means the run had already ended — the daemon's
 * body says "team not found" about a run, and that wrong noun must never
 * reach the screen.
 */
function CancelNote({ error }: { error: unknown }) {
  if (isApiRefusal(error) && error.status === 404) {
    return (
      <p className="teams-note" role="status">
        this run had already ended.
      </p>
    );
  }
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — this run was not cancelled</ErrorNote>;
}

function DeleteNote({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — this run was not deleted</ErrorNote>;
}

function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

/* ------------------------------------------------------------------ origin -- */

function OriginLine({ run }: { run: TeamRunView }) {
  return (
    <p className="teams-origin">
      <OriginText run={run} />
      {run.root_id !== run.id && (
        <>
          {" "}
          This run is part of a chain — <Link to={`/team-runs/${run.root_id}`}>see the root</Link>.
        </>
      )}
    </p>
  );
}

function OriginText({ run }: { run: TeamRunView }) {
  if (run.parent_id === null && run.trigger_id === null) return <>somebody asked for this.</>;
  if (run.trigger_id !== null) return <>a rule started this.</>;
  return (
    <>
      another run started this — <Link to={`/team-runs/${run.parent_id}`}>the run it came from</Link>.
    </>
  );
}

/* ------------------------------------------------------------------ rounds -- */

function RoundsPanel({ items }: { items: TeamItem[] }) {
  const rounds = groupByRound(items);
  return (
    <Panel title="Rounds">
      {rounds.length === 0 && <p className="teams-empty">no item yet.</p>}
      {rounds.map(([round, rows]) => (
        <div className="teams-round" key={round}>
          <p className="teams-round-head">round {round}</p>
          <ul className="teams-items" aria-label={`Round ${round}`}>
            {rows.map((item) => (
              <ItemRow key={item.ordinal} item={item} />
            ))}
          </ul>
        </div>
      ))}
    </Panel>
  );
}

function groupByRound(items: TeamItem[]): [number, TeamItem[]][] {
  const byRound = new Map<number, TeamItem[]>();
  for (const item of items) {
    const bucket = byRound.get(item.round) ?? [];
    bucket.push(item);
    byRound.set(item.round, bucket);
  }
  for (const bucket of byRound.values()) bucket.sort((a, b) => a.ordinal - b.ordinal);
  return Array.from(byRound.entries()).sort((a, b) => a[0] - b[0]);
}

function ItemRow({ item }: { item: TeamItem }) {
  return (
    <li className="teams-item">
      <div className="teams-item-head">
        <span className="teams-item-agent">{item.agent_id}</span>
        <StateBadge domain="team_item" state={item.state} />
      </div>
      <p className="teams-item-what">{item.description}</p>
      {/* The delivery is the folder and it shows up in Files — nothing here
          fetches it, so the path is shown as text. */}
      {item.output_path !== null && <p className="teams-item-output">{item.output_path}</p>}
      {/* `run_id` is a daemon run id — an integer, a different id space from
          `id`, which is a UUID string. */}
      {item.run_id !== null && <Link to={`/runs/${item.run_id}`}>see the run</Link>}
    </li>
  );
}

/* ----------------------------------------------------------------- actions -- */

function ActionsPanel({
  actions,
  isError,
  error,
}: {
  actions: TeamAction[];
  isError: boolean;
  error: unknown;
}) {
  return (
    <Panel title="Actions">
      {isError && actions.length === 0 && <ActionsError error={error} />}
      {!isError && actions.length === 0 && <p className="teams-empty">nothing asked for yet.</p>}
      {actions.length > 0 && (
        <ul className="teams-acts" aria-label="Actions">
          {actions.map((action) => (
            <ActionCard key={action.id} action={action} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function ActionsError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about what this run asked for</ErrorNote>;
}

/**
 * One thing a department asked the core to do.
 *
 * A refused action is stored `state = 'failed', error = 'rejected'` —
 * `teamActionState` turns that pair into the `rejected` reading so the owner's
 * own "no" never renders as if something broke.
 */
function ActionCard({ action }: { action: TeamAction }) {
  const state = teamActionState(action);
  const payload = parseActionPayload(action.payload);
  return (
    <li className="teams-act">
      <div className="teams-act-head">
        <span className="teams-act-kind">{action.kind}</span>
        <StateBadge domain="team_action" state={state} />
      </div>
      <p className="teams-act-why">{action.why}</p>
      {payload !== null ? (
        <ul className="teams-act-payload" aria-label="Payload">
          {Object.entries(payload).map(([key, value]) => (
            <li className="teams-act-field" key={key}>
              <span>{key}</span>
              <span>{formatPayloadValue(value)}</span>
            </li>
          ))}
        </ul>
      ) : (
        // A payload this second parse cannot read still shows the raw text —
        // a worse view, not a broken one.
        <pre className="teams-act-raw">{action.payload}</pre>
      )}
      <p className="teams-note">
        {action.ordinal === null ? "the director asked" : "a specialist asked"}
        {" — "}
        {action.proposal_id === null ? "this department may do that without asking" : "waiting on a decision"}
      </p>
    </li>
  );
}

function formatPayloadValue(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  return JSON.stringify(value);
}

/* -------------------------------------------------------------------- cost -- */

function CostPanel({ run }: { run: TeamRunView }) {
  return (
    <Panel title="Cost">
      <p className="teams-cost">$ {run.cost_usd.toFixed(4)}</p>
      <p className="teams-cost-note">
        This run&apos;s own spend only — the chain&apos;s total is not something the núcleo answers.
        {run.root_id !== run.id && (
          <>
            {" "}
            See <Link to={`/team-runs/${run.root_id}`}>the root</Link>.
          </>
        )}
      </p>
    </Panel>
  );
}
