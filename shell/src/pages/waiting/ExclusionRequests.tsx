import type { Proposal } from "../../data/system";
import { useApproveProposal, useRejectProposal } from "../../data/waiting";
import { ConfirmButton, RelativeTime, Row, Rows } from "../../ui";
import type { Reading } from "./reading";
import { DecisionNotes, QUEUES, Queue, Reasoning, dense, useOrderFreeze } from "./shared";
import { taskLine } from "./ActionApprovals";

/* --------------------------------------------------- 7. exclusion requests -- */

interface ExclusionPair {
  low: number | null;
  high: number | null;
  paths: string[];
}

/** The pair an exclusion request names, out of the JSON `exclusion::propose` wrote. */
function readExclusionPair(raw: string | null): ExclusionPair {
  const empty: ExclusionPair = { low: null, high: null, paths: [] };
  if (raw === null || raw.trim() === "") return empty;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return empty;
  }
  if (typeof parsed !== "object" || parsed === null) return empty;
  const body = parsed as Record<string, unknown>;
  const paths = Array.isArray(body.paths)
    ? body.paths.filter((path): path is string => typeof path === "string")
    : [];
  return {
    low: typeof body.job_low === "number" ? body.job_low : null,
    high: typeof body.job_high === "number" ? body.job_high : null,
    paths,
  };
}

export function ExclusionRequestSection({ view }: { view: Reading<Proposal> }) {
  const approve = useApproveProposal();
  const reject = useRejectProposal();
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (proposal) => proposal.id,
  );

  return (
    <Queue
      id={QUEUES.exclusions.id}
      title={QUEUES.exclusions.title}
      view={view}
      what="the exclusion requests"
      count={items.length}
      says={QUEUES.exclusions.says}
      notes={
        <DecisionNotes
          outcome={approve.data}
          approveError={approve.isError ? approve.error : null}
          refuseError={reject.isError ? reject.error : null}
        />
      }
    >
      <Rows label={QUEUES.exclusions.title} className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((proposal) => {
          const pair = readExclusionPair(proposal.tool_input);
          const context = pair.paths.length > 0;
          return (
            <Row
              className={context ? "waiting-card waiting-item waiting-item-split" : "waiting-card waiting-item"}
              dense={dense(items.length)}
              key={proposal.id}
            >
              <div className="waiting-item-main">
                <div className="waiting-card-head">
                  <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>request #{proposal.id}</span>
                  <span className="waiting-card-title">
                    {pair.low === null || pair.high === null
                      ? "a pair this request does not name"
                      : `jobs ${pair.low} and ${pair.high}`}
                  </span>
                  <span className="waiting-meta">
                    {proposal.project_id ?? "no project"}
                  </span>
                  <RelativeTime at={proposal.created_at} />
                </div>
                <Reasoning>
                  {proposal.reasoning.trim() === ""
                    ? "nothing was recorded about why"
                    : proposal.reasoning}
                </Reasoning>
                {(proposal.job_low_task || proposal.job_high_task) && (
                  <dl className="waiting-input" aria-label={`Jobs in request #${proposal.id}`}>
                    {[
                      [pair.low, proposal.job_low_task],
                      [pair.high, proposal.job_high_task],
                    ].map(([job, task]) => (
                      <div className="waiting-input-line" key={String(job)}>
                        <dt className="waiting-input-name">job {job ?? "?"}</dt>
                        <dd className="waiting-input-value">
                          {taskLine(task as string | null) ?? "the daemon no longer has this job"}
                        </dd>
                      </div>
                    ))}
                  </dl>
                )}
                <div className="waiting-cluster">
                  <ConfirmButton
                    label={`Approve request #${proposal.id}`}
                    confirmLabel="Approve — keep these two apart"
                    subject={`#${proposal.id}`}
                    variant="approve"
                    disabled={approve.isPending || pair.low === null || pair.high === null}
                    onArmedChange={onArmedChange}
                    onConfirm={() => approve.mutate(proposal.id)}
                  />
                  <ConfirmButton
                    label={`Decline request #${proposal.id}`}
                    confirmLabel="Decline — let them run together"
                    subject={`#${proposal.id}`}
                    variant="ghost"
                    disabled={reject.isPending}
                    onArmedChange={onArmedChange}
                    onConfirm={() => reject.mutate(proposal.id)}
                  />
                </div>
              </div>
              {context && (
                <div className="waiting-item-context">
                  <div className="waiting-provenance">
                    <p className="waiting-provenance-head">files it names</p>
                    <ul className="waiting-paths">
                      {pair.paths.map((path) => (
                        <li key={path}>{path}</li>
                      ))}
                    </ul>
                  </div>
                </div>
              )}
            </Row>
          );
        })}
      </Rows>
    </Queue>
  );
}
