import { useDecideContactMerge, type MergeSide, type MergeSuggestion } from "../../data/waiting";
import { ConfirmButton, ConflictNote, RelativeTime, Row, Rows, Section } from "../../ui";
import type { Reading } from "./reading";
import { DecisionNotes, QUEUES, Queue, Reasoning, dense, useOrderFreeze } from "./shared";

/* ------------------------------------------------------- 5. contact merges -- */

/**
 * One side of a suggested merge, under the word for what happens to it.
 *
 * `kept` and `absorbed` are a heading and not a field label — the whole point of
 * showing both sides is that the merge is not symmetrical, and which of the two
 * survives is the first thing a reader has to know. `Section` is the heading
 * rank; the label rank it used to be written in is what a `dt` gets, and at 11px
 * the two are told apart by 0.06em of tracking and nothing else.
 */
function MergeSideView({ side, role }: { side: MergeSide; role: string }) {
  return (
    <Section label={role} level={3}>
      <div className="waiting-side">
        <p className="waiting-side-name">
          {side.display_name ?? "no name recorded"}
        </p>
        <ul className="waiting-addresses">
          {side.addresses.map((address) => (
            <li key={address}>{address}</li>
          ))}
        </ul>
        <p className="waiting-meta">{side.messages_in} messages in</p>
        <p className="waiting-verdict">
          {side.verdict === null
            ? "no standing decision"
            : `standing decision: ${side.verdict}`}
        </p>
      </div>
    </Section>
  );
}

/** Two standing decisions that disagree, which is the 409 this card can predict. */
function verdictsConflict(suggestion: MergeSuggestion): boolean {
  const keep = suggestion.keep.verdict;
  const absorb = suggestion.absorb.verdict;
  return keep !== null && absorb !== null && keep !== absorb;
}

export function ContactMergeSection({ view }: { view: Reading<MergeSuggestion> }) {
  const decide = useDecideContactMerge();
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (suggestion) => suggestion.proposal_id,
  );

  return (
    <Queue
      id={QUEUES.merges.id}
      title={QUEUES.merges.title}
      view={view}
      what="the suggested merges"
      count={items.length}
      says={QUEUES.merges.says}
      /* One mutation for both verdicts, so the refusal is shown once — and its
         prose is trusted, because the 409 here is the refusal that names the two
         decisions that disagree. */
      notes={
        <DecisionNotes
          outcome={decide.data}
          approveError={decide.isError ? decide.error : null}
          refuseError={null}
        />
      }
    >
      <Rows label={QUEUES.merges.title} className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((suggestion) => (
          <Row className="waiting-card waiting-item" dense={dense(items.length)} key={suggestion.proposal_id}>
            <div className="waiting-item-main">
              <div className="waiting-card-head">
                <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>
                  merge #{suggestion.proposal_id}
                </span>
                <RelativeTime at={suggestion.created_at} />
              </div>
              <Reasoning>{suggestion.reasoning}</Reasoning>
              <div className="waiting-merge">
                <MergeSideView side={suggestion.keep} role="kept" />
                <MergeSideView side={suggestion.absorb} role="absorbed" />
              </div>
              {/* Said before the button rather than after the 409: the daemon
                  refuses a merge whose two people carry decisions that
                  contradict, and the remedy is to settle one of them first. */}
              {verdictsConflict(suggestion) && (
                <ConflictNote>
                  These two carry standing decisions that disagree, so the daemon
                  will refuse the merge — settle one of them and decide this
                  again.
                </ConflictNote>
              )}
              <div className="waiting-cluster">
                <ConfirmButton
                  label={`Merge #${suggestion.proposal_id}`}
                  confirmLabel="Merge — they are one person"
                  subject={`#${suggestion.proposal_id}`}
                  variant="approve"
                  disabled={decide.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() =>
                    decide.mutate({
                      proposalId: suggestion.proposal_id,
                      verdict: "approve",
                    })
                  }
                />
                <ConfirmButton
                  label={`Decline merge #${suggestion.proposal_id}`}
                  confirmLabel="Decline — they are different people"
                  subject={`#${suggestion.proposal_id}`}
                  variant="ghost"
                  disabled={decide.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() =>
                    decide.mutate({
                      proposalId: suggestion.proposal_id,
                      verdict: "reject",
                    })
                  }
                />
              </div>
            </div>
          </Row>
        ))}
      </Rows>
    </Queue>
  );
}
