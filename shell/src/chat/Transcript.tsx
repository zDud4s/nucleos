import { useEffect, useRef } from "react";
import { formatUsd, runIsLive } from "../derive";
import { Teach } from "../ui";
import type { Said } from "../api";
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

/**
 * Whether the conversation started over here — a new context, with nothing above it in memory.
 *
 * The daemon refuses to resume a session once it has passed its context ceiling, or once anything in
 * it read third-party text, and the next turn then runs in a fresh one. That is a deliberate policy
 * and not a fault; what was wrong was that it happened in silence, so the transcript above and below
 * the line read as one unbroken conversation while the model had forgotten all of it.
 *
 * It bites hardest on the conversations picked up from the IDE. Those arrive carrying a context
 * somebody else's session already filled — often past the ceiling on the very first turn — so
 * continuing one can mean exactly one continued turn, and then this.
 *
 * A restart is only claimed when BOTH turns name a session. A null is not a new session, it is no
 * evidence, and drawing the line there would announce a restart on every turn from before the daemon
 * recorded one.
 */
function contextRestart(previous: Turn | undefined, turn: Turn): boolean {
  if (previous === undefined) return false;
  if (previous.sessionId === null || turn.sessionId === null) return false;
  return previous.sessionId !== turn.sessionId;
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
  /**
   * What was said in the conversation this one was picked up from, or null when there is no such
   * conversation — it was opened here, or its transcript is no longer on this machine.
   *
   * Read out of the editor's own file rather than out of the daemon, and drawn above the turns
   * because that is when it happened. Without it, picking up a conversation opened onto a blank
   * page: the model on the other side remembered every word of it and the person continuing it
   * could see none of them.
   */
  pickedUp: Said[] | null;
}

/** A conversation's turns, oldest first, with the model changes marked where they happened. */
function Transcript({ turns, loaded, pickedUp }: TranscriptProps) {
  const tail = useRef<HTMLDivElement | null>(null);
  const before = pickedUp ?? [];

  // Follow the conversation down as it grows, the way a chat is read.
  useEffect(() => {
    tail.current?.scrollIntoView({ block: "end" });
  }, [turns, pickedUp]);

  if (!loaded && turns.length === 0 && before.length === 0) {
    return <p className="a-note">Reading the conversation…</p>;
  }

  // Both halves empty, and not just the daemon's. A picked-up conversation has no turns of its own
  // until you answer in it, and saying "nothing said yet" over a page full of what you said in the
  // editor is the same wrong answer `loaded` exists to prevent.
  if (turns.length === 0 && before.length === 0) {
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
      {before.map((said, index) => (
        // Keyed by position: these came from a file, in the order they are in it, and nothing here
        // reorders or removes one. There is no id in a transcript to key by.
        <div className={said.by_owner ? "bubble asked" : "bubble said"} key={`ide-${index}`}>
          <span className="b-who">{said.by_owner ? "you" : "núcleo"}</span>
          {/* Text, never markup, on both sides — for the reason the turns below give. */}
          <pre className="b-text">{said.text}</pre>
        </div>
      ))}
      {before.length > 0 && (
        <p className="brain-cut">
          Picked up here. Everything above was said in the editor and is read back out of its own
          file — none of it was a run, and none of it was billed here.
        </p>
      )}
      {turns.map((turn, index) => {
        const previous = turns[index - 1];
        const changed = modelChange(previous, turn);
        // Only one line is drawn. A model change already says "no memory of what is above", which is
        // the same sentence this one would add — and two rules stacked read as two separate events.
        const restarted = changed === null && contextRestart(previous, turn);
        return (
          <div className="turn" key={turn.id}>
            {changed !== null && <p className="brain-cut">{changed}</p>}
            {restarted && (
              <p className="brain-cut">
                The conversation restarted here — this turn began a new context, with no memory of
                what is above.
              </p>
            )}
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
