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
import {
  Button,
  ConfirmButton,
  Crumb,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  Section,
  StatCard,
  StateBadge,
  money,
} from "../ui";
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
        <RunCrumb />
        <PageHeader title="Team run" />
        {run.isError ? <DetailError error={run.error} /> : <p className="teams-loading">reading the run…</p>}
      </>
    );
  }

  return (
    <>
      <RunCrumb />
      <PageHeader
        /*
          The request, because the request is what this page is about. "Team run" was the
          title on every one of these and told a reader nothing they had not already
          decided by clicking; the sentence somebody actually typed is the only thing on
          the screen that tells this run from the last one. The words that WERE the title
          are the crumb above, which is where a page's kind belongs.
        */
        title={detail.request}
        headline={
          <>
            {detail.team_id} · started <RelativeTime at={detail.created_at} />
          </>
        }
        /*
          Cancel, alone. The state badge is NOT here, and that is a decision rather than
          an omission: it is the first instrument in the strip below, and a reading drawn
          twice on one screen is two things that can disagree.

          Deleting used to stand beside it, both `intent="stop"` and both a click away —
          two red buttons in one corner, one of which ends the work and one of which ends
          the record of it. They are now different distances away: this is the live
          control and belongs where a live control belongs, and the other is at the foot
          of the page, past everything a reader would want before destroying it.

          Only while live: a run that has already ended has nothing left to stop.
        */
        actions={
          alive ? (
            <Button intent="stop" disabled={cancel.isPending} onClick={() => cancel.mutate(id)}>
              Cancel
            </Button>
          ) : undefined
        }
      />

      {cancel.isError && <CancelNote error={cancel.error} />}

      <Instruments run={detail} alive={alive} />

      <div className="teams-summary">
        {/* A stopped or expired run must not carry any word or tone of failure anywhere
            on this page — the badge already refuses to, and this note must not put it
            back. `why` renders exactly as the daemon wrote it. */}
        {detail.why !== null && <p className="teams-note">{detail.why}</p>}
        <OriginLine run={detail} />
      </div>

      <RoundsPanel items={detail.items} />

      <ActionsPanel
        actions={actions.data ?? []}
        isError={actions.isError}
        error={actions.isError ? actions.error : undefined}
      />

      {/* Last on the page, under a heading that says what it is for. A destructive
          write is not a page action here — it is the end of the thing the page is
          about, and it is reached by scrolling past the run rather than by aiming at
          the same corner as Cancel. `DeleteNote` stays with the control that produced
          it. */}
      <Section label="Ending this run">
        <ConfirmButton
          label="Delete run"
          confirmLabel="Delete it, and its folder"
          intent="stop"
          disabled={del.isPending}
          onConfirm={() => del.mutate(id, { onSuccess: () => void navigate({ to: "/teams" }) })}
        />
        {del.isError && <DeleteNote error={del.error} />}
      </Section>
    </>
  );
}

/**
 * Where this page sits, and the way back out of it.
 *
 * Above the title rather than under the last panel, which is where "Back to the teams"
 * used to be: a way out reached only by reading the whole run is a way out for whoever
 * no longer needs one. It carries the page's KIND — "Team run", the words this page used
 * to spend its heading on — so that giving the heading to the request costs nothing a
 * reader had.
 */
function RunCrumb() {
  return (
    <Crumb to="/teams" here="Team run">
      Teams
    </Crumb>
  );
}

/**
 * The four readings of a run, on one line under the header.
 *
 * They were three stacked panels — "This run", the origin line and "Cost" — each with a
 * heading, a border and one fact in it, so the first screen of a run was mostly frames.
 * A strip says the same four things in the space one of those panels took, and puts them
 * where an instrument belongs: beside each other, in a row that is read in one movement.
 */
function Instruments({ run, alive }: { run: TeamRunView; alive: boolean }) {
  return (
    <div className="teams-strip">
      <StatCard label="State" value={<StateBadge domain="team_run" state={run.state} />} />
      <StatCard label="Round" value={run.round} detail={`${run.items.length} items so far`} />
      {/* `money` and never a raw `toFixed`: every figure in this app that is a spend is
          written by the one formatter, so `$1.25` and `$0.004` are the same reading at
          two magnitudes rather than two conventions. */}
      <StatCard
        label="Cost"
        value={money(run.cost_usd)}
        detail={run.root_id === run.id ? "this run's own spend" : "this run's own spend — it is part of a chain"}
      />
      {/* A word and not a figure, so it is set at the rank a word can be read at: the
          display face at `--text-3xl` turned "between rounds" into two lines of headline
          that outshouted the three readings beside it. The card's job is the same; only
          the type is honest about what is in it. */}
      <StatCard
        label="Director"
        value={<span className="text-lg leading-snug">{directorNodeText(run.director_node)}</span>}
        detail={alive ? "what it is doing now" : "where it stopped"}
      />
    </div>
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
          <ul className="ui-rows" aria-label={`Round ${round}`}>
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
    <li className="ui-rows-row teams-item-row">
      {item.run_id !== null ? (
        <Link className="teams-item-agent" to={`/runs/${item.run_id}`}>
          {item.agent_id}
        </Link>
      ) : (
        <span className="teams-item-agent">{item.agent_id}</span>
      )}
      <StateBadge domain="team_item" state={item.state} />
      <p className="teams-item-what">{item.description}</p>
      {/* The delivery is the folder and it shows up in Files — nothing here
          fetches it, so the path is shown as text. */}
      {item.output_path !== null && <p className="teams-item-output">{item.output_path}</p>}
      {item.run_id !== null && <span className="teams-item-output">run {item.run_id}</span>}
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
  if (!isError && actions.length === 0) return null;
  return (
    <Panel title="Actions">
      {isError && actions.length === 0 && <ActionsError error={error} />}
      {!isError && actions.length === 0 && <p className="teams-empty">nothing asked for yet.</p>}
      {actions.length > 0 && (
        <ul className="ui-rows" aria-label="Actions">
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
    <li className="ui-rows-row teams-action-row">
      <div className="teams-action-head">
        <span className="teams-action-kind">{action.kind}</span>
        <StateBadge domain="team_action" state={state} />
      </div>
      <p className="teams-action-why">{action.why}</p>
      {payload !== null ? (
        <ul className="teams-action-payload" aria-label="Payload">
          {Object.entries(payload).map(([key, value]) => (
            <li className="teams-action-field" key={key}>
              <span>{key}</span>
              <span>{formatPayloadValue(value)}</span>
            </li>
          ))}
        </ul>
      ) : (
        // A payload this second parse cannot read still shows the raw text —
        // a worse view, not a broken one.
        <pre className="teams-action-raw">{action.payload}</pre>
      )}
      <p className="teams-note">
        {action.ordinal === null ? "the director asked" : "a specialist asked"}
        {" — "}
        {action.proposal_id === null ? "this team may do that without asking" : "waiting on a decision"}
      </p>
    </li>
  );
}

function formatPayloadValue(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  return JSON.stringify(value);
}

/* The Cost panel is now the third instrument in the strip above. The sentence it carried
   — that this is the run's OWN spend and the chain's total is not something the núcleo
   answers — is the card's detail line, so the one thing that panel said that a figure
   cannot say is still said. */
