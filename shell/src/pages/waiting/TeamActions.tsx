import { Link } from "@tanstack/react-router";
import type { Proposal } from "../../data/system";
import { parseActionPayload, teamActionState, useApproveTeamAction, type TeamAction } from "../../data/teams";
import { useRejectProposal } from "../../data/waiting";
import { ConfirmButton, RelativeTime, Row, Rows, StateBadge } from "../../ui";
import type { Reading } from "./reading";
import { DecisionNotes, QUEUES, Queue, Reasoning, dense, useOrderFreeze } from "./shared";
import { type InputField, plainValue } from "./ToolInput";
import { taskLine } from "./ActionApprovals";

/* --------------------------------------------------------- 3. team actions -- */

/**
 * What a department has asked the núcleo to carry out under a `propose` grant.
 *
 * Approving is not the action itself — the núcleo carries it out on its next
 * tick, about ten seconds later — so the card shows two facts side by side
 * rather than collapsing them: the human decision, and separately, whether the
 * núcleo has gotten to it yet.
 */
export function TeamActionSection({
  view,
  executing,
}: {
  view: Reading<Proposal>;
  executing: TeamAction[] | undefined;
}) {
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (proposal) => proposal.id,
  );

  return (
    <Queue
      id={QUEUES.teamActions.id}
      title={QUEUES.teamActions.title}
      view={view}
      what="the team's action requests"
      count={items.length}
      says={QUEUES.teamActions.says}
    >
      <Rows label={QUEUES.teamActions.title} className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((proposal) => (
          <TeamActionCard
            key={proposal.id}
            proposal={proposal}
            count={items.length}
            executing={executing}
            onArmedChange={onArmedChange}
          />
        ))}
      </Rows>
    </Queue>
  );
}

/** One team action's payload, per `tool_name` — the three kinds a grant can ever name. */
function payloadFields(
  kind: string | null,
  parsed: Record<string, unknown>,
): InputField[] {
  const field = (value: unknown): string =>
    value === undefined ? "not given" : plainValue(value);
  switch (kind) {
    case "send_email":
      return [
        { name: "to", value: field(parsed.to) },
        { name: "subject", value: field(parsed.subject) },
        { name: "body", value: field(parsed.body) },
      ];
    case "file_document":
      return [
        { name: "path", value: field(parsed.path) },
        { name: "title", value: field(parsed.title) },
      ];
    case "calendar_event":
      return [
        { name: "title", value: field(parsed.title) },
        { name: "start", value: field(parsed.start) },
        { name: "duration", value: field(parsed.duration) },
      ];
    default:
      return Object.entries(parsed).map(([name, value]) => ({
        name,
        value: plainValue(value),
      }));
  }
}

/**
 * A team action's payload, as fields a person can actually decide on.
 *
 * `tool_input` is the daemon's canonical re-serialisation, so parsing it a
 * second time can fail on data that is not ours to fix — the raw string is the
 * fallback, never a blank card.
 */
function TeamActionPayload({
  kind,
  raw,
}: {
  kind: string | null;
  raw: string | null;
}) {
  if (raw === null || raw.trim() === "") return null;
  const parsed = parseActionPayload(raw);
  if (parsed === null) {
    return <p className="waiting-payload-raw">{raw}</p>;
  }
  const fields = payloadFields(kind, parsed);
  if (fields.length === 0) return null;
  return (
    <dl className="waiting-input">
      {fields.map((line) => (
        <div className="waiting-input-line" key={line.name}>
          <dt className="waiting-input-name">{line.name}</dt>
          <dd className="waiting-input-value">{line.value}</dd>
        </div>
      ))}
    </dl>
  );
}

/** Whether a department asked at all, which is when the context column has anything to say. */
export function hasDepartmentContext(proposal: Proposal): boolean {
  return (proposal.team_name ?? null) !== null || (proposal.team_run_id ?? null) !== null;
}

/**
 * Which department asked, who in it, and what it was working on.
 *
 * A team action or a recruit names what would happen and why the asker wants it, but not who the
 * asker is or what job it is doing — and "send this email" is a different decision coming from
 * the director of a reconciliation than from a specialist halfway through one item of it.
 */
export function DepartmentContext({ proposal }: { proposal: Proposal }) {
  const team = proposal.team_name ?? null;
  const runId = proposal.team_run_id ?? null;
  const task = taskLine(proposal.team_request);
  if (!hasDepartmentContext(proposal)) return null;
  return (
    <>
      <p className="waiting-meta waiting-asked">
        asked by {proposal.asked_by ?? "an agent the daemon no longer names"}
        {proposal.asked_for ? (
          <>
            , working on <q>{proposal.asked_for}</q>
          </>
        ) : null}
        {" in "}
        {runId !== null ? (
          <Link className="waiting-row-link" to="/team-runs/$runId" params={{ runId }}>
            {team ?? "a department"}
          </Link>
        ) : (
          team
        )}
      </p>
      {task !== null && (
        <div className="waiting-provenance">
          <p className="waiting-provenance-head">the department was asked to</p>
          <Reasoning>{task}</Reasoning>
        </div>
      )}
    </>
  );
}

function TeamActionCard({
  proposal,
  count,
  executing,
  onArmedChange,
}: {
  proposal: Proposal;
  /** How many rows the list has, which is what decides how tight this one is. */
  count: number;
  executing: TeamAction[] | undefined;
  onArmedChange: (armed: boolean) => void;
}) {
  const approve = useApproveTeamAction();
  const reject = useRejectProposal();
  // `GET /team-actions` lists only `pending` and `working`, so an action that
  // moved on between polls simply is not in this list — absent is not a state.
  const action = executing?.find((row) => row.proposal_id === proposal.id);

  const context = hasDepartmentContext(proposal);

  return (
    <Row
      className={context ? "waiting-card waiting-item waiting-item-split" : "waiting-card waiting-item"}
      dense={dense(count)}
    >
      <div className="waiting-item-main">
        <div className="waiting-card-head">
          <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>team action #{proposal.id}</span>
          <span className="waiting-card-title">
            {proposal.tool_name ?? "an action that names no tool"}
          </span>
          <span className="waiting-meta">
            {proposal.project_id ?? "no project"}
          </span>
          <RelativeTime at={proposal.created_at} />
          {/* The núcleo's half of the story, at the far end of the identity line: the owner's
              decision is below it, and this is what happens after. */}
          <dl className="waiting-head-fact">
            <dt>execution</dt>
            <dd>
              {action === undefined ? (
                <span className="waiting-meta">
                  the execution state is not known yet
                </span>
              ) : (
                <StateBadge
                  domain="team_action"
                  state={teamActionState(action)}
                />
              )}
            </dd>
          </dl>
        </div>
        <Reasoning>
          {proposal.reasoning.trim() === ""
            ? "nothing was recorded about why"
            : proposal.reasoning}
        </Reasoning>
        <TeamActionPayload kind={proposal.tool_name} raw={proposal.tool_input} />
        <div className="waiting-cluster">
          <ConfirmButton
            label="Approve"
            confirmLabel="Approve and let this action happen"
            variant="approve"
            disabled={approve.isPending}
            onArmedChange={onArmedChange}
            onConfirm={() => approve.mutate(proposal.id)}
          />
          <ConfirmButton
            label="Decline"
            confirmLabel="Decline and close the action"
            variant="ghost"
            disabled={reject.isPending}
            onArmedChange={onArmedChange}
            onConfirm={() => reject.mutate(proposal.id)}
          />
        </div>
        <DecisionNotes
          outcome={approve.data}
          approveError={approve.isError ? approve.error : null}
          refuseError={reject.isError ? reject.error : null}
        />
      </div>
      {context && (
        <div className="waiting-item-context">
          <DepartmentContext proposal={proposal} />
        </div>
      )}
    </Row>
  );
}
