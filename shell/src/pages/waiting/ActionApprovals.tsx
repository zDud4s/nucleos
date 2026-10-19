import { useState } from "react";
import { Link } from "@tanstack/react-router";
import type { Proposal } from "../../data/system";
import { useApproveProposal, useDeclineAction, useRejectProposal } from "../../data/waiting";
import { Button, ConfirmButton, RelativeTime, Row, Rows } from "../../ui";
import type { Reading } from "./reading";
import { DecisionNotes, DecisionRefusal, QUEUES, Queue, Reasoning, dense, useOrderFreeze } from "./shared";
import { ReadFrom, ToolInput } from "./ToolInput";

/* ----------------------------------------------------- 2. action approvals -- */

/* --------------------------------------------- the batch on the approvals -- */

export function ActionApprovalSection({ view }: { view: Reading<Proposal> }) {
  const approve = useApproveProposal();
  const reject = useRejectProposal();
  const decline = useDeclineAction();
  /**
   * A second pair of doors for the batch, deliberately.
   *
   * `DecisionNotes` below reads the first pair, and it is a note about the last *single*
   * decision — what it did, or why it did not. A batch that failed on one row out of five
   * would overwrite that with its own refusal, on a note that belongs to a different
   * gesture. The batch reports per row instead, beside the row.
   */
  const batchApprove = useApproveProposal();
  const batchReject = useRejectProposal();
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [failed, setFailed] = useState<Map<number, unknown>>(new Map());
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (proposal) => proposal.id,
  );
  /**
   * The selection, read through the list rather than out of the Set.
   *
   * A row can be decided somewhere else while this page is open — the run resumed, another
   * window approved it — and it leaves the queue on the next read. A count taken from the Set
   * would keep counting it, and the interlock would name a decision nobody can take.
   */
  const chosen = items.filter((proposal) => selected.has(proposal.id));
  const busy = batchApprove.isPending || batchReject.isPending;
  // One decision at a time per queue: approve, decline and reject each continue or end the same
  // run, and two in flight would leave the second to lose a race the daemon settles silently.
  const deciding = approve.isPending || decline.isPending || reject.isPending;

  function toggle(id: number, on: boolean) {
    setSelected((current) => {
      const next = new Set(current);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  }

  /**
   * One call per row, one at a time, in the order they are on screen.
   *
   * The ids are taken before the first call and not read again: every decision invalidates
   * the queue `onSettled`, so the list can refetch under the loop, and a batch that re-read
   * it would be deciding a different set from the one that was named on the button. Sequential
   * and not `Promise.all` for the same reason `Files.tsx` is: the núcleo answers each of these
   * with a transaction of its own, and a failure has to be attributable to a row.
   */
  async function decide(door: {
    mutateAsync: (id: number) => Promise<unknown>;
  }) {
    const ids = chosen.map((proposal) => proposal.id);
    setFailed(new Map());
    const failures = new Map<number, unknown>();
    for (const id of ids) {
      try {
        await door.mutateAsync(id);
      } catch (error) {
        failures.set(id, error);
      }
    }
    setFailed(failures);
    // What went through is gone on the next read; what did not is still a decision to take.
    setSelected(new Set(failures.keys()));
  }

  return (
    <Queue
      title={QUEUES.approvals.title}
      view={view}
      what="the approval queue"
      count={items.length}
      says={QUEUES.approvals.says}
      notes={
        <DecisionNotes
          // A decline continues the run too, so its outcome is the note the owner reads;
          // whichever of the two was sent last is the one that answers.
          outcome={
            decline.submittedAt > approve.submittedAt ? decline.data : approve.data
          }
          approveError={approve.isError ? approve.error : null}
          refuseError={
            reject.isError ? reject.error : decline.isError ? decline.error : null
          }
        />
      }
    >
      <>
        {/* The bar is drawn whole whenever there is more than one row to pick from, and
            nothing in it changes size as boxes are ticked: the buttons carry no count and no
            subject (both would resize them, and the Decline a slip lands on with them), the
            selection line is one truncating line, and an empty selection disables rather than
            removes. Approve opens the bar and Decline closes it, so the two never sit side by
            side. */}
        {items.length > 1 && (
          <div className="waiting-bulk">
            <div className="waiting-bulk-head">
              {/* A count and nothing else. The identifiers are already on screen, as the
                  ticked boxes on the cards below; listing them here grew the line with every
                  tick and ran out of room past a handful. */}
              <p className="waiting-bulk-said" role="status">
                {chosen.length} of {items.length} selected
              </p>
              <Button
                variant="quiet"
                disabled={busy || chosen.length === items.length}
                onClick={() =>
                  setSelected(new Set(items.map((proposal) => proposal.id)))
                }
              >
                Select all
              </Button>
              <Button
                variant="quiet"
                disabled={busy || chosen.length === 0}
                onClick={() => setSelected(new Set())}
              >
                Clear
              </Button>
            </div>
            <div className="waiting-actions-split">
              <ConfirmButton
                label="Approve"
                confirmLabel="Approve the selected"
                variant="approve"
                disabled={busy || chosen.length === 0}
                onArmedChange={onArmedChange}
                onConfirm={() => void decide(batchApprove)}
              />
              <ConfirmButton
                label="Decline"
                confirmLabel="Decline and end their runs"
                variant="ghost"
                disabled={busy || chosen.length === 0}
                onArmedChange={onArmedChange}
                onConfirm={() => void decide(batchReject)}
              />
            </div>
          </div>
        )}
        <Rows label={QUEUES.approvals.title} className={dense(items.length) ? "waiting-dense" : undefined}>
          {items.map((proposal) => (
            <Row className="waiting-card" dense={dense(items.length)} key={proposal.id}>
              <div className="waiting-card-head">
                {/* The box is the smallest target on the card and the one used most when a
                    queue is worked through, so the label around it is what is clickable:
                    28 by 28 CSS pixels, with the box centred in it (`waiting.css`). */}
                <label className="waiting-select-hit">
                  <input
                    className="waiting-select"
                    type="checkbox"
                    aria-label={`Select approval #${proposal.id}`}
                    checked={selected.has(proposal.id)}
                    disabled={busy}
                    onChange={(event) =>
                      toggle(proposal.id, event.target.checked)
                    }
                  />
                </label>
                <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>approval #{proposal.id}</span>
                <span className="waiting-card-title">
                  {proposal.tool_name ?? "an action that names no tool"}
                </span>
                {/* Where it came from is two short facts, so they sit in the head as plain
                    text: a labelled grid ("RUN", "PROJECT") spent a row on words that say
                    what their values already say. */}
                {proposal.run_id === null ? (
                  <span className="waiting-meta">no run</span>
                ) : (
                  <Link
                    className="waiting-row-link"
                    to="/runs/$runId"
                    params={{ runId: String(proposal.run_id) }}
                  >
                    run {proposal.run_id}
                  </Link>
                )}
                <span className="waiting-meta">
                  {proposal.project_id ?? "no project"}
                </span>
                <RelativeTime at={proposal.created_at} />
              </div>
              <ApprovalContext proposal={proposal} />
              <div className="waiting-actions-split">
                <ConfirmButton
                  label="Approve"
                  confirmLabel="Approve and let this action happen"
                  variant="approve"
                  disabled={deciding}
                  onArmedChange={onArmedChange}
                  onConfirm={() => approve.mutate(proposal.id)}
                />
                <ConfirmButton
                  label="Decline, run continues"
                  confirmLabel="Decline action and keep the run going"
                  variant="ghost"
                  disabled={deciding}
                  onArmedChange={onArmedChange}
                  onConfirm={() => decline.mutate(proposal.id)}
                />
                <ConfirmButton
                  label="Decline, stop run"
                  confirmLabel="Decline and end the run"
                  variant="ghost"
                  disabled={deciding}
                  onArmedChange={onArmedChange}
                  onConfirm={() => reject.mutate(proposal.id)}
                />
              </div>
              {failed.has(proposal.id) && (
                <DecisionRefusal error={failed.get(proposal.id)} trustProse />
              )}
            </Row>
          ))}
        </Rows>
      </>
    </Queue>
  );
}

/** One step a run took, as `run_recent` carries it. */
interface RecentStep {
  name: string | null;
  detail: string | null;
}

/** The run's last steps, or none when the text is absent or not the array it should be. */
export function readRecentSteps(raw: string | null | undefined): RecentStep[] {
  if (raw === null || raw === undefined || raw.trim() === "") return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (step): step is RecentStep => typeof step === "object" && step !== null,
    );
  } catch {
    return [];
  }
}

/**
 * How a run's prompt opens, as the one paragraph that says what the run is for.
 *
 * The daemon cuts the prompt at 600 characters; the first paragraph is where a task, a chat
 * message or a resolver's brief says what it is, and the rest is instructions about how.
 */
export function taskLine(prompt: string | null | undefined): string | null {
  if (prompt === null || prompt === undefined) return null;
  const first = prompt.trim().split(/\n\s*\n/)[0]?.trim() ?? "";
  return first === "" ? null : first;
}

/**
 * What a person needs beside the command to answer an approval.
 *
 * The command alone says what would happen, and the classifier's reason says only why it was
 * stopped — "unrecognized shell commands require approval" is true of every second line an agent
 * writes. Whether it SHOULD happen turns on what the run is for, where it is standing, what it
 * was doing just before, and what it had read; so the card says those, in that order, under the
 * command it is asking to run.
 */
function ApprovalContext({ proposal }: { proposal: Proposal }) {
  const task = taskLine(proposal.run_prompt);
  const steps = readRecentSteps(proposal.run_recent);
  const status = proposal.run_status ?? null;
  return (
    <>
      <div>
        <p className="waiting-approval-label">held because</p>
        <Reasoning>
          {proposal.reasoning.trim() === ""
            ? "nothing was recorded about why this was stopped"
            : proposal.reasoning}
        </Reasoning>
      </div>
      <ToolInput raw={proposal.tool_input} />
      {proposal.run_cwd && (
        <p className="waiting-meta" title={proposal.run_cwd}>
          runs in <code>{proposal.run_cwd}</code>
        </p>
      )}
      {status !== null && status !== "awaiting_approval" && (
        <p className="waiting-notice" role="status">
          The run is {status.replace(/_/g, " ")}, not waiting on this — approving resumes nothing.
        </p>
      )}
      {(task !== null || steps.length > 0) && (
        <div className="waiting-provenance">
          {task !== null && (
            <>
              <p className="waiting-provenance-head">the run was asked to</p>
              <Reasoning>{task}</Reasoning>
            </>
          )}
          {steps.length > 0 && (
            <>
              <p className="waiting-provenance-head">just before, it ran</p>
              <ul
                className="waiting-provenance-list"
                aria-label={`Steps before approval #${proposal.id}`}
              >
                {steps.map((step, index) => (
                  <li className="waiting-provenance-line" key={index}>
                    <span className="waiting-provenance-tool">
                      {step.name ?? "a step"}
                    </span>
                    {step.detail && (
                      <span className="waiting-provenance-detail">
                        {step.detail}
                      </span>
                    )}
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      )}
      <ReadFrom raw={proposal.read_from} />
    </>
  );
}
