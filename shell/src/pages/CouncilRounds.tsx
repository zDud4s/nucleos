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
import { Panel, Quiet, Tabs, TabsContent, TabsList, TabsTrigger } from "../ui";
import { CouncilRich } from "./CouncilRich";

/**
 * The council round by round: what each seat answered, then for every critique
 * round how it voted, what it said about each peer, and whether it changed its
 * mind.
 *
 * The seat grid above this answers "where is each seat now"; this answers "how
 * did it get there", which the grid cannot, because it shows only the answer
 * and the latest step. One tab per round, because a round is the unit the
 * council itself runs in and the unit a reader compares — reading every round
 * stacked would bury the second round under the first's critiques.
 *
 * Every anonymous label is turned back into a seat name before it is drawn.
 * The labels were the blinding the seats deliberated under; to the reader they
 * are a code to decode against `anon_map`, and decoding it is this page's job.
 */
export function CouncilRounds({ view }: { view: CouncilView }) {
  const rounds = Array.from(
    { length: view.rounds_run + 1 },
    (_, round) => round,
  );
  return (
    <Panel title="Rounds">
      {view.stopped_early && (
        /* Its own element, apart from the facts line in the header: the round it
           stopped at is the fact here, and the header only says that it did. */
        <p className="council-note council-stopped">
          Stopped early at round {view.rounds_run} — no seat changed its answer,
          so another round would have changed nothing.
        </p>
      )}
      <Tabs defaultValue="0">
        <TabsList aria-label="Rounds">
          {rounds.map((round) => (
            <TabsTrigger key={round} value={String(round)}>
              {round === 0 ? "Answers" : `Round ${round}`}
            </TabsTrigger>
          ))}
        </TabsList>
        {rounds.map((round) => (
          <TabsContent key={round} value={String(round)}>
            {round === 0 ? (
              <AnswersRound seats={view.seats} />
            ) : (
              <CritiqueRound round={round} view={view} />
            )}
          </TabsContent>
        ))}
      </Tabs>
      {view.leaderboard_by_round.length >= 2 && (
        <RankEvolution byRound={view.leaderboard_by_round} seats={view.seats} />
      )}
    </Panel>
  );
}

/* --------------------------------------------------------------- round 0 -- */

function AnswersRound({ seats }: { seats: SeatView[] }) {
  return (
    <div className="council-round">
      {seats.map((seat) => {
        const answer = seat.steps.find(
          (step) => step.round === 0 && step.phase === "answer",
        );
        return (
          <section className="council-round-seat" key={seat.seat_idx}>
            <h3 className="council-round-seat-name">{seatName(seat)}</h3>
            {answer?.answer != null ? (
              <CouncilRich text={answer.answer} />
            ) : (
              <Quiet says="no answer to show." />
            )}
          </section>
        );
      })}
    </div>
  );
}

/* -------------------------------------------------------- critique round -- */

function CritiqueRound({ round, view }: { round: number; view: CouncilView }) {
  /* A label resolved to the seat it stood for, by name. A label the map does
     not know is said as such rather than printed bare — a bare label is exactly
     the code this view exists to decode. */
  const nameOf = (seatIdx: number | null): string => {
    const seat =
      seatIdx === null
        ? undefined
        : view.seats.find((s) => s.seat_idx === seatIdx);
    return seat === undefined ? "an unrecorded seat" : seatName(seat);
  };
  const labelName = (label: string): string =>
    nameOf(deanonymise([label], view.anon_map)[0]);

  return (
    <div className="council-round">
      {view.seats.map((seat) => {
        const critique = seat.steps.find(
          (step) => step.round === round && step.phase === "critique",
        );
        const revise = seat.steps.find(
          (step) => step.round === round && step.phase === "revise",
        );
        if (critique === undefined && revise === undefined) return null;
        const name = seatName(seat);
        return (
          <section className="council-round-seat" key={seat.seat_idx}>
            <h3 className="council-round-seat-name">{name}</h3>
            {critique?.critique != null && (
              <>
                {critique.critique.ranking.length === 0 ? (
                  <p className="council-note">abstained — ranked nobody.</p>
                ) : (
                  <ol
                    className="council-ballot"
                    aria-label={`Ballot of ${name}`}
                  >
                    {deanonymise(critique.critique.ranking, view.anon_map).map(
                      (seatIdx, at) => (
                        <li key={at}>{nameOf(seatIdx)}</li>
                      ),
                    )}
                  </ol>
                )}
                {critique.critique.reviews.length > 0 && (
                  <ReviewsGiven
                    name={name}
                    reviews={critique.critique.reviews}
                    labelName={labelName}
                  />
                )}
              </>
            )}
            {critique !== undefined && critique.critique === null && (
              <p className="council-note">
                no readable critique for this round.
              </p>
            )}
            {revise !== undefined && (
              <Revision name={name} seat={seat} step={revise} />
            )}
          </section>
        );
      })}
    </div>
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
