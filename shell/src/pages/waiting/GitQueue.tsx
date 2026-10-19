import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { VCS_LIST_LIMIT, useApproveVcsRequest, useDismissVcsRequest, useRefuseVcsRequest, type VcsRequestSummary } from "../../data/waiting";
import { ConfirmButton, Count, Quiet, RelativeTime, Row, Rows, Section, StateBadge } from "../../ui";
import type { Reading } from "./reading";
import { DecisionNotes, QUEUES, Queue, Reasoning, dense, useOrderFreeze } from "./shared";

/* --------------------------------------------------------------- 7. git queue -- */

/**
 * The git queue: one status asks for a decision, and the rest is for reading.
 *
 * `awaiting_owner` is the only row this panel can be *decided* on — a merge that changes the
 * test map, held until somebody approves or declines it. `escalated` is a *normal outcome*
 * (somebody owns the conflict now, which is the queue working rather than breaking) and
 * `blocked` is terminal without being a failure (the queue will not retry it; the answer is to
 * fix the tree and submit again). Both still want a person, in that they are counted in the
 * headline, but all anyone can do is read them and put them away — so they sit behind the
 * disclosure at the bottom with the history, and not between the decision and the person
 * making it. `ui/state-map.ts` holds the tones.
 */
/** How much history to show behind the rows that want a person. */
const VCS_RECENT = 5;

export function GitQueuePanel({
  history,
  waiting,
}: {
  history: Reading<VcsRequestSummary>;
  waiting: Reading<VcsRequestSummary>;
}) {
  const dismiss = useDismissVcsRequest();
  const approve = useApproveVcsRequest();
  const refuse = useRefuseVcsRequest();
  // What wants a person comes from `/waiting/git`, never from filtering the history: that listing
  // is capped at 200 and keeps every escalation that has since been settled.
  const wanted = waiting.rows ?? [];
  // One order for both groups: arming any control pins the sequence, so a poll that reorders
  // the list cannot move another request under the finger that is about to confirm.
  const { items, onArmedChange } = useOrderFreeze(wanted, (row) => row.id);
  const owed = items.filter((row) => row.status === "awaiting_owner");
  const toRead = items.filter((row) => row.status !== "awaiting_owner");
  const rows = history.rows ?? [];
  const rest = rows.filter((row) => !wanted.some((open) => open.id === row.id));
  const recent = rest.slice(0, VCS_RECENT);
  const behind = toRead.length + recent.length;

  return (
    <Queue
      title={QUEUES.git.title}
      view={waiting}
      what="the git queue"
      says={QUEUES.git.says}
      count={wanted.length}
      aside={<Count n={wanted.length} />}
      notes={
        <DecisionNotes
          outcome={undefined}
          approveError={null}
          refuseError={
            approve.isError
              ? approve.error
              : refuse.isError
                ? refuse.error
                : dismiss.isError
                  ? dismiss.error
                  : null
          }
        />
      }
    >
      {owed.length === 0 ? (
        <Quiet says="nothing in the git queue needs your approval." />
      ) : (
        <Rows label="Git requests waiting on you" className={dense(owed.length) ? "waiting-dense" : undefined}>
          {owed.map((row) => (
            <VcsRow key={row.id} row={row} count={owed.length} decision>
              <div className="waiting-actions-split">
                <ConfirmButton
                  label="Approve"
                  confirmLabel="Approve the map change"
                  variant="approve"
                  disabled={approve.isPending || refuse.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => {
                    refuse.reset();
                    dismiss.reset();
                    approve.mutate(row.id);
                  }}
                />
                <ConfirmButton
                  label="Decline"
                  confirmLabel="Decline the map change"
                  variant="ghost"
                  disabled={approve.isPending || refuse.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => {
                    approve.reset();
                    dismiss.reset();
                    refuse.mutate(row.id);
                  }}
                />
              </div>
            </VcsRow>
          ))}
        </Rows>
      )}
      {/* Everything that is read rather than decided, below what is, under its own headings.
          It sat behind a disclosure until the owner dropped it (2026-10-09); the list is
          short by construction — the escalations plus at most `VCS_RECENT` rows. */}
      {(behind > 0 || rows.length >= VCS_LIST_LIMIT) && (
        <>
          {toRead.length > 0 && (
            <div className="waiting-recent">
              <Section label="escalated or blocked" level={3}>
                <Rows label="Git requests to put away">
                  {toRead.map((row) => (
                    <VcsRow key={row.id} row={row} count={toRead.length}>
                      <div className="waiting-actions">
                        <ConfirmButton
                          label={`Put ${row.op} #${row.id} away`}
                          confirmLabel="Nobody needs to act"
                          subject={`#${row.id}`}
                          variant="quiet"
                          disabled={dismiss.isPending}
                          onArmedChange={onArmedChange}
                          onConfirm={() => dismiss.mutate(row.id)}
                        />
                      </div>
                    </VcsRow>
                  ))}
                </Rows>
              </Section>
            </div>
          )}
          {recent.length > 0 && (
            <div className="waiting-recent">
              <Section label="recently through the queue" level={3}>
                <Rows label="Recent git requests">
                  {recent.map((row) => (
                    <VcsRow key={row.id} row={row} count={recent.length} />
                  ))}
                </Rows>
              </Section>
            </div>
          )}
          {/* Nothing prunes `vcs_requests`, so this listing is the whole history and
              not a backlog. Arriving at the cap says the daemon has been running a
              while — it is not a finding and is not drawn as one. */}
          {rows.length >= VCS_LIST_LIMIT && (
            <p className="waiting-ceiling">
              the listing stops at {VCS_LIST_LIMIT} rows — this is the queue&apos;s
              whole history, kept rather than pruned, so a full listing only means
              the daemon has been running a while
            </p>
          )}
        </>
      )}
    </Queue>
  );
}

function VcsRow({
  row,
  count,
  decision,
  children,
}: {
  row: VcsRequestSummary;
  count: number;
  /** A request somebody is being asked to decide: a card, with a heading focus can land on. */
  decision?: boolean;
  children?: ReactNode;
}) {
  const conflicts = conflictedFiles(row.output_tail);
  const next = gitNextStep(row);
  // The queue's own reason, when nothing more specific says why. A conflict's reason is written
  // for the agent that asked ("not yours to fix from here") and the files say it better.
  const reason = conflicts.length === 0 ? row.failure_reason : null;
  return (
    <Row className={decision === true ? "waiting-card" : undefined} dense={dense(count)}>
      <div className="waiting-card-head">
        <span
          className="waiting-card-id"
          tabIndex={decision === true ? -1 : undefined}
          data-waiting-heading={decision === true ? "" : undefined}
        >
          {row.op} #{row.id}
        </span>
        <StateBadge
          domain="vcs"
          state={row.status === "escalated" && next !== null && !next.yours ? "resolving" : row.status}
        />
        <span className="waiting-meta">{row.project_id}</span>
        <RelativeTime at={row.created_at} />
      </div>
      {/* Which branches, first: "merge #573" alone does not say what would be merged. */}
      {row.source != null && row.target != null && (
        <p className="waiting-git-branches">
          <code>{row.source}</code> into <code>{row.target}</code>
        </p>
      )}
      <p className="waiting-meta" title={row.repo_key}>
        {repoLabel(row.repo_key)} — asked from {row.origin}
      </p>
      {conflicts.length > 0 && (
        <div className="waiting-git-conflicts">
          <span className="waiting-meta">conflicts in</span>
          <ul aria-label={`Conflicts in ${row.op} #${row.id}`}>
            {conflicts.map((file) => (
              <li key={file}>
                <code>{file}</code>
              </li>
            ))}
          </ul>
        </div>
      )}
      {reason != null && reason !== "" && <Reasoning>{reason}</Reasoning>}
      {/* And what to do about it is its own line, not a third clause on the faint mono one. */}
      {next !== null && (
        <p className={next.yours ? "waiting-hint waiting-hint-yours" : "waiting-hint"}>
          {next.before}
          {next.run !== undefined && (
            <Link className="waiting-row-link" to="/runs/$runId" params={{ runId: String(next.run) }}>
              run #{next.run}
            </Link>
          )}
          {next.after}
        </p>
      )}
      {children}
    </Row>
  );
}

/** The repository key without Windows' `\\?\` verbatim prefix, which only git needs. */
function repoLabel(key: string): string {
  return key.replace(/^\\\\\?\\/, "");
}

/**
 * The files git named in a conflict, read from its own output.
 *
 * `CONFLICT (content): Merge conflict in a.rs` gives `a.rs`; the other kinds
 * (`modify/delete`, `rename/rename`, …) have no fixed shape, so their whole message is kept rather
 * than guessed at.
 */
export function conflictedFiles(tail: string | null | undefined): string[] {
  if (tail == null) return [];
  const found: string[] = [];
  for (const match of tail.matchAll(/^CONFLICT \([^)]*\): (.+)$/gm)) {
    const file = match[1].replace(/^Merge conflict in /, "").trim();
    if (!found.includes(file)) found.push(file);
  }
  return found;
}

/**
 * What happens next on a git request, and whether that is the reader's to do.
 *
 * Split around the run it names so the run can be a link. `yours` is the line that asks the
 * person to act; every other line says who is acting instead, which is what lets somebody leave an
 * escalation alone without wondering whether it is stuck.
 */
export function gitNextStep(
  row: VcsRequestSummary,
): { before: string; run?: number; after: string; yours: boolean } | null {
  if (row.status === "blocked") return { before: "submit it again", after: "", yours: true };
  if (row.status === "awaiting_owner")
    return { before: "changes the test map — approve to let it land", after: "", yours: true };
  if (row.status !== "escalated" || row.op !== "merge") return null;
  if (row.from_resolution === true)
    return {
      before:
        "this merge was itself a resolution landing and conflicted again, so no agent is given it — resolve it by hand, or put it away if the merge no longer matters",
      after: "",
      yours: true,
    };
  const run = row.resolution_run_id;
  if (run == null)
    return {
      before: "an agent is given this conflict within a minute — nothing to do yet",
      after: "",
      yours: false,
    };
  switch (row.resolution_status) {
    case "running":
      return { before: "", run, after: " is resolving it — nothing to do yet", yours: false };
    case "awaiting_approval":
      return { before: "", run, after: " is resolving it and waits on your approval", yours: true };
    case "completed": {
      // The resolver hands a finished resolution to the queue only when its gate passed (or never
      // ran) and its worktree is still there (`resolver::land_finished`); otherwise nothing lands
      // it, and the row is the person's.
      const gate = row.resolution_gate_status;
      if (gate != null && gate !== "passed")
        return {
          before: "",
          run,
          after: ` finished its resolution but its gate ${gate}, so the queue will not land it — open the run, land it by hand, or put it away if the merge no longer matters`,
          yours: true,
        };
      if (row.resolution_worktree_gone === true)
        return {
          before: "",
          run,
          after:
            " finished its resolution but its worktree is gone, so the queue cannot land it — resolve it by hand, or put it away if the merge no longer matters",
          yours: true,
        };
      return {
        before: "",
        run,
        after:
          " finished its resolution; the queue lands it and this clears on its own — if it stays, open the run to see why",
        yours: false,
      };
    }
    case "failed":
    case "cancelled":
    case "interrupted":
    case "timed_out":
    case "superseded":
      return {
        before: "",
        run,
        after: ` ended (${row.resolution_status}) without resolving it — resolve it by hand, or put it away if the merge no longer matters`,
        yours: true,
      };
    default:
      return { before: "an agent was given it as ", run, after: "", yours: false };
  }
}
