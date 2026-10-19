import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { DAEMON_NAME } from "../copy";
import { isApiRefusal } from "../data/client";
import type { Proposal } from "../data/system";
import {
  useDismissProposal,
  useRefusedActions,
  useSkippedItems,
} from "../data/waiting";
import {
  ConfirmButton,
  Count,
  ErrorNote,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  Section,
  StaleNote,
} from "../ui";
import { DecisionRefusal, useOrderFreeze } from "./waiting/shared";
import { ReadFrom, ToolInput } from "./waiting/ToolInput";
// The records wear the Waiting cards: the same head, the same facts, the same well for a payload.
import "./waiting.css";

/**
 * What the night and the injection barrier already settled, kept where governance is read.
 *
 * These two panels used to sit on the Waiting page, and they ask nobody for a decision: a skipped
 * item is work a job put down hours ago, and a refused action is something the barrier stopped in
 * a turn that has since ended. A queue whose job is deciding pending approvals should hold
 * exactly those, so they are here — on the page that answers *is anything allowed to act, and on
 * whose authority* — as overnight and barrier records.
 *
 * Nothing in either panel counts toward the Waiting headline, and nothing here is a second place
 * to decide a proposal: the one control is "put away" on a skipped item, which is a dismissal and
 * not an approval.
 */

/** Dense above three, as on Waiting: the tenth record should be on the screen with the first. Rows tighten; the text is never clamped, because these panels have no "Show all". */
function dense(count: number): boolean {
  return count > 3;
}

/**
 * The shape both panels take: a panel with the rows, or one line under a heading.
 *
 * The reasoning behind an absence is kept in the line's own disclosure rather than cut — "no job
 * put anything down" alone reads as a list that failed to load.
 */
function RecordPanel({
  title,
  what,
  says,
  why,
  rows,
  error,
  dataUpdatedAt,
  notes,
  children,
}: {
  title: string;
  what: string;
  says: string;
  why: ReactNode;
  rows: Proposal[] | undefined;
  error: unknown;
  dataUpdatedAt: number;
  notes?: ReactNode;
  children: ReactNode;
}) {
  if (rows === undefined) {
    return (
      <Section label={title}>
        {error === null ? (
          <Quiet says={`reading ${what}…`} />
        ) : isApiRefusal(error) ? (
          <RefusalNote refusal={error} />
        ) : (
          <ErrorNote>
            Could not reach the {DAEMON_NAME} — nothing is known about {what}
          </ErrorNote>
        )}
      </Section>
    );
  }
  const stale = error !== null ? <StaleNote dataUpdatedAt={dataUpdatedAt} /> : null;
  if (rows.length === 0) {
    return (
      <Section label={title}>
        <Quiet says={says}>{why}</Quiet>
        {stale}
        {notes}
      </Section>
    );
  }
  return (
    <Panel title={title} aside={<Count n={rows.length} />}>
      {stale}
      {children}
      {notes}
    </Panel>
  );
}

/* ---------------------------------------------------------------- skipped items -- */

function lostWorkOf(proposal: Proposal): { title: string; detail: string | null } {
  if (proposal.job_id == null) {
    return { title: "a job this record can no longer trace", detail: null };
  }

  const job = "job " + proposal.job_id + " \u00B7 ";
  if (typeof proposal.item_ordinal === "number") {
    // The column is 0-based; every surface renders item ordinals one-based.
    return {
      title: job + "item " + (proposal.item_ordinal + 1),
      detail: proposal.item_description ?? null,
    };
  }
  if (proposal.run_stage === "review") return { title: job + "the job's review", detail: null };
  if (proposal.run_stage === "plan") return { title: job + "the job's plan", detail: null };
  return { title: job + "an item it no longer names", detail: null };
}

export function SkippedItemsPanel() {
  const query = useSkippedItems();
  const dismiss = useDismissProposal();
  const rows = query.data ?? [];
  const { items, onArmedChange } = useOrderFreeze(rows, (proposal) => proposal.id);

  return (
    <RecordPanel
      title="Skipped items"
      what="the skipped items"
      says="no job put anything down"
      why={
        <>
          Work a job put down overnight because it needed a decision, and
          carried on without. Nothing resumes from here — the tree moved on
          hours ago — so the only thing left is to read it and put it away.
          This is a record of a decision the night already took, not one held
          for you, which is why the Waiting page does not list it.
        </>
      }
      rows={query.data === undefined ? undefined : items}
      error={query.isError ? query.error : null}
      dataUpdatedAt={query.dataUpdatedAt}
      notes={dismiss.isError ? <DecisionRefusal error={dismiss.error} trustProse={false} /> : null}
    >
      <Rows label="Skipped items">
        {items.map((proposal) => {
          const work = lostWorkOf(proposal);
          return (
            <Row className="waiting-card" dense={dense(items.length)} key={proposal.id}>
              <div className="waiting-card-head">
                <span className="waiting-card-id">item #{proposal.id}</span>
                <span className="waiting-card-title">{work.title}</span>
                {proposal.run_id !== null && (
                  <Link className="waiting-row-link" to={`/runs/${proposal.run_id}`}>
                    run {proposal.run_id}
                  </Link>
                )}
                <span className="waiting-meta">{proposal.project_id ?? "no project"}</span>
                <RelativeTime at={proposal.created_at} />
              </div>
              {work.detail !== null && <p className="waiting-reasoning">{work.detail}</p>}
              <p className="waiting-meta">
                <span>{proposal.tool_name ?? "an action that names no tool"}</span>{" — "}
                <span>
                  {proposal.reasoning.trim() === ""
                    ? "nothing was recorded about why"
                    : proposal.reasoning}
                </span>
              </p>
              <ToolInput raw={proposal.tool_input} />
              <div className="waiting-actions">
                {/* `/dismiss`, never `/reject`: rejecting guards on
                    `action-approval` and would answer 409 for every one of these. */}
                <ConfirmButton
                  label={`Put item #${proposal.id} away`}
                  confirmLabel="I have read it"
                  subject={`#${proposal.id}`}
                  variant="quiet"
                  disabled={dismiss.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => dismiss.mutate(proposal.id)}
                />
              </div>
            </Row>
          );
        })}
      </Rows>
    </RecordPanel>
  );
}

/* --------------------------------------------------------------- refused actions -- */

/**
 * What the injection barrier refused — a record, and deliberately buttonless.
 *
 * The daemon would accept a dismiss for these, and the design still asks for no
 * controls, which is the right call: the turn that reached for this ended long
 * ago, so there is nothing to allow and nothing to release. The only useful
 * response is to go and do the thing yourself, or to decide the agent was wrong
 * to try — and neither of those is a button on this page. A dismiss button here
 * would read as "handled" for something nobody handled.
 */
export function RefusedActionsPanel() {
  const query = useRefusedActions();
  const rows = query.data ?? [];

  return (
    <RecordPanel
      title="Refused actions"
      what="the refused actions"
      says="the barrier has refused nothing"
      why={
        <>
          The barrier stopped these before they happened, in turns that have
          since ended. There is nothing here to allow: what they were going to
          do is written out so you can decide whether to do it yourself, and
          under it what the turn had read when it decided to — which is usually
          the half that answers whether the idea was the agent&apos;s or a
          stranger&apos;s. The barrier already answered these, and what is left
          is a record to read.
        </>
      }
      rows={query.data}
      error={query.isError ? query.error : null}
      dataUpdatedAt={query.dataUpdatedAt}
    >
      <Rows label="Refused actions">
        {rows.map((proposal) => (
          <Row className="waiting-card" dense={dense(rows.length)} key={proposal.id}>
            <div className="waiting-card-head">
              <span className="waiting-card-id">refusal #{proposal.id}</span>
              <span className="waiting-card-title">
                {proposal.tool_name ?? "an action that names no tool"}
              </span>
              <RelativeTime at={proposal.created_at} />
            </div>
            <p className="waiting-reasoning">
              {proposal.reasoning.trim() === ""
                ? "nothing was recorded about why"
                : proposal.reasoning}
            </p>
            <ToolInput raw={proposal.tool_input} />
            <ReadFrom raw={proposal.read_from} />
          </Row>
        ))}
      </Rows>
    </RecordPanel>
  );
}

