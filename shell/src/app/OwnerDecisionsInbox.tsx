import { useState } from "react";
import {
  answerableCount,
  isAnswerable,
  useAnswerOwnerDecision,
  useOwnerDecisions,
  type OwnerDecision,
} from "../data/owner-decisions";
import { Button, ErrorNote, Modal } from "../ui";

/** Whole minutes/hours/days since `at`, as short text. Empty when `at` does not parse. */
export function ageOf(at: string, now: number = Date.now()): string {
  const then = Date.parse(at);
  if (Number.isNaN(then)) return "";
  const minutes = Math.max(0, Math.floor((now - then) / 60000));
  if (minutes < 60) return `${minutes}m`;
  if (minutes < 60 * 24) return `${Math.floor(minutes / 60)}h`;
  return `${Math.floor(minutes / (60 * 24))}d`;
}

/**
 * The owner decisions inbox: a button in the persistent chrome with the count of answerable open
 * decisions, opening a panel to read and answer them. Notices are listed greyed and never counted.
 */
export function OwnerDecisionsInbox() {
  const [open, setOpen] = useState(false);
  const decisions = useOwnerDecisions();
  const count = answerableCount(decisions.data);

  return (
    <>
      <Button variant="ghost" onClick={() => setOpen(true)}>
        Decisions{count === undefined ? "" : ` ${count}`}
      </Button>
      <Modal open={open} onOpenChange={setOpen} title="Decisions" size="lg">
        {decisions.isError ? <ErrorNote>The open decisions could not be read.</ErrorNote> : null}
        {decisions.data !== undefined && decisions.data.length === 0 ? (
          <p className="text-sm text-text-faint">Nothing is waiting for you.</p>
        ) : null}
        <ul className="flex flex-col gap-4">
          {(decisions.data ?? []).map((d) => (
            <li key={d.id} data-testid={`owner-decision-${d.id}`}>
              <DecisionRow decision={d} />
            </li>
          ))}
        </ul>
      </Modal>
    </>
  );
}

function DecisionRow({ decision }: { decision: OwnerDecision }) {
  const [text, setText] = useState("");
  const answer = useAnswerOwnerDecision();
  const answerable = isAnswerable(decision);
  const where = [decision.tree_id, decision.node_id].filter((x) => x !== null).join(" / ");

  return (
    <div className={answerable ? "flex flex-col gap-2" : "flex flex-col gap-2 opacity-60"}>
      <p className="text-xs text-text-faint">
        {where === "" ? decision.id : where} · {ageOf(decision.at)}{" "}
        {answerable ? null : <span className="text-xs text-text-faint">notice</span>}
      </p>
      <pre className="text-sm" style={{ maxHeight: "16rem", overflow: "auto", whiteSpace: "pre-wrap" }}>
        {decision.question}
      </pre>
      {answerable ? (
        <>
          <textarea
            aria-label="Your answer"
            className="w-full text-sm"
            rows={3}
            value={text}
            onChange={(event) => setText(event.target.value)}
          />
          {answer.isError ? <ErrorNote>The answer could not be sent.</ErrorNote> : null}
          <div>
            <Button
              variant="primary"
              disabled={text.trim() === "" || answer.isPending}
              onClick={() => answer.mutate({ id: decision.id, answer: text.trim() })}
            >
              Send
            </Button>
          </div>
        </>
      ) : null}
    </div>
  );
}
