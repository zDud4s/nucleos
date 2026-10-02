import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { lineDiff } from "../lib/line-diff";
import { keys } from "../data/keys";
import { RECORDED, useRunTail, type RunTailChunk } from "../data/runs";
import {
  deanonymise,
  seatName,
  type BordaRow,
  type CouncilView,
  type Review,
  type SeatView,
  type StepView,
} from "../data/council";
import {
  Button,
  Count,
  Panel,
  Quiet,
  StateBadge,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "../ui";
import { CouncilRich } from "./CouncilRich";

/**
 * The deliberation: one tab per round, and in each tab every seat side by side.
 *
 * One round across all seats is the comparison a reader makes — "how did they
 * differ here" — so a tab is a grid of seat columns rather than a seats x rounds
 * matrix, which at eight seats and four rounds is 32 cramped cells repeating
 * the same identity header in each. The tabs keep the round axis without
 * drawing it 32 times, and each answer exists only in the Answers tab, so it is
 * never printed twice. The panel opens on the last round with a recorded step:
 * where the council ended, or where it is now, is what a reader comes for.
 *
 * Every anonymous label is turned back into a seat name before it is drawn.
 * The labels were the blinding the seats deliberated under; to the reader they
 * are a code to decode against `anon_map`, and decoding it is this page's job.
 */
export function CouncilDeliberation({ view, running }: { view: CouncilView; running: boolean }) {
  // `rounds_run` counts finished rounds; a step already recorded in the next
  // one (running, or failed there) still earns that round its tab.
  const lastRound = Math.max(
    view.rounds_run,
    ...view.seats.flatMap((seat) => seat.steps.map((step) => step.round)),
  );
  const rounds = Array.from({ length: lastRound + 1 }, (_, round) => round);
  return (
    <Panel title="Deliberation" aside={<Count n={view.seats.length} />}>
      {view.seats.length === 0 ? (
        <Quiet says="no seat has been recorded for this council yet." />
      ) : (
        <Tabs defaultValue={String(lastRound)}>
          <TabsList aria-label="Rounds">
            {rounds.map((round) => (
              <TabsTrigger key={round} value={String(round)}>
                {round === 0 ? "Answers" : `Round ${round}`}
              </TabsTrigger>
            ))}
          </TabsList>
          {rounds.map((round) => (
            <TabsContent key={round} value={String(round)}>
              <ul className="council-seats" aria-label="Seats">
                {view.seats.map((seat) => (
                  <SeatColumn key={seat.seat_idx} seat={seat} round={round} view={view} running={running} />
                ))}
              </ul>
            </TabsContent>
          ))}
        </Tabs>
      )}
      {view.leaderboard_by_round.length >= 2 && (
        <RankEvolution byRound={view.leaderboard_by_round} seats={view.seats} />
      )}
      {view.stopped_early && (
        /* Its own element, apart from the facts line in the header: the round it
           stopped at is the fact here, and the header only says that it did. */
        <p className="council-note council-stopped">
          Stopped early at round {view.rounds_run} — no seat changed its answer,
          so another round would have changed nothing.
        </p>
      )}
    </Panel>
  );
}

/* ------------------------------------------------------------ seat column -- */

/**
 * What this seat is called out loud.
 *
 * An agent's name wins the title, and the model stays underneath it in
 * `.council-seat-ref`: *who* answered and *what* ran are different facts, and
 * the second is the one you reach for when the answer is bad. A seat the
 * roster named by model has no name of its own, so its kind is still the title
 * — that case is unchanged and is not the lesser one.
 *
 * The agent's id stands in when the name is gone. `agent_name` is read from
 * the catalogue as the view is built, so a `null` beside a set `agent_id`
 * means the agent has been deleted since it answered — a real state the seat
 * says out loud rather than papering over with a blank line. (`seatName`, used
 * away from the column header, falls back to the model instead: there the
 * header is not beside it to say which agent is gone.)
 */
function seatTitle(seat: SeatView): string {
  if (seat.agent_name !== null) return seat.agent_name;
  if (seat.agent_id !== null) return seat.agent_id;
  const trimmed = seat.kind.trim();
  return trimmed === "" ? "unnamed seat" : trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
}

/** What a step is called in a column: "answer", or "round 1 · critique". */
function stepLabel(step: StepView): string {
  return step.round === 0 && step.phase === "answer" ? "answer" : `round ${step.round} · ${step.phase}`;
}

/**
 * A critique that settled and ranked nobody. A blank vote is a real answer to a
 * critique round, not a failure and not a gap — the seat chose not to rank.
 * A critique with no readable payload is not this: the daemon marks that one
 * `invalid`, and it reads as such.
 */
function abstained(step: StepView): boolean {
  return step.phase === "critique" && step.status === "ok" && (step.critique?.ranking.length ?? 0) === 0;
}

/**
 * One seat in one round: who it is, then what it did in that round. The
 * identity header repeats in every tab on purpose — a column read without its
 * name is a column nobody can attribute.
 */
function SeatColumn({
  seat,
  round,
  view,
  running,
}: {
  seat: SeatView;
  round: number;
  view: CouncilView;
  running: boolean;
}) {
  const steps = seat.steps.filter((step) => step.round === round);
  const tailing = steps.find((step) => step.status === "pending" && step.run_id !== null);
  // An agent that answered and is no longer in the catalogue. Told apart from a
  // model-named seat by `agent_id`, which the row keeps forever.
  const agentIsGone = seat.agent_id !== null && seat.agent_name === null;

  return (
    <li className="council-seat">
      <span className={seat.agent_id === null ? "council-seat-name" : "council-seat-name council-seat-agent"}>
        {seatTitle(seat)}
      </span>
      <p className="council-seat-ref">{seat.ref}</p>
      {/* Against the model line, which `.council-seat-gone` pulls it up to: it
          explains the id standing in as the title, not the role below. */}
      {agentIsGone && <p className="council-seat-gone">this agent is no longer in the catalogue</p>}
      {seat.role !== null && <p className="council-seat-role">plays the {seat.role.replace(/_/g, " ")}</p>}
      <span className="council-seat-idx">{`Seat ${seat.seat_idx + 1}`}</span>

      {round === 0 ? (
        <AnswerCell seat={seat} />
      ) : steps.length === 0 ? (
        <p className="council-note">nothing recorded in this round.</p>
      ) : (
        <CritiqueCell seat={seat} round={round} view={view} />
      )}

      {/* What the seat is writing right now. Only while the step is pending and
          the council still runs: a settled step has its stored result above,
          and a council that ended will never write another byte to any tail. */}
      {running && tailing !== undefined && tailing.run_id !== null && (
        <StepTail key={tailing.run_id} runId={tailing.run_id} name={seatName(seat)} />
      )}
    </li>
  );
}

/** One step's label, its badge, and the daemon's sentence when it did not go well. */
function StepHead({ step }: { step: StepView }) {
  return (
    <>
      <div className="council-seat-stage">
        <span className="council-seat-stage-label">{stepLabel(step)}</span>
        <StateBadge domain="council_seat" state={step.status} />
      </div>
      {step.error !== null && (
        <p className="council-seat-error" role="alert">
          {step.error}
        </p>
      )}
    </>
  );
}

/* --------------------------------------------------------------- round 0 -- */

/**
 * The seat's round-0 answer — the text every ranking was cast over, and the
 * one place on the page it is printed.
 */
function AnswerCell({ seat }: { seat: SeatView }) {
  const [full, setFull] = useState(false);
  const answer = seat.steps.find((step) => step.round === 0 && step.phase === "answer");
  if (answer === undefined) return <p className="council-seat-answer">no answer recorded</p>;
  return (
    <>
      <StepHead step={answer} />
      {answer.answer !== null ? (
        <>
          <div className={full ? "council-seat-answer" : "council-seat-answer council-seat-answer-clamped"}>
            <CouncilRich text={answer.answer} />
          </div>
          {/* The clamp is a few lines, and a seat's answer is routinely longer.
              The control unclamps this same block rather than printing a second
              copy of it underneath. Named after the seat, because every column
              has one and "more" alone does not say whose. */}
          <Button
            variant="quiet"
            aria-expanded={full}
            aria-label={`${full ? "less" : "more"} of ${seatName(seat)}'s answer`}
            onClick={() => setFull(!full)}
          >
            {full ? "less" : "more"}
          </Button>
        </>
      ) : (
        // `ok` with no text is the pruned case: the seat did answer, and the
        // transcript that held it is simply gone now.
        <p className="council-seat-answer">
          {answer.status === "ok" ? "answered — the text has expired" : "no answer recorded"}
        </p>
      )}
    </>
  );
}

/* -------------------------------------------------------- critique round -- */

function CritiqueCell({ seat, round, view }: { seat: SeatView; round: number; view: CouncilView }) {
  /* A label resolved to the seat it stood for, by name. A label the map does
     not know is said as such rather than printed bare — a bare label is exactly
     the code this view exists to decode. */
  const nameOf = (seatIdx: number | null): string => {
    const found = seatIdx === null ? undefined : view.seats.find((s) => s.seat_idx === seatIdx);
    return found === undefined ? "an unrecorded seat" : seatName(found);
  };
  const labelName = (label: string): string => nameOf(deanonymise([label], view.anon_map)[0]);

  const critique = seat.steps.find((step) => step.round === round && step.phase === "critique");
  const revise = seat.steps.find((step) => step.round === round && step.phase === "revise");
  const name = seatName(seat);
  return (
    <>
      {critique !== undefined && (
        <>
          <StepHead step={critique} />
          {abstained(critique) && <p className="council-seat-abstained">abstained</p>}
          {critique.critique != null && critique.critique.ranking.length > 0 && (
            <ol className="council-ballot" aria-label={`Ballot of ${name}`}>
              {deanonymise(critique.critique.ranking, view.anon_map).map((seatIdx, at) => (
                <li key={at}>{nameOf(seatIdx)}</li>
              ))}
            </ol>
          )}
          {critique.critique != null && critique.critique.reviews.length > 0 && (
            <ReviewsGiven name={name} reviews={critique.critique.reviews} labelName={labelName} />
          )}
          {/* Settled without a payload and without the daemon's own sentence
              for why: said, rather than left as a blank column. */}
          {critique.critique === null && critique.status === "ok" && (
            <p className="council-note">no readable critique for this round.</p>
          )}
        </>
      )}
      {revise !== undefined && (
        <>
          <StepHead step={revise} />
          <Revision name={name} seat={seat} step={revise} />
        </>
      )}
    </>
  );
}

function ReviewsGiven({
  name,
  reviews,
  labelName,
}: {
  name: string;
  reviews: Review[];
  labelName: (label: string) => string;
}) {
  return (
    <div
      className="council-critiques"
      role="group"
      aria-label={`Critiques by ${name}`}
    >
      {reviews.map((review, at) => {
        const reviewed = labelName(review.label);
        return (
          <div
            className="council-critique"
            role="group"
            aria-label={`On ${reviewed}'s answer`}
            key={`${review.label}-${at}`}
          >
            <p className="council-critique-on">On {reviewed}&apos;s answer</p>
            <ul className="council-points">
              {review.points.map((point, index) => (
                <li className="council-point" key={index}>
                  <span className="council-point-stance">{point.stance}</span>{" "}
                  <span>{point.claim}</span>
                  <span className="council-point-why"> — {point.why}</span>
                </li>
              ))}
            </ul>
          </div>
        );
      })}
    </div>
  );
}

/**
 * The answer a revision is measured against: the round-0 answer, replaced by
 * every earlier revision that changed it. A revision that kept its answer
 * leaves the previous text standing, which is what "kept" means.
 */
function previousAnswer(seat: SeatView, step: StepView): string {
  let current = "";
  for (const candidate of seat.steps) {
    if (candidate === step) break;
    if (candidate.phase === "answer" && candidate.answer !== null)
      current = candidate.answer;
    if (
      candidate.phase === "revise" &&
      candidate.changed === true &&
      candidate.answer !== null
    ) {
      current = candidate.answer;
    }
  }
  return current;
}

function Revision({
  name,
  seat,
  step,
}: {
  name: string;
  seat: SeatView;
  step: StepView;
}) {
  const changed = step.changed === true;
  return (
    <div
      className="council-revision"
      role="group"
      aria-label={`Revision by ${name}`}
    >
      <p className="council-revision-verdict">
        {changed
          ? "changed its answer"
          : step.changed === false
            ? "kept its answer"
            : "revision not readable"}
        {step.why !== null && step.why !== "" ? `: ${step.why}` : ""}
      </p>
      {changed && step.answer !== null && (
        <ol className="council-diff" aria-label="Diff">
          {lineDiff(previousAnswer(seat, step), step.answer).map((line, at) => (
            <li className="council-diff-line" data-kind={line.kind} key={at}>
              {/* The sign carries the meaning, so it survives without the tint. */}
              <span className="council-diff-sign" aria-hidden="true">
                {line.kind === "added"
                  ? "+"
                  : line.kind === "removed"
                    ? "-"
                    : " "}
              </span>
              {line.kind === "added" ? (
                <ins>{line.text}</ins>
              ) : line.kind === "removed" ? (
                <del>{line.text}</del>
              ) : (
                <span>{line.text}</span>
              )}
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

/* --------------------------------------------------------- rank evolution -- */

/**
 * Where each seat stood on every round's Borda leaderboard, 1 being best.
 *
 * Drawn only from the second vote on: one round is the leaderboard panel
 * already, and a one-column "evolution" is a table that evolves nothing. A seat
 * missing from a round's leaderboard reads as a dash, not as last place — it was
 * not ranked, which is a different fact.
 */
function RankEvolution({
  byRound,
  seats,
}: {
  byRound: BordaRow[][];
  seats: SeatView[];
}) {
  return (
    <table className="council-evolution">
      <caption className="council-evolution-caption">Rank evolution</caption>
      <thead>
        <tr>
          <th scope="col"></th>
          {byRound.map((_, at) => (
            <th scope="col" key={at}>{`Round ${at + 1}`}</th>
          ))}
        </tr>
      </thead>
      <tbody>
        {seats.map((seat) => (
          <tr key={seat.seat_idx}>
            <th scope="row">{seatName(seat)}</th>
            {byRound.map((board, at) => {
              const position = board.findIndex(
                (row) => row.seat_idx === seat.seat_idx,
              );
              return (
                <td key={at}>{position === -1 ? "–" : String(position + 1)}</td>
              );
            })}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/* -------------------------------------------------------------- live tail -- */

/**
 * What a seat's running step has written so far, read off its run's live tail.
 *
 * Mounted only while the step is pending and the council runs — the caller
 * decides that, so a settled step or an ended council never asks for a tail at
 * all. It reads exactly as `RunDetail.tsx::RunTail` does: each chunk is taken
 * once, by object identity (react-query hands back the same object for a
 * deep-equal refetch, and a StrictMode remount would otherwise append a chunk
 * twice), and the next read starts at `chunk.next`, the daemon's own BYTE
 * offset. Counting the text here instead would drift on the first non-ASCII
 * byte, since JavaScript counts UTF-16 units.
 *
 * On unmount the tail's cache entry is dropped. A settled step's stored result
 * is the truth from then on, and an inactive query with data is still
 * refetched by anything that refetches by key — reading a tail nobody shows.
 * It also keeps a later mount from starting at offset 0 with a stale chunk
 * already in the cache, which it would take as the whole of the output.
 */
export function StepTail({ runId, name }: { runId: number; name: string }) {
  const queryClient = useQueryClient();
  const [since, setSince] = useState(0);
  const [text, setText] = useState("");
  const tail = useRunTail(runId, since, true);
  const chunk = tail.data;

  const consumed = useRef<RunTailChunk | null>(null);
  useEffect(() => {
    if (chunk === undefined || chunk === RECORDED) return;
    if (consumed.current === chunk) return;
    consumed.current = chunk;
    if (chunk.text === "") return;
    setText((current) => current + chunk.text);
    setSince(chunk.next);
  }, [chunk]);

  useEffect(
    () => () => {
      const queryKey = keys.runs.tail(runId);
      const query = queryClient.getQueryCache().find({ queryKey, exact: true });
      // Only when nobody else is reading the same run — another screen's
      // observer keeps its own tail.
      if (query !== undefined && query.getObserversCount() === 0) {
        void queryClient.cancelQueries({ queryKey, exact: true });
        queryClient.removeQueries({ queryKey, exact: true });
      }
    },
    [queryClient, runId],
  );

  return (
    <pre className="council-seat-tail" role="log" aria-label={`Live tail of ${name}`}>
      {text}
    </pre>
  );
}
