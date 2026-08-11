import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  archiveChat, createChat, getAssistantChat, getAssistantTurn, getLocalModelAvailable, listChats,
  patchChat, sendAssistantMessage, titleChatLocally,
  type ApiResult, type Brain, type ChatRow, type ConnectionState,
} from "./api";
import { runIsLive } from "./derive";
import BrainPicker from "./chat/BrainPicker";
import ChatList from "./chat/ChatList";
import Composer from "./chat/Composer";
import Transcript from "./chat/Transcript";
import { merge, replyText, turnFromRow, type Turn } from "./chat/turns";
import { Button, ErrorNote, Panel, Teach } from "./ui";

/** How often a turn in flight is asked whether it has landed. */
const POLL_MS = 1500;

interface ChatsProps {
  token: string | null;
  connection: ConnectionState;
  /**
   * Every open conversation's transcript, owned by `App`.
   *
   * Held above this page for the reason the single-conversation page held it above itself: leaving
   * the tab unmounts everything here, and a transcript in local state went with it — the message you
   * had just sent was gone when you came back, and so was the poll waiting for its answer.
   */
  turnsByChat: Record<string, Turn[]>;
  setTurnsForChat: (chatId: string, update: (current: Turn[]) => Turn[]) => void;
}

/**
 * The conversations with the núcleo.
 *
 * The list is the daemon's `chats` table, which is the app's own: a row is born from opening one
 * here, so the Telegram sidecar's conversations are absent without anything filtering them out.
 *
 * Every transcript is READ BACK from the daemon rather than remembered — a turn is a run, and the
 * run row records which chat it belonged to — which is what makes a conversation outlive a restart
 * and not merely a tab switch.
 */
function Chats({ token, connection, turnsByChat, setTurnsForChat }: ChatsProps) {
  const [chats, setChats] = useState<ChatRow[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  /** The conversations whose transcript has been read back at least once. */
  const [read, setRead] = useState<Set<string>>(new Set());
  const [localAvailable, setLocalAvailable] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const ready = connection === "connected" && token !== null;

  const refreshChats = useCallback(async () => {
    if (token === null) return;
    const listed = await listChats(token);
    // Only on success. A failed read leaves what is on screen alone rather than replacing the list
    // with an empty one, which would look exactly like every conversation having been lost.
    if (listed !== null) setChats(listed);
  }, [token]);

  useEffect(() => {
    if (!ready) return;
    void refreshChats();
  }, [ready, refreshChats]);

  useEffect(() => {
    if (!ready || token === null) return;
    let cancelled = false;
    void (async () => {
      const available = await getLocalModelAvailable(token);
      if (!cancelled) setLocalAvailable(available);
    })();
    return () => {
      cancelled = true;
    };
  }, [ready, token]);

  // Reads the open conversation back out of the núcleo, once per conversation opened.
  useEffect(() => {
    if (!ready || token === null || selected === null) return;
    let cancelled = false;
    void (async () => {
      const history = await getAssistantChat(token, selected);
      if (cancelled) return;
      if (history !== null) {
        setTurnsForChat(selected, (current) => merge(history.map(turnFromRow), current));
      }
      setRead((current) => new Set(current).add(selected));
    })();
    return () => {
      cancelled = true;
    };
  }, [ready, selected, setTurnsForChat, token]);

  /**
   * Every turn still in flight, across every conversation, DERIVED from the transcripts.
   *
   * Derived rather than stored for the reason the single-conversation page gave: remounting
   * recomputes it from the turns that never settled, so the poll starts again by itself and collects
   * an answer that landed while this page did not exist.
   *
   * Across every conversation and not only the open one, because the daemon holds one turn slot PER
   * CHAT: two can be thinking at once, and a poll that watched only the selected one would leave the
   * other marked "thinking…" for ever.
   */
  const pending = useMemo(
    () =>
      Object.entries(turnsByChat).flatMap(([chatId, turns]) =>
        turns
          .filter((turn) => turn.answer === null && runIsLive(turn.status))
          .map((turn) => ({ chatId, id: turn.id })),
      ),
    [turnsByChat],
  );
  // The effect below must not restart on every poll result, so it depends on WHICH turns are in
  // flight rather than on the transcripts they came from.
  const pendingKey = pending
    .map((turn) => turn.id)
    .sort((a, b) => a - b)
    .join(",");
  const pendingRef = useRef(pending);
  pendingRef.current = pending;

  useEffect(() => {
    if (token === null || pendingKey === "") return;
    let cancelled = false;
    const poll = async () => {
      for (const { chatId, id } of pendingRef.current) {
        const detail = await getAssistantTurn(token, id);
        if (cancelled || detail === null) continue;
        const settled = !runIsLive(detail.status);
        setTurnsForChat(chatId, (current) =>
          current.map((turn) =>
            turn.id !== id
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
        // The list carries the fallback title and the ordering, and both move when a turn lands.
        if (settled) void refreshChats();
      }
    };
    void poll();
    const timer = setInterval(() => void poll(), POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [pendingKey, refreshChats, setTurnsForChat, token]);

  const busy = useMemo(() => new Set(pending.map((turn) => turn.chatId)), [pending]);

  async function openChat() {
    if (token === null) return;
    setFailed(null);
    const created = await createChat(token, "cloud");
    if (!created.ok) {
      setFailed("The daemon did not open a conversation.");
      return;
    }
    await refreshChats();
    setSelected(created.value);
  }

  async function send(text: string): Promise<ApiResult<number>> {
    if (token === null || selected === null) {
      return { ok: false, fault: "unreachable", status: 0 };
    }
    const result = await sendAssistantMessage(token, selected, text);
    if (result.ok) {
      // Appending the turn is all that is needed to start polling it: `pending` reads it back out of
      // the transcript on the next render.
      setTurnsForChat(selected, (current) => [
        ...current,
        {
          id: result.value,
          asked: text,
          answer: null,
          status: "running",
          cost_usd: null,
          failed: false,
          answeredBy: null,
        },
      ]);
      void refreshChats();
    }
    return result;
  }

  async function changeBrain(brain: Brain) {
    if (token === null || selected === null) return;
    setFailed(null);
    const patched = await patchChat(token, selected, { brain });
    if (!patched.ok) {
      setFailed(
        patched.status === 409
          ? "This conversation is mid-turn. The model can change once it has landed."
          : "The daemon did not change the model.",
      );
      return;
    }
    await refreshChats();
  }

  async function name() {
    if (token === null || selected === null) return;
    setFailed(null);
    const named = await titleChatLocally(token, selected);
    if (!named.ok) {
      setFailed(
        named.status === 409
          ? "Nothing has been said in this conversation to name it after."
          : "The local model could not name this conversation.",
      );
      return;
    }
    await refreshChats();
  }

  async function archive(chatId: string) {
    if (token === null) return;
    setFailed(null);
    const archived = await archiveChat(token, chatId);
    if (!archived.ok) {
      setFailed("The daemon did not archive the conversation.");
      return;
    }
    if (chatId === selected) setSelected(null);
    await refreshChats();
  }

  if (!ready) {
    return (
      <section className="chats">
        <Teach title="The chats are waiting for the daemon.">
          Connect to the daemon to talk to the núcleo. A turn costs a run, so nothing is spent while
          it cannot be reached.
        </Teach>
      </section>
    );
  }

  const current = chats?.find((chat) => chat.chat_id === selected) ?? null;
  const turns = selected === null ? [] : (turnsByChat[selected] ?? []);
  const thinking = selected !== null && busy.has(selected);

  return (
    <section className="chats">
      <ChatList
        chats={chats ?? []}
        selected={selected}
        busy={busy}
        onSelect={setSelected}
        onNew={() => void openChat()}
        onArchive={(chatId) => void archive(chatId)}
      />
      <div className="chat-open">
        {current === null ? (
          <Teach title="Nothing is open.">
            Open a conversation to talk to the núcleo. Each message is a run, so it is billed and
            appears in the run history like any other — and the thread is kept by the daemon, so it
            is here when you come back to it.
          </Teach>
        ) : (
          <>
            <div className="statusline">
              <span>
                chat <b>{current.chat_id}</b>
              </span>
              <span>
                one turn at a time · <b>a turn costs a run</b>
              </span>
              {localAvailable && (
                <Button size="sm" onClick={() => void name()}>
                  Name it with the local model
                </Button>
              )}
            </div>
            <BrainPicker
              brain={current.brain}
              localAvailable={localAvailable}
              busy={thinking}
              onChange={(brain) => void changeBrain(brain)}
            />
            <Panel title="Conversation" aside={thinking ? "working" : undefined}>
              <Transcript turns={turns} loaded={read.has(current.chat_id)} />
            </Panel>
            <Composer busy={thinking} onSend={send} />
          </>
        )}
        {failed !== null && <ErrorNote>{failed}</ErrorNote>}
      </div>
    </section>
  );
}

export default Chats;
