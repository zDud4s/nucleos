import type { ReactNode } from "react";
import { agreementText, seatName, type CouncilView, type Synthesis } from "../data/council";
import { Panel, Quiet } from "../ui";
import { CouncilRich } from "./CouncilRich";

/**
 * The chairman's synthesis — or the fact that the chairman never produced one.
 *
 * Three shapes, decided by what the daemon could read out of the chairman:
 *  - structured: the answer, then what the seats agreed on, where they split
 *    (by seat name), the minority view and what is still open — each card only
 *    when it has something to say;
 *  - degraded: the chairman wrote text the daemon could not read as its
 *    structure. The text is shown as written, with a warning, and none of the
 *    cards or badges — inventing a consensus out of prose would be the page
 *    claiming something the chairman never said;
 *  - an old council (written before the structure existed): its text, plainly,
 *    with no warning — nothing went wrong with it.
 *
 * A `null` synthesis with `error` set is not a reason to hide the rounds: the
 * seats and the leaderboard above this panel are real answers regardless of
 * what the chairman did with them, and only this one panel changes shape.
 * Every text is markdown, drawn through `CouncilRich`, never as HTML.
 *
 * The verdict line is plain spans rather than `Badge`: a tone is a state-map
 * decision (`ui/badge-authorship.test.ts`), and neither the chairman's
 * confidence nor the ballots' agreement is a lifecycle state of anything. The
 * chairman's reason for its confidence is printed, not hidden in a `title`.
 */
export function CouncilSynthesis({ view }: { view: CouncilView }) {
  const structured = view.synthesis_structured;
  if (structured !== null && view.synthesis_status !== "degraded") {
    return (
      <Panel title="Synthesis">
        <StructuredSynthesis synthesis={structured} view={view} />
      </Panel>
    );
  }
  if (view.synthesis !== null) {
    return (
      <Panel title="Synthesis">
        {view.synthesis_status === "degraded" && (
          <p className="council-synthesis-degraded" role="note">
            unstructured synthesis — the chairman&apos;s answer could not be read as consensus,
            disagreements and open questions, so it is shown as written.
          </p>
        )}
        <div className="council-synthesis">
          <CouncilRich text={view.synthesis} />
        </div>
      </Panel>
    );
  }
  if (view.error !== null) {
    return (
      <Panel title="Synthesis">
        <p className="council-chairman-failed" role="alert">
          the chairman failed to write a synthesis: {view.error}
        </p>
      </Panel>
    );
  }
  return (
    <Panel title="Synthesis">
      <Quiet says="no synthesis yet." />
    </Panel>
  );
}

function StructuredSynthesis({ synthesis, view }: { synthesis: Synthesis; view: CouncilView }) {
  /* A position's seats by name. An index the view does not carry is the
     daemon's inconsistency, said as such rather than printed as a number. */
  const nameOf = (seatIdx: number): string => {
    const seat = view.seats.find((candidate) => candidate.seat_idx === seatIdx);
    return seat === undefined ? `an unrecorded seat (#${seatIdx})` : seatName(seat);
  };
  const agreement = view.agreement;

  return (
    <>
      <div className="council-synthesis">
        <CouncilRich text={synthesis.answer} />
      </div>
      {/* The verdict after the answer it qualifies: how sure the chairman is
          and why, then how far the ballots agreed and how many there were. */}
      <div className="council-synthesis-verdict">
        <span className="council-synthesis-confidence">{`${synthesis.confidence.level} confidence`}</span>
        <p className="council-synthesis-why">{synthesis.confidence.why}</p>
        {agreement !== null && (
          <span className="council-synthesis-agreement">
            {`${agreementText(agreement)} · ${agreement.ballots} ${agreement.ballots === 1 ? "ballot" : "ballots"}`}
          </span>
        )}
      </div>

      {synthesis.consensus.length > 0 && (
        <SynthesisCard name="Consensus">
          <ul className="council-synthesis-list">
            {synthesis.consensus.map((point, at) => (
              <li key={at}>{point}</li>
            ))}
          </ul>
        </SynthesisCard>
      )}

      {synthesis.disagreements.length > 0 && (
        <SynthesisCard name="Disagreements">
          {synthesis.disagreements.map((disagreement, at) => (
            <div className="council-synthesis-topic" key={at}>
              <p className="council-synthesis-topic-name">{disagreement.topic}</p>
              <ul className="council-synthesis-list">
                {disagreement.positions.map((position, index) => (
                  <li key={index}>
                    <span className="council-synthesis-seats">
                      {position.seats.map(nameOf).join(", ")}
                    </span>
                    {": "}
                    {position.view}
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </SynthesisCard>
      )}

      {synthesis.minority !== null && synthesis.minority.trim() !== "" && (
        <SynthesisCard name="Minority">
          <p className="council-synthesis-text">{synthesis.minority}</p>
        </SynthesisCard>
      )}

      {synthesis.open_questions.length > 0 && (
        <SynthesisCard name="Open questions">
          <ul className="council-synthesis-list">
            {synthesis.open_questions.map((question, at) => (
              <li key={at}>{question}</li>
            ))}
          </ul>
        </SynthesisCard>
      )}
    </>
  );
}

function SynthesisCard({ name, children }: { name: string; children: ReactNode }) {
  return (
    <div className="council-synthesis-card ui-panel-inset" role="group" aria-label={name}>
      <h3 className="council-synthesis-card-name">{name}</h3>
      {children}
    </div>
  );
}
