import { useCallback, useEffect, useRef, useState } from "react";
import {
  approveProposal, dismissSkippedItem, getAwaitingApproval, getSkippedItems,
  getTeamActionProposals, getVcsRequest, listVcsRequests, rejectProposal,
  type AwaitingRun, type ConnectionState, type Proposal, type VcsRequestSummary, type VcsTicket,
} from "./api";
import {
  relativeTime, vcsIsSettled, vcsOriginLabel, vcsPending, vcsStatusLabel, vcsStatusTone,
} from "./derive";
import { Badge, Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";

/**
 * What has stopped and is waiting on a person.
 *
 * Three lists that were reachable only over HTTP until now, gathered because they answer one
 * question — "what is not moving, and why" — and were previously answerable only by curling the
 * daemon. Deliberately NOT the `action-approval` queue: that one already has a home on the Autopilot
 * tab, next to the kill switch and the budget it is governed by, and moving it here would put the
 * decision a run is blocked on one tab further from the switch that stops the run.
 *
 * The three differ in what a reader can do about them, and the page does not pretend otherwise:
 *
 * - The **git queue** is read-only. `/vcs/requests` has no approve or reject route, because a merge
 *   that needs a person is approved through the proposal it raised, not through the queue row. What
 *   this list is for is the thing nothing else shows: which git operations are stacked up behind one
 *   repository, and which one is holding the line.
 * - **Runs waiting** is a pointer, not a decision. Answering them means answering their proposal on
 *   the Autopilot tab; what this list adds is that the run is parked at all, which the proposal
 *   queue does not say.
 * - **Skipped items** are the only ones with a button here, and it puts the item away rather than
 *   refusing it — nothing is being denied, because the job let go hours ago.
 */

/** How often the queue re-reads itself while something in it is still moving. */
const POLL_MS = 5_000;

function GitQueuePanel({
  requests, loading, token, refresh,
}: {
  requests: VcsRequestSummary[] | null;
  loading: boolean;
  token: string;
  refresh: () => Promise<void>;
}) {
  const [ticket, setTicket] = useState<VcsTicket | null>(null);
  const [ticketFailed, setTicketFailed] = useState(false);

  const inspect = useCallback(
    async (id: number) => {
      setTicketFailed(false);
      const found = await getVcsRequest(token, id);
      setTicketFailed(found === null);
      setTicket(found);
    },
    [token],
  );

  const pending = requests === null ? 0 : vcsPending(requests);

  return (
    <Panel
      title="Git queue"
      aside={
        requests === null
          ? undefined
          : pending === 0
            ? "nothing in flight"
            : `${pending} in flight`
      }
    >
      {requests === null ? (
        !loading && <ErrorNote>Could not read the git queue from the daemon.</ErrorNote>
      ) : requests.length === 0 ? (
        <Teach title="No git operation has been queued.">
          Every merge an agent asks for is queued here first, one at a time per repository, so two
          runs can never rewrite the same refs at once. Nothing has asked yet.
        </Teach>
      ) : (
        <>
          <p className="faint">
            Newest first. This is the whole history of what the daemon has queued, not a list of what
            is pending — nothing prunes it, and it stops at 200 rows.
          </p>
          <ul className="vcs-list">
            {requests.map((request) => (
              <li key={request.id} className="vcs-row">
                <div className="vcs-row__head">
                  <b>
                    #{request.id} · {request.op}
                  </b>
                  <Badge tone={vcsStatusTone(request.status)}>
                    {vcsStatusLabel(request.status)}
                  </Badge>
                </div>
                <div className="vcs-row__meta">
                  <span className="proj">{request.project_id}</span>
                  {" · asked by "}
                  {vcsOriginLabel(request.origin)}
                  {" · "}
                  <time dateTime={request.created_at} title={request.created_at}>
                    {relativeTime(request.created_at)}
                  </time>
                </div>
                {/* The key, not the label. Two rows with different project names and the same key
                    were queued behind each other; without it that reads as a bug in the listing. */}
                <div className="vcs-row__repo faint">
                  <code>{request.repo_key}</code>
                </div>
                {vcsIsSettled(request.status) && (
                  <Button size="sm" variant="link" onClick={() => void inspect(request.id)}>
                    How it ended
                  </Button>
                )}
              </li>
            ))}
          </ul>
        </>
      )}

      {ticketFailed && <ErrorNote>Could not read that request.</ErrorNote>}
      {ticket !== null && (
        <div className="vcs-ticket flat">
          <h3>
            Request #{ticket.id}{" "}
            <Badge tone={vcsStatusTone(ticket.status)}>{vcsStatusLabel(ticket.status)}</Badge>
          </h3>
          {ticket.result_sha !== null && (
            <p>
              Wrote <code>{ticket.result_sha}</code>.
            </p>
          )}
          {ticket.failure_reason !== null && <p className="faint">{ticket.failure_reason}</p>}
          {ticket.result_sha === null && ticket.failure_reason === null && (
            <p className="faint">The row carries no sha and no reason.</p>
          )}
          <Button size="sm" onClick={() => setTicket(null)}>
            Close
          </Button>
        </div>
      )}

      <Button size="sm" variant="ghost" onClick={() => void refresh()}>
        Refresh
      </Button>
    </Panel>
  );
}

function WaitingRunsPanel({ runs, loading }: { runs: AwaitingRun[] | null; loading: boolean }) {
  return (
    <Panel
      title="Runs waiting on you"
      aside={runs === null ? undefined : `${runs.length}`}
    >
      {runs === null ? (
        !loading && <ErrorNote>Could not read the parked runs from the daemon.</ErrorNote>
      ) : runs.length === 0 ? (
        <Teach title="No run is parked.">
          A run stops here when it reaches outside its allowlist and asks. None is asking.
        </Teach>
      ) : (
        <>
          <p className="faint">
            These runs are stopped until someone answers them. The answer itself is on the Autopilot
            tab, beside the kill switch — this list only says that the run is parked, which the
            proposal queue does not.
          </p>
          <ul className="waiting-list">
            {runs.map((run) => (
              <li key={run.id}>
                <div className="waiting-row__head">
                  <b>run {run.id}</b>
                  <span className="proj">{run.project_id ?? "global"}</span>
                  <time dateTime={run.created_at} title={run.created_at}>
                    {relativeTime(run.created_at)}
                  </time>
                </div>
                <p className="waiting-row__prompt">{run.prompt}</p>
                {run.cwd !== null && (
                  <div className="faint">
                    <code>{run.cwd}</code>
                  </div>
                )}
              </li>
            ))}
          </ul>
        </>
      )}
    </Panel>
  );
}

function SkippedItemsPanel({
  items, loading, token, refresh,
}: {
  items: Proposal[] | null;
  loading: boolean;
  token: string;
  refresh: () => Promise<void>;
}) {
  const [busy, setBusy] = useState<Set<number>>(new Set());
  const [errors, setErrors] = useState<Record<number, string>>({});

  async function dismiss(id: number) {
    setBusy((current) => new Set(current).add(id));
    setErrors((current) => {
      const next = { ...current };
      delete next[id];
      return next;
    });
    const ok = await dismissSkippedItem(token, id);
    if (ok) {
      await refresh();
    } else {
      setErrors((current) => ({ ...current, [id]: "Could not put this item away." }));
    }
    setBusy((current) => {
      const next = new Set(current);
      next.delete(id);
      return next;
    });
  }

  return (
    <Panel title="Skipped items" aside={items === null ? undefined : `${items.length}`}>
      {items === null ? (
        !loading && <ErrorNote>Could not read the skipped items from the daemon.</ErrorNote>
      ) : items.length === 0 ? (
        <Teach title="Nothing was put down.">
          When a job cannot finish an item it lets go of it and moves on, so one stuck item does not
          end the night. Those items land here to be read afterwards. None has.
        </Teach>
      ) : (
        <>
          <p className="faint">
            The job already let go of these and released the worktree, so nothing is waiting on the
            answer. Putting one away only means you have read it.
          </p>
          <ul className="skipped-list">
            {items.map((item) => (
              <li key={item.id} className="skipped-row">
                <div className="skipped-row__head">
                  <b>
                    #{item.id} · run {item.run_id ?? "—"}
                  </b>
                  <span className="proj">{item.project_id ?? "global"}</span>
                  <time dateTime={item.created_at} title={item.created_at}>
                    {relativeTime(item.created_at)}
                  </time>
                </div>
                <p className="skipped-row__why">{item.reasoning}</p>
                <ConfirmButton
                  size="sm"
                  variant="link"
                  confirmLabel="Put it away?"
                  disabled={busy.has(item.id)}
                  onConfirm={() => void dismiss(item.id)}
                >
                  Dismiss
                </ConfirmButton>
                {errors[item.id] !== undefined && <ErrorNote>{errors[item.id]}</ErrorNote>}
              </li>
            ))}
          </ul>
        </>
      )}
    </Panel>
  );
}

/**
 * What departments have asked for and nobody has answered.
 *
 * The payload is rendered as what it would DO rather than as the JSON it is stored as: an email
 * shows its recipient, subject and body. **A person who cannot read what they are approving is not
 * approving anything**, and this is the one queue in the house where saying yes causes something to
 * happen out in the world rather than releasing something that had stopped.
 */
function TeamActionsPanel({
  actions, loading, token, refresh,
}: {
  actions: Proposal[] | null;
  loading: boolean;
  token: string;
  refresh: () => Promise<void>;
}) {
  const [busy, setBusy] = useState<Set<number>>(new Set());
  const [errors, setErrors] = useState<Record<number, string>>({});

  async function decide(id: number, verdict: "approve" | "reject") {
    setBusy((current) => new Set(current).add(id));
    setErrors((current) => {
      const next = { ...current };
      delete next[id];
      return next;
    });
    // The two return different shapes — approving may carry a resume run id, refusing carries
    // nothing — so they are normalised here rather than in the branches below. `rejectProposal`
    // gives a bare boolean, which is why a refusal that lost a race says less than an approval
    // that did.
    const failure: string | null =
      verdict === "approve"
        ? await approveProposal(token, id).then((outcome) =>
            outcome.ok
              ? null
              : outcome.status === 409
                ? "Somebody answered this one already."
                : outcome.reason,
          )
        : await rejectProposal(token, id).then((ok) =>
            ok ? null : "Could not record that refusal — it may already have been answered.",
          );
    if (failure === null) {
      await refresh();
    } else {
      setErrors((current) => ({ ...current, [id]: failure }));
    }
    setBusy((current) => {
      const next = new Set(current);
      next.delete(id);
      return next;
    });
  }

  return (
    <Panel title="Departments" aside={actions === null ? undefined : `${actions.length}`}>
      {actions === null ? (
        !loading && <ErrorNote>Could not read the departments' requests from the daemon.</ErrorNote>
      ) : actions.length === 0 ? (
        <Teach title="No department is waiting on you.">
          A department writes documents into its own folder on its own. Anything outside that — an
          email, a file in your folder, an hour in your calendar — it asks for, and the request
          lands here. None has.
        </Teach>
      ) : (
        <>
          <p className="faint">
            Nothing is held up by these: the department that asked has usually finished. Approving
            one makes the core do it on its next pass, which is a change out in the world rather
            than a run let through.
          </p>
          <ul className="skipped-list">
            {actions.map((action) => (
              <li key={action.id} className="skipped-row">
                <div className="skipped-row__head">
                  <b>#{action.id} · {action.tool_name ?? "an action"}</b>
                  <time dateTime={action.created_at} title={action.created_at}>
                    {relativeTime(action.created_at)}
                  </time>
                </div>
                <p className="skipped-row__why">{action.reasoning}</p>
                {action.tool_input !== null && <pre className="a-note">{action.tool_input}</pre>}
                <div className="a-actions">
                  <Button
                    size="sm"
                    variant="approve"
                    disabled={busy.has(action.id)}
                    onClick={() => void decide(action.id, "approve")}
                  >
                    Do it
                  </Button>
                  <ConfirmButton
                    size="sm"
                    variant="danger"
                    confirmLabel="Refuse it?"
                    disabled={busy.has(action.id)}
                    onConfirm={() => void decide(action.id, "reject")}
                  >
                    No
                  </ConfirmButton>
                </div>
                {errors[action.id] !== undefined && <ErrorNote>{errors[action.id]}</ErrorNote>}
              </li>
            ))}
          </ul>
        </>
      )}
    </Panel>
  );
}

interface ApprovalsProps {
  token: string | null;
  connection: ConnectionState;
}

export default function Approvals({ token, connection }: ApprovalsProps) {
  const [requests, setRequests] = useState<VcsRequestSummary[] | null>(null);
  const [runs, setRuns] = useState<AwaitingRun[] | null>(null);
  const [items, setItems] = useState<Proposal[] | null>(null);
  const [teamActions, setTeamActions] = useState<Proposal[] | null>(null);
  const [loading, setLoading] = useState(true);
  /**
   * Mirrors what the last load found still moving, for the poll's own use.
   *
   * A ref rather than state because the interval closes over it: reading `requests` there would
   * capture the array from the render that installed the timer and keep polling a queue that
   * drained ten minutes ago — or stop polling one that has since filled.
   */
  const inFlight = useRef(0);

  const load = useCallback(async () => {
    if (token === null) return;
    const [nextRequests, nextRuns, nextItems, nextTeamActions] = await Promise.all([
      listVcsRequests(token),
      getAwaitingApproval(token),
      getSkippedItems(token),
      getTeamActionProposals(token),
    ]);
    setRequests(nextRequests);
    setRuns(nextRuns);
    setItems(nextItems);
    setTeamActions(nextTeamActions);
    inFlight.current = (nextRequests === null ? 0 : vcsPending(nextRequests)) + (nextRuns?.length ?? 0);
    setLoading(false);
  }, [token]);

  useEffect(() => {
    if (connection !== "connected" || token === null) return;
    void load();
    /**
     * Polls only while something is actually in flight. A settled queue is history and does not
     * change on its own, so a timer that kept re-reading 200 finished rows every five seconds would
     * be spending the daemon's time to learn nothing.
     */
    const timer = window.setInterval(() => {
      if (inFlight.current > 0) void load();
    }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [connection, token, load]);

  if (connection !== "connected" || token === null) {
    return <ErrorNote>The daemon is not reachable, so there is nothing to show.</ErrorNote>;
  }

  return (
    <>
      <Teach title="What is not moving">
        Work that stopped and is waiting on a person. The git queue is what agents have asked to do
        to a repository; the runs are parked until you answer them on Autopilot; the departments'
        requests are things a team would like the core to do outside its own folder; the skipped
        items are what a job put down so one stuck task would not end the night.
      </Teach>

      <GitQueuePanel requests={requests} loading={loading} token={token} refresh={load} />
      <WaitingRunsPanel runs={runs} loading={loading} />
      <TeamActionsPanel actions={teamActions} loading={loading} token={token} refresh={load} />
      <SkippedItemsPanel items={items} loading={loading} token={token} refresh={load} />
    </>
  );
}
