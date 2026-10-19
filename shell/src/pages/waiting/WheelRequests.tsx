import { Link } from "@tanstack/react-router";
import { useRejectProposal, type WheelRequest } from "../../data/waiting";
import { useApproveWheel } from "../../data/browser";
import { ConfirmButton, RelativeTime, Row, Rows } from "../../ui";
import type { Reading } from "./reading";
import { DecisionNotes, QUEUES, Queue, Reasoning, dense, useOrderFreeze } from "./shared";

/* ------------------------------------------------------- 1. wheel requests -- */

export function WheelRequestSection({ view }: { view: Reading<WheelRequest> }) {
  const approve = useApproveWheel();
  const reject = useRejectProposal();
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (session) => session.proposal_id,
  );

  return (
    <Queue
      title={QUEUES.wheel.title}
      view={view}
      what="the open browser sessions"
      count={items.length}
      says={QUEUES.wheel.says}
      // The receipt rides in the notes and not in the children: approving the last card
      // empties the list, and the link to the session is the one thing the person needs next.
      notes={
        <>
          {approve.isSuccess && approve.variables?.seat === "shell" && (
            <p className="waiting-outcome" role="status">
              the shell has the wheel — <Link to="/web">drive it on Web</Link>
            </p>
          )}
          <DecisionNotes
            outcome={undefined}
            approveError={approve.isError ? approve.error : null}
            refuseError={reject.isError ? reject.error : null}
          />
        </>
      }
    >
      <Rows label={QUEUES.wheel.title} className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((session) => (
          <Row className="waiting-card" dense={dense(items.length)} key={session.id}>
            <div className="waiting-card-head">
              <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>
                wheel #{session.proposal_id}
              </span>
              <span className="waiting-meta">
                {session.project_id ?? "no project"}
              </span>
              <span className="waiting-meta">
                {session.profile_kind} {session.profile_id}
              </span>
              {session.run_id !== null && (
                <Link
                  className="waiting-row-link"
                  to="/runs/$runId"
                  params={{ runId: String(session.run_id) }}
                >
                  run {session.run_id}
                </Link>
              )}
              <RelativeTime at={session.opened_at} />
            </div>
            <dl className="waiting-facts">
              <div className="waiting-fact">
                <dt>asked for</dt>
                {/* Verbatim, punycode and all. A host prettified out of its
                    `xn--` form is exactly the lookalike this decision exists
                    to catch, and the shell must never be the thing that hides
                    it. */}
                <dd className="waiting-url">{session.requested_url}</dd>
              </div>
              {session.final_url !== session.requested_url && (
                <div className="waiting-fact">
                  <dt>ended at</dt>
                  <dd className="waiting-url">{session.final_url}</dd>
                </div>
              )}
              <div className="waiting-fact">
                <dt>rule</dt>
                <dd>{session.rule}</dd>
              </div>
            </dl>
            {session.refusal !== null && (
              <Reasoning>{session.refusal}</Reasoning>
            )}
            <div className="waiting-actions">
              {session.shell_eligible && (
                <ConfirmButton
                  label={`Approve wheel #${session.proposal_id}`}
                  confirmLabel="Approve and drive it here in the shell"
                  variant="approve"
                  disabled={approve.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() =>
                    approve.mutate({ proposalId: session.proposal_id, sessionId: session.id, seat: "shell" })
                  }
                />
              )}
              {session.shell_eligible ? (
                <ConfirmButton
                  label={`Open real window for wheel #${session.proposal_id}`}
                  confirmLabel="Open a real browser here"
                  variant="ghost"
                  disabled={approve.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() =>
                    approve.mutate({ proposalId: session.proposal_id, sessionId: session.id, seat: "window" })
                  }
                />
              ) : (
                <ConfirmButton
                  label={`Open real window for wheel #${session.proposal_id}`}
                  confirmLabel="Open a real browser here"
                  variant="approve"
                  disabled={approve.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() =>
                    approve.mutate({ proposalId: session.proposal_id, sessionId: session.id, seat: "window" })
                  }
                />
              )}
              <ConfirmButton
                label={`Decline wheel #${session.proposal_id}`}
                confirmLabel="Decline and close the session"
                subject={`#${session.proposal_id}`}
                variant="ghost"
                disabled={reject.isPending}
                onArmedChange={onArmedChange}
                onConfirm={() => reject.mutate(session.proposal_id)}
              />
            </div>
          </Row>
        ))}
      </Rows>
    </Queue>
  );
}
