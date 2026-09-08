import { useState } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  useCancelCouncil,
  useCouncil,
  useCouncils,
  useCreateCouncil,
  type CouncilSummary,
  type LeaderboardEntry,
  type SeatView,
} from "../data/council";
import {
  Button,
  ConfirmButton,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  StaleNote,
  StateBadge,
  Teach,
} from "../ui";
import "./council.css";

/**
 * Council — one component serving `/council` and `/council/$councilId`, the
 * `Projects` pattern: a list that is always on screen, with the detail added
 * below it once something is selected rather than replacing it.
 *
 * Three phases, always drawn in the same order regardless of how far a council
 * got: seats (phase 1 and 2 together, one card per seat) and the leaderboard
 * (phase 2's output) render whenever there are seats at all, and only the
 * synthesis panel (phase 3) changes shape when the chairman never wrote one —
 * a council whose chairman failed still has two phases worth of real answers
 * on it, and hiding them behind the one panel that failed would throw the rest
 * away.
 */
export function Council() {
  const params = useParams({ strict: false }) as { councilId?: string };
  const councilId = params.councilId ?? null;

  const councils = useCouncils();
  const rows = councils.data ?? [];
  const stale = councils.isError && councils.data !== undefined;

  return (
    <>
      <PageHeader title="Council" headline={headlineFor(rows, councils.data !== undefined)} />

      <ConveneForm />

      {stale && <StaleNote dataUpdatedAt={councils.dataUpdatedAt} />}
      {councils.isError && councils.data === undefined && <ListError error={councils.error} />}

      <CouncilList rows={rows} answered={councils.data !== undefined} selected={councilId} />

      {councilId === null && (
        <Teach title="Choose a council">
          <p>Pick a question from the list, or convene a new one above.</p>
        </Teach>
      )}

      {councilId !== null && <CouncilDetail key={councilId} id={councilId} />}
    </>
  );
}

/** One derived sentence about the whole list. */
function headlineFor(rows: CouncilSummary[], answered: boolean): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0) return "no council has been convened";
  const noun = rows.length === 1 ? "council" : "councils";
  const running = rows.filter((row) => row.status === "running").length;
  return running === 0
    ? `${rows.length} ${noun}, none deliberating`
    : `${rows.length} ${noun}, ${running} deliberating`;
}

function ListError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the councils</ErrorNote>;
}

/**
 * The daemon's own sentence, when it really sent one.
 *
 * `client.ts` falls back to `statusText` for a refusal with an empty body, so a
 * bare status arrives carrying only the status word — four words is the floor
 * between that and a sentence the daemon wrote on purpose, such as the budget
 * refusal naming the limit and the spend.
 */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

/* ------------------------------------------------------------- convene -- */

function ConveneForm() {
  const [question, setQuestion] = useState("");
  const create = useCreateCouncil();
  const navigate = useNavigate();

  return (
    <Panel title="Convene a council">
      <p className="council-note">
        One question, put to every seat in <code>.ai/council.yaml</code>. Each seat answers on its
        own, ranks the others blind, and a chairman writes a synthesis. This page does not offer a
        roster override — the roster lives in the file, and a per-question one is not built here.
      </p>
      <form
        className="council-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (question.trim() === "" || create.isPending) return;
          create.mutate(question.trim(), {
            onSuccess: (result) => {
              setQuestion("");
              void navigate({ to: `/council/${result.id}` });
            },
          });
        }}
      >
        <label className="council-field">
          <span>Question</span>
          <textarea
            rows={3}
            aria-label="Question"
            value={question}
            onChange={(event) => setQuestion(event.target.value)}
          />
        </label>
        <Button type="submit" intent="go" disabled={question.trim() === "" || create.isPending}>
          Convene
        </Button>
      </form>
      {create.isError && <ConveneRefusal error={create.error} />}
    </Panel>
  );
}

/**
 * Why convening was refused.
 *
 * The daemon's own sentence is let through for every code here, because
 * `post_council` refuses with prose rather than a name — a 503 says there is no
 * roster configured, a 429 names the budget limit and how much of it is spent,
 * and a 400 says what was wrong with the question. Generic copy for any of
 * those would throw away the one part of the refusal worth reading.
 */
function ConveneRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — no council was convened</ErrorNote>;
  return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
}

/* ------------------------------------------------------------------- list -- */

function CouncilList({
  rows,
  answered,
  selected,
}: {
  rows: CouncilSummary[];
  answered: boolean;
  selected: string | null;
}) {
  if (answered && rows.length === 0) return null;

  return (
    <Panel title="Councils" aside={<Count n={answered ? rows.length : undefined} />}>
      {!answered && <p className="council-loading">reading the councils…</p>}
      {rows.length > 0 && (
        <ul className="ui-rows council-list" aria-label="Councils">
          {rows.map((row) => (
            <CouncilRow key={row.id} row={row} active={row.id === selected} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function CouncilRow({ row, active }: { row: CouncilSummary; active: boolean }) {
  return (
    <li className={active ? "ui-rows-row council-row council-row-active" : "ui-rows-row council-row"}>
      <Link className="council-row-link" to={`/council/${row.id}`} aria-current={active ? "page" : undefined}>
        <span className="council-row-question">{row.question}</span>
        <StateBadge domain="council" state={row.status} />
        <span className="council-row-phase">phase {row.stage} of 3</span>
        <RelativeTime at={row.created_at} />
      </Link>
    </li>
  );
}

/* ----------------------------------------------------------------- detail -- */

function CouncilDetail({ id }: { id: string }) {
  const council = useCouncil(id);
  const cancel = useCancelCouncil();
  const detail = council.data;

  if (detail === undefined) {
    return (
      <Panel title="Council">
        {council.isError ? (
          <DetailError error={council.error} />
        ) : (
          <p className="council-loading">reading the council…</p>
        )}
      </Panel>
    );
  }

  return (
    <>
      <Panel
        title="This council"
        aside={
          detail.status === "running" ? (
            <ConfirmButton
              label="Cancel"
              confirmLabel="Cancel this council"
              variant="ghost"
              intent="stop"
              disabled={cancel.isPending}
              onConfirm={() => cancel.mutate(id)}
            />
          ) : undefined
        }
      >
        <p className="council-question">{detail.question}</p>
        <div className="council-facts">
          <StateBadge domain="council" state={detail.status} />
          <span className="council-phase">phase {detail.stage} of 3</span>
          <RelativeTime at={detail.created_at} />
        </div>
        {cancel.isError && <CancelRefusal error={cancel.error} />}
        {/* `false` is a success that changed nothing — the council had already
            ended, which is not an error and is not silence either. */}
        {cancel.data?.cancelled === false && (
          <p className="council-note" role="status">
            it had already ended.
          </p>
        )}
      </Panel>

      <SeatGrid seats={detail.seats} />
      <Leaderboard leaderboard={detail.leaderboard} />
      <Synthesis synthesis={detail.synthesis} error={detail.error} />
    </>
  );
}

function DetailError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{ not_found: "there is no council with that id", ...daemonProse(error) }}
      />
    );
  }
  return <ErrorNote>the núcleo did not answer — nothing is known about this council</ErrorNote>;
}

function CancelRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — this council was not cancelled</ErrorNote>;
}

/* ------------------------------------------------------------------- seats -- */

function SeatGrid({ seats }: { seats: SeatView[] }) {
  return (
    <Panel title="Seats" aside={<Count n={seats.length} />}>
      {seats.length === 0 ? (
        <p className="council-empty">no seat has been recorded for this council yet.</p>
      ) : (
        <ul className="ui-rows council-seats" aria-label="Seats">
          {seats.map((seat) => (
            <SeatCard key={seat.seat_idx} seat={seat} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

/** What this seat is called out loud, from what the wire actually names. */
function seatName(kind: string): string {
  const trimmed = kind.trim();
  return trimmed === "" ? "unnamed seat" : trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
}

/**
 * What a seat's answer reads as.
 *
 * `answer === null` is ambiguous on its own — nothing written yet, or a
 * transcript that has since been pruned — and the two must not read the same.
 * `stage1_status === "ok"` with no answer is the pruned case: the seat did
 * answer, and the text is simply gone now.
 */
function answerText(seat: SeatView): string {
  if (seat.answer !== null) return seat.answer;
  if (seat.stage1_status === "ok") return "answered — the text has expired";
  return "no answer recorded";
}

function SeatCard({ seat }: { seat: SeatView }) {
  const abstained = seat.stage2_status === "ok" && seat.rankings.length === 0;

  return (
    <li className="ui-rows-row council-seat">
      <div className="council-seat-head">
        <span className="council-seat-name">{seatName(seat.kind)}</span>
        <span className="council-seat-idx">seat {seat.seat_idx}</span>
      </div>
      <p className="council-seat-ref">{seat.ref}</p>

      <div className="council-seat-stage">
        <span className="council-seat-stage-label">stage 1</span>
        <StateBadge domain="council_seat" state={seat.stage1_status} />
      </div>
      {seat.stage1_error !== null && (
        <p className="council-seat-error" role="alert">
          {seat.stage1_error}
        </p>
      )}
      <p className="council-seat-answer">{answerText(seat)}</p>
      {seat.answer !== null && (
        <details className="council-seat-more">
          <summary className="ui-quiet">more</summary>
          <p>{seat.answer}</p>
        </details>
      )}

      <div className="council-seat-stage">
        <span className="council-seat-stage-label">stage 2</span>
        <StateBadge domain="council_seat" state={seat.stage2_status} />
      </div>
      {seat.stage2_error !== null && (
        <p className="council-seat-error" role="alert">
          {seat.stage2_error}
        </p>
      )}
      {/* A blank vote is a valid outcome, not a failure — the seat answered ok
          and simply ranked nobody. */}
      {abstained && <p className="council-seat-abstained">abstained</p>}
    </li>
  );
}

/* ------------------------------------------------------------- leaderboard -- */

function Leaderboard({ leaderboard }: { leaderboard: LeaderboardEntry[] }) {
  return (
    <Panel title="Leaderboard">
      {leaderboard.length === 0 ? (
        <p className="council-note">
          Fewer than two seats have a valid answer to rank, so there is nothing to show here —
          that does not stop the chairman from writing a synthesis.
        </p>
      ) : (
        <div role="list" aria-label="Leaderboard">
          <table className="council-leaderboard">
            <thead>
              <tr><th scope="col">Seat</th><th scope="col">Average rank</th><th scope="col">Votes</th></tr>
            </thead>
            <tbody>
              {leaderboard.map((entry) => (
                <tr key={entry.seat_idx}>
                  <th scope="row" className="council-leaderboard-seat">seat {entry.seat_idx}</th>
                  <td className="council-leaderboard-rank">{entry.avg_rank.toFixed(2)}</td>
                  <td className="council-leaderboard-n">{entry.n}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Panel>
  );
}

/* --------------------------------------------------------------- synthesis -- */

/**
 * Phase 3's output — or the fact that the chairman never produced one.
 *
 * A `null` synthesis with `error` set is not a reason to hide phases 1 and 2:
 * the seats and the leaderboard above this panel are real answers regardless
 * of what the chairman did with them, and only this one panel changes shape.
 */
function Synthesis({ synthesis, error }: { synthesis: string | null; error: string | null }) {
  if (synthesis !== null) {
    return (
      <Panel title="Synthesis">
        <p className="council-synthesis">{synthesis}</p>
      </Panel>
    );
  }
  if (error !== null) {
    return (
      <Panel title="Synthesis">
        <p className="council-chairman-failed" role="alert">
          the chairman failed to write a synthesis: {error}
        </p>
      </Panel>
    );
  }
  return (
    <Panel title="Synthesis">
      <p className="council-note">no synthesis yet.</p>
    </Panel>
  );
}

/* ---------------------------------------------------------------- shared -- */

function Count({ n }: { n: number | undefined }) {
  if (n === undefined) return null;
  return <span className="council-count">{n}</span>;
}
