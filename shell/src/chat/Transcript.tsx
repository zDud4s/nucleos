import { useEffect, useRef } from "react";
import { formatUsd, runIsLive } from "../derive";
import { Teach } from "../ui";
import type { Turn } from "./turns";

/**
 * What to say where a conversation changed model, or null where it did not.
 *
 * Deliberately asymmetric, because the two directions are not the same event. Going local, the
 * daemon replays the recent turns out of the runs table on every turn, so the conversation carries
 * on. Going to the cloud, the session the other model left is dropped — resuming it would hand the
 * cloud a context missing every turn answered on this machine — so it genuinely starts blank. One
 * sentence covering both would be tidier and false on one of them.
 *
 * Null on either side means nothing recorded which model answered, and no mark is drawn: a change
 * asserted against an unknown is a claim nobody made.
 */
function modelChange(previous: Turn | undefined, turn: Turn): string | null {
  if (previous === undefined) return null;
  if (previous.answeredBy === null || turn.answeredBy === null) return null;
  if (previous.answeredBy === turn.answeredBy) return null;
  return turn.answeredBy === "cloud"
    ? "Switched to the cloud model — it starts here with no memory of what is above."
    : "Switched to the local model — it re-reads the recent turns of this conversation.";
}

interface TranscriptProps {
  turns: Turn[];
  /**
   * Whether the conversation has been read back from the daemon yet.
   *
   * Kept apart from `turns.length === 0`, because an empty transcript that has not been loaded and
   * one that genuinely has no turns look identical — and telling someone "nothing said yet" while
   * their conversation is still arriving is the same wrong answer this page exists to prevent.
   */
  loaded: boolean;
}

/** A conversation's turns, oldest first, with the model changes marked where they happened. */
function Transcript({ turns, loaded }: TranscriptProps) {
  const tail = useRef<HTMLDivElement | null>(null);

  // Follow the conversation down as it grows, the way a chat is read.
  useEffect(() => {
    tail.current?.scrollIntoView({ block: "end" });
  }, [turns]);

  if (!loaded && turns.length === 0) {
    return <p className="a-note">Reading the conversation…</p>;
  }

  if (turns.length === 0) {
    return (
      <Teach title="Nothing said yet.">
        Each message is a run, so it is billed and appears in the run history like any other. The
        núcleo keeps the thread as well as the runs, so this conversation is here when you come back
        to it.
      </Teach>
    );
  }

  return (
    <div className="chat">
      {turns.map((turn, index) => {
        const changed = modelChange(turns[index - 1], turn);
        return (
          <div className="turn" key={turn.id}>
            {changed !== null && <p className="brain-cut">{changed}</p>}
            <div className="bubble asked">
              <span className="b-who">you</span>
              <p>{turn.asked}</p>
            </div>
            <div className={turn.failed ? "bubble said bad-turn" : "bubble said"}>
              <span className="b-who">
                núcleo
                <span className="b-run">#{turn.id}</span>
                {turn.answeredBy !== null && <span className="b-run">{turn.answeredBy}</span>}
                {turn.cost_usd !== null && <span className="b-run">{formatUsd(turn.cost_usd)}</span>}
              </span>
              {turn.answer === null ? (
                <p className="a-note">
                  {runIsLive(turn.status) ? "Working…" : "It answered with nothing."}
                </p>
              ) : (
                // Text, never markup: a model's output is not this shell's to execute.
                <pre className="b-text">{turn.answer}</pre>
              )}
            </div>
          </div>
        );
      })}
      <div ref={tail} />
    </div>
  );
}

export default Transcript;
