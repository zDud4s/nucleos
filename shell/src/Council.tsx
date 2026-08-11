import { useCallback, useEffect, useRef, useState } from "react";
import {
  cancelCouncil, createCouncil, getCouncil, listCouncils,
  type ConnectionState, type CouncilSummary, type CouncilView,
} from "./api";
import { relativeTime } from "./derive";
import { Badge, Button, ErrorNote, Panel, Teach } from "./ui";

/**
 * Ask several models the same thing, and see where they disagree.
 *
 * The tab is a window onto a record the daemon writes as it goes, not a live wire into it — the
 * deliberation runs whether this window is open or not, and closing it mid-council loses nothing.
 * That is why this polls rather than streams: the council's progress is state, and state is
 * something you can come back to.
 *
 * What it is NOT is a chat. There is one question, asked once, answered by everybody at once. A
 * follow-up is a new council, because a seat that could be argued with would be answering the
 * argument rather than the question.
 */

/** How long between reads while a council is still deliberating. */
const POLL_MS = 2000;

function StatusBadge({ status }: { status: string }) {
  if (status === "running") return <Badge tone="pending">deliberating</Badge>;
  if (status === "done") return <Badge tone="active">done</Badge>;
  if (status === "cancelled") return <Badge tone="paused">stopped</Badge>;
  return <Badge tone="off">failed</Badge>;
}

/**
 * What happened to one seat, said in words rather than left as an absence.
 *
 * A seat that failed and a seat still working both have no answer to show, and rendering them the
 * same way would make a broken model look like a slow one. The three endings are kept apart for the
 * same reason the daemon keeps them apart: a refusal, a wall clock and a cancellation say different
 * things about a model, and only one of them is worth trying again.
 */
function SeatEnding({ status, error }: { status: string; error: string | null }) {
  if (status === "ok") return null;
  const said =
    status === "pending"
      ? "Still working."
      : status === "timeout"
        ? "Ran out of time before it finished."
        : status === "cancelled"
          ? "Stopped."
          : status === "skipped"
            ? "Nothing to rank."
            : "Did not answer.";
  return (
    <p className="faint">
      {said}
      {error !== null && error.length > 0 && <> {error}</>}
    </p>
  );
}

interface CouncilProps {
  token: string | null;
  connection: ConnectionState;
}

export default function Council({ token, connection }: CouncilProps) {
  const [question, setQuestion] = useState("");
  const [past, setPast] = useState<CouncilSummary[] | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [open, setOpen] = useState<CouncilView | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);
  /**
   * Whether the open council is still moving — read by the poll, and held in a ref so that
   * scheduling the next read does not depend on the effect re-running. Without it a completed
   * council would keep being fetched every two seconds for as long as the tab stayed open.
   */
  const stillRunning = useRef(false);

  const refreshList = useCallback(async () => {
    if (token === null) return;
    setPast(await listCouncils(token));
  }, [token]);

  useEffect(() => {
    if (connection !== "connected") return;
    void refreshList();
  }, [connection, refreshList]);

  /**
   * Polls the open council while it is deliberating, and stops the moment it is not.
   *
   * The interval is cleared by the effect's own cleanup, so switching councils, closing the one on
   * screen, or unmounting the tab all stop the timer by the same path — there is no second place
   * that has to remember to.
   */
  useEffect(() => {
    if (token === null || openId === null) return;
    let cancelled = false;

    const read = async () => {
      const view = await getCouncil(token, openId);
      if (cancelled) return;
      setOpen(view);
      stillRunning.current = view !== null && view.status === "running";
      if (!stillRunning.current) {
        window.clearInterval(timer);
        // The list carries each council's status, so it is stale the moment this one settles.
        void refreshList();
      }
    };

    void read();
    const timer = window.setInterval(() => {
      if (stillRunning.current) void read();
    }, POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [token, openId, refreshList]);

  const ask = useCallback(async () => {
    if (token === null || question.trim() === "") return;
    setAsking(true);
    setNote(null);
    const result = await createCouncil(token, question.trim());
    setAsking(false);
    if (!result.ok) {
      setNote(
        result.status === 503
          ? "No council is configured. Write a roster to .ai/council.yaml and restart the daemon."
          : result.status === 429
            ? "The autonomy budget will not cover a council right now. It reopens with the window."
            : result.status === 400
              ? "The daemon would not take that question."
              : "Could not convene a council.",
      );
      return;
    }
    setQuestion("");
    setOpenId(result.value.id);
    await refreshList();
  }, [token, question, refreshList]);

  const stop = useCallback(async () => {
    if (token === null || openId === null) return;
    await cancelCouncil(token, openId);
    const view = await getCouncil(token, openId);
    setOpen(view);
    stillRunning.current = false;
    await refreshList();
  }, [token, openId, refreshList]);

  if (connection !== "connected" || token === null) {
    return <ErrorNote>The daemon is not reachable, so there is nothing to show.</ErrorNote>;
  }

  return (
    <>
      <Teach title="Ask a panel">
        One question, put to several models at once. They answer separately, then rank each other's
        answers blind — no seat sees its own, and none knows whose is whose — and a chairman writes
        the final answer from what survived. The ranking is what tells you whether they agreed.
      </Teach>

      <Panel title="Ask" aside="answered by every seat at once">
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void ask();
          }}
        >
          <input
            type="text"
            value={question}
            placeholder="What should the panel be asked?"
            aria-label="Question for the council"
            onChange={(event) => setQuestion(event.target.value)}
          />
          <Button type="submit" variant="approve" disabled={question.trim() === "" || asking}>
            {asking ? "Convening…" : "Convene"}
          </Button>
        </form>
        {note !== null && <p className="gate-note">{note}</p>}
      </Panel>

      {open !== null && (
        <Panel
          title={open.question}
          aside={<StatusBadge status={open.status} />}
        >
          <p className="faint">
            Convened {relativeTime(open.created_at)}. Chairman: {open.chairman_ref}.
            {open.status === "running" && <> Phase {open.stage} of 3.</>}
          </p>
          {open.error !== null && <ErrorNote>{open.error}</ErrorNote>}
          {open.status === "running" && <Button onClick={() => void stop()}>Stop</Button>}

          <h3>Seats</h3>
          <ul className="council-seats">
            {open.seats.map((seat) => (
              <li key={seat.seat_idx}>
                <span className="council-seat__model">
                  {seat.ref} <small>{seat.kind === "local" ? "on this machine" : "cloud"}</small>
                </span>
                <SeatEnding status={seat.stage1_status} error={seat.stage1_error} />
                {seat.answer !== null && <pre className="council-answer">{seat.answer}</pre>}
              </li>
            ))}
          </ul>

          <h3>Ranking</h3>
          {open.leaderboard.length === 0 ? (
            <p className="faint">
              {/* An empty table would suggest the ranking happened and found nothing. It did not
                  happen: below two answers there is nothing to compare. */}
              No ranking — that needs at least two seats to have answered.
            </p>
          ) : (
            <ol className="council-board">
              {open.leaderboard.map((entry) => (
                <li key={entry.seat_idx}>
                  {open.seats.find((seat) => seat.seat_idx === entry.seat_idx)?.ref ??
                    `seat ${entry.seat_idx}`}
                  {" — "}
                  <span className="faint">
                    average rank {entry.avg_rank.toFixed(2)} from {entry.n}{" "}
                    {entry.n === 1 ? "vote" : "votes"}
                  </span>
                </li>
              ))}
            </ol>
          )}

          <h3>The answer</h3>
          {open.synthesis === null ? (
            <p className="faint">
              {open.status === "running"
                ? "The chairman has not written it yet."
                : "The chairman produced nothing."}
            </p>
          ) : (
            <pre className="council-answer">{open.synthesis}</pre>
          )}
        </Panel>
      )}

      <Panel title="Before" aside={past === null ? undefined : `${past.length}`}>
        {past === null && <ErrorNote>Could not read past councils.</ErrorNote>}
        {past !== null && past.length === 0 && (
          <p className="faint">
            No councils yet. There is none to convene until .ai/council.yaml names a roster.
          </p>
        )}
        <ul className="council-list">
          {(past ?? []).map((summary) => (
            <li key={summary.id}>
              <button className="council-row" onClick={() => setOpenId(summary.id)}>
                <span className="council-row__question">{summary.question}</span>
                <span className="council-row__meta">{relativeTime(summary.created_at)}</span>
              </button>
              <StatusBadge status={summary.status} />
            </li>
          ))}
        </ul>
      </Panel>
    </>
  );
}
