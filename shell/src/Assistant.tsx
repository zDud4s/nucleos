import { useEffect, useRef, useState } from "react";
import {
  getAssistantTurn, sendAssistantMessage, SHELL_CHAT_ID,
  type ConnectionState, type RunDetail,
} from "./api";
import { formatUsd, runIsLive } from "./derive";
import { Button, ErrorNote, Panel, Teach } from "./ui";

/**
 * One exchange, as the shell remembers it.
 *
 * Held by `App` rather than by this component: switching tabs unmounts the page, and a transcript
 * kept in local state went with it — the message you had just sent was gone when you came back, and
 * so was the poll that would have collected its answer.
 */
export interface Turn {
  /** The daemon's turn id, which is also a run id — the assistant's turns ARE runs. */
  id: number;
  asked: string;
  /** null while the turn is still running. */
  answer: string | null;
  status: string;
  cost_usd: number | null;
  failed: boolean;
}

/**
 * How a finished turn's text is found.
 *
 * A turn is a run, so its reply arrives as the run's stdout, and a turn that failed leaves stderr
 * instead. Preferring stdout and falling back to stderr means a failure is shown rather than
 * rendered as an empty bubble, which reads as the assistant ignoring you.
 */
function replyText(detail: RunDetail): string | null {
  const out = detail.stdout?.trim();
  if (out !== undefined && out !== "") return out;
  const err = detail.stderr?.trim();
  if (err !== undefined && err !== "") return err;
  return null;
}

interface AssistantProps {
  token: string | null;
  connection: ConnectionState;
  /** Owned by `App`, so it outlives this page being unmounted by a tab switch. */
  turns: Turn[];
  setTurns: (update: (current: Turn[]) => Turn[]) => void;
}

/**
 * A conversation with the núcleo.
 *
 * The daemon holds ONE turn slot per chat, and this page speaks as its own chat (`SHELL_CHAT_ID`),
 * so it never takes the slot from the Telegram sidecar and the sidecar never takes it from here. A
 * 409 therefore means this chat is still mid-turn, which is why the composer stays disabled until
 * the turn lands rather than queueing a second one.
 *
 * The transcript lives in `App` and survives this page being unmounted, but it is still only in
 * memory: the daemon keeps the turns as runs and exposes no "list my chat" route, so a restart loses
 * the thread. Reconstructing it from `/runs?mode=assistant` is not an option either — that returns
 * every chat's turns, including the Telegram sidecar's, and nothing in a run row says which chat it
 * belonged to.
 */
function Assistant({ token, connection, turns, setTurns }: AssistantProps) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const tail = useRef<HTMLDivElement | null>(null);

  /**
   * The turn still in flight, DERIVED from the transcript rather than stored beside it.
   *
   * That is what makes the poll resume by itself after a tab switch: remounting recomputes this from
   * the turn that never settled, so the effect below starts again and collects an answer that landed
   * while this page did not exist. Held as its own state, it came back null and the turn sat on
   * "Working…" forever.
   */
  const pending = turns.find((turn) => turn.answer === null && runIsLive(turn.status))?.id ?? null;

  useEffect(() => {
    if (pending === null || token === null) return;
    let cancelled = false;
    const poll = async () => {
      const detail = await getAssistantTurn(token, pending);
      if (cancelled || detail === null) return;
      const settled = !runIsLive(detail.status);
      setTurns((current) =>
        current.map((turn) =>
          turn.id !== pending
            ? turn
            : {
                ...turn,
                status: detail.status,
                cost_usd: detail.cost_usd,
                answer: settled ? replyText(detail) : null,
                failed: settled && detail.status !== "completed",
              },
        ),
      );
      // Nothing to clear: `pending` is derived, so writing the settled status above is what ends it.
    };
    void poll();
    const id = setInterval(() => void poll(), 1500);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [pending, setTurns, token]);

  // Follow the conversation down as it grows, the way a chat is read.
  useEffect(() => {
    tail.current?.scrollIntoView({ block: "end" });
  }, [turns]);

  async function send() {
    if (token === null) return;
    const asked = text.trim();
    setSending(true);
    setFailed(null);
    const result = await sendAssistantMessage(token, SHELL_CHAT_ID, asked);
    setSending(false);
    if (!result.ok) {
      setFailed(
        result.status === 409
          ? "This chat is still working on the previous message. Wait for it to land."
          : result.status === 503
            ? "Refused: the kill switch is engaged."
            : "The daemon did not take the message.",
      );
      return;
    }
    setText("");
    // Appending the turn is all that is needed to start polling it: `pending` reads this turn back
    // out of the transcript on the next render.
    setTurns((current) => [
      ...current,
      { id: result.value, asked, answer: null, status: "running", cost_usd: null, failed: false },
    ]);
  }

  if (connection !== "connected" || token === null) {
    return (
      <section className="assistant">
        <Teach title="The assistant is waiting for the daemon.">
          Connect to the daemon to talk to the núcleo. A turn costs a run, so nothing is spent while
          it cannot be reached.
        </Teach>
      </section>
    );
  }

  const busy = pending !== null;

  return (
    <section className="assistant">
      <h1 className="headline">
        {busy
          ? <><em>Thinking…</em></>
          : turns.length === 0
            ? <>Ask the núcleo something.</>
            : <>{turns.length} turn{turns.length === 1 ? "" : "s"} this session.</>}
      </h1>
      <div className="statusline">
        <span>chat <b>{SHELL_CHAT_ID}</b></span>
        <span>one turn at a time · <b>a turn costs a run</b></span>
      </div>
      <Panel title="Conversation" aside={busy ? "working" : undefined}>
        {turns.length === 0 ? (
          <Teach title="Nothing said yet.">
            Each message is a run, so it is billed and appears in the run history like any other.
            This transcript lives only in this window — the núcleo keeps the runs, not the thread, so
            leaving the tab loses what is on screen.
          </Teach>
        ) : (
          <div className="chat">
            {turns.map((turn) => (
              <div className="turn" key={turn.id}>
                <div className="bubble asked">
                  <span className="b-who">you</span>
                  <p>{turn.asked}</p>
                </div>
                <div className={turn.failed ? "bubble said bad-turn" : "bubble said"}>
                  <span className="b-who">
                    núcleo
                    <span className="b-run">#{turn.id}</span>
                    {turn.cost_usd !== null && <span className="b-run">{formatUsd(turn.cost_usd)}</span>}
                  </span>
                  {turn.answer === null
                    ? <p className="a-note">{runIsLive(turn.status) ? "Working…" : "It answered with nothing."}</p>
                    // Text, never markup: a model's output is not this shell's to execute.
                    : <pre className="b-text">{turn.answer}</pre>}
                </div>
              </div>
            ))}
            <div ref={tail} />
          </div>
        )}
      </Panel>
      <form
        className="composer"
        onSubmit={(event) => {
          event.preventDefault();
          if (text.trim() === "" || sending || busy) return;
          void send();
        }}
      >
        <textarea
          rows={3}
          value={text}
          disabled={busy}
          placeholder={busy ? "Waiting for the current turn…" : "Ask the núcleo…"}
          onChange={(event) => setText(event.target.value)}
        />
        <Button type="submit" variant="approve" disabled={text.trim() === "" || sending || busy}>
          {sending ? "Sending…" : busy ? "Working…" : "Send"}
        </Button>
      </form>
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
    </section>
  );
}

export default Assistant;
