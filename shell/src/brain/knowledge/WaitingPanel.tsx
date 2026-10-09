import { useState } from "react";
import { isApiRefusal } from "../../data/client";
import {
  APPROVABLE_SCOPES,
  groupWaiting,
  useApproveKnowledge,
  useDecideKnowledgeBatch,
  useRejectKnowledge,
  type ApproveInput,
  type Known,
} from "../../data/knowledge";
import {
  Button,
  ConfirmButton,
  Count,
  ErrorNote,
  Panel,
  Quiet,
  RefusalNote,
  Rows,
} from "../../ui";
import { KnownRow } from "./KnownRow";
import "./knowledge.css";

export interface WaitingPanelProps {
  /** The proposed rows to decide; the panel draws nothing when there are none. */
  rows: Known[];
}

/**
 * The "Lessons to approve" groups: proposed rows by scope and source, with a
 * decision per row and batch decisions per group. Owns its own mutations, so it
 * can stand anywhere a list of proposed rows can be handed to it.
 */
export function WaitingPanel({ rows }: WaitingPanelProps) {
  const approve = useApproveKnowledge();
  const reject = useRejectKnowledge();
  const batch = useDecideKnowledgeBatch();

  const groups = groupWaiting(rows);
  const deciding = approve.isPending || reject.isPending || batch.isPending;
  // The two single answers share one error slot on purpose: only one is ever in
  // flight, and a refusal from either is about the row the person just touched.
  const refusal = approve.error ?? reject.error;

  if (rows.length === 0) return null;

  return (
    <>
      {refusal !== null && <DecisionRefusal error={refusal} />}
      <Panel title="Lessons to approve" aside={<Count n={rows.length} />}>
        <p className="learned-lede">
          None reaches a prompt until you approve it; a refusal stays on the record.
        </p>
        {groups.map((group) => {
          const resultBelongsHere = sameProposalIds(
            batch.variables?.proposalIds,
            group.proposalIds,
          );
          return (
            <section
              key={group.key}
              className="learned-group"
              aria-label={`${group.scope}, ${group.source}`}
            >
              <div className="learned-group-head">
                <span>{group.scope}</span>
                <span>{group.source}</span>
                <Count n={group.rows.length} />
                {group.proposalIds.length > 1 && (
                  <>
                    <ConfirmButton
                      label={`Approve all ${group.proposalIds.length}`}
                      confirmLabel={`Let all ${group.proposalIds.length} into every later prompt`}
                      variant="approve"
                      onConfirm={() =>
                        batch.mutate({
                          action: "approve",
                          proposalIds: group.proposalIds,
                        })
                      }
                      disabled={deciding}
                    />
                    <Button
                      onClick={() =>
                        batch.mutate({
                          action: "reject",
                          proposalIds: group.proposalIds,
                        })
                      }
                      disabled={deciding}
                    >
                      Refuse all {group.proposalIds.length}
                    </Button>
                  </>
                )}
              </div>
              {resultBelongsHere &&
                !batch.isPending &&
                batch.data !== undefined &&
                batch.data.failed.length > 0 && (
                  <ErrorNote>
                    {batch.data.done} decided; {batch.data.failed.length} could not be — they
                    were already decided, or are gone. The list clears on the next read.
                  </ErrorNote>
                )}
              {resultBelongsHere && batch.isError && (
                <DecisionRefusal error={batch.error} />
              )}
              <Rows label={`${group.scope}, ${group.source}`}>
                {group.rows.map((row) => (
                  <KnownRow
                    key={row.id}
                    row={row}
                    decisions={
                      row.proposal_id === null ? (
                        // A proposed row whose question is gone cannot be decided from here, and a
                        // button that 404s is worse than none: it invites a click that teaches the
                        // person the app is broken when the daemon is merely inconsistent. Said in
                        // the row, and said as `Quiet`: what is missing here is the decision, which
                        // is the one-line absence that component is for. It was set faint, which is
                        // the rung for metadata standing beside content — here the sentence is all
                        // the row has to say in place of its two buttons.
                        <Quiet says="no question to answer — decide in the daemon" />
                      ) : (
                        <RowDecisions
                          proposalId={row.proposal_id}
                          title={row.title}
                          declaredKind={row.scope_kind}
                          declaredId={row.scope_id}
                          approve={approve}
                          reject={reject}
                          deciding={deciding}
                        />
                      )
                    }
                  />
                ))}
              </Rows>
            </section>
          );
        })}
      </Panel>
    </>
  );
}

/**
 * Approve and Refuse for one row, with the scope the approval lands in.
 *
 * The kind starts at the row's declared one when that can be approved with an
 * id, and at "as declared" otherwise (a machine row has no id to give); with
 * "as declared" the approval carries no scope and the daemon keeps the row's own.
 * Moving to another kind empties the id: the declared id names something of the
 * declared kind, and carried across it would approve into a scope nobody chose.
 */
function RowDecisions({
  proposalId,
  title,
  declaredKind,
  declaredId,
  approve,
  reject,
  deciding,
}: {
  proposalId: number;
  title: string;
  declaredKind: Known["scope_kind"];
  declaredId: string | null;
  approve: { mutate: (input: ApproveInput) => void };
  reject: { mutate: (proposalId: number) => void };
  deciding: boolean;
}) {
  const declaredApprovable = (APPROVABLE_SCOPES as readonly string[]).includes(declaredKind);
  const [kind, setKind] = useState<string>(declaredApprovable ? declaredKind : "");
  const [id, setId] = useState<string>(declaredId ?? "");
  const trimmed = id.trim();

  return (
    <>
      <span className="learned-scope-chooser" role="group" aria-label={`Scope for ${title}`}>
        <select
          aria-label="Approve into"
          value={kind}
          onChange={(event) => {
            const next = event.target.value;
            setKind(next);
            setId(next === declaredKind ? (declaredId ?? "") : "");
          }}
          disabled={deciding}
        >
          {!declaredApprovable && <option value="">as declared</option>}
          {APPROVABLE_SCOPES.map((scope) => (
            <option key={scope} value={scope}>
              {scope}
            </option>
          ))}
        </select>
        {kind !== "" && (
          <input
            type="text"
            aria-label="Scope id"
            value={id}
            onChange={(event) => setId(event.target.value)}
            disabled={deciding}
          />
        )}
      </span>
      <Button
        variant="approve"
        onClick={() =>
          approve.mutate(kind === "" ? proposalId : { proposalId, scope: `${kind}:${trimmed}` })
        }
        disabled={deciding || (kind !== "" && trimmed === "")}
      >
        Approve
      </Button>
      <Button onClick={() => reject.mutate(proposalId)} disabled={deciding}>
        Refuse
      </Button>
    </>
  );
}

/**
 * What the decision doors say when they say no.
 *
 * The 409 is the one worth writing copy for: it almost always means the row was
 * answered while it sat on screen — by the other window, or by a later text
 * superseding this one — and the list clears itself on the next read.
 */
const DECISION_SENTENCES: Record<string, string> = {
  conflict:
    "this one was already decided, or is no longer in force — the list clears it on the next read",
  not_found: "that one is gone; there is nothing left to decide",
  unprocessable:
    "the núcleo found nothing usable inside the proposal to act on",
  internal:
    "the núcleo failed while carrying the decision out — nothing was changed",
};

export function DecisionRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>the núcleo did not answer — nothing was decided</ErrorNote>
    );
  }
  return <RefusalNote refusal={error} sentences={DECISION_SENTENCES} />;
}

function sameProposalIds(left: number[] | undefined, right: number[]): boolean {
  if (left === undefined || left.length !== right.length) return false;
  const orderedLeft = [...left].sort((a, b) => a - b);
  const orderedRight = [...right].sort((a, b) => a - b);
  return orderedLeft.every((id, index) => id === orderedRight[index]);
}
