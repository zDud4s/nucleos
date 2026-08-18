import { useEffect, useMemo, useRef, useState } from "react";
import {
  archiveChat, createChat, getAssistantChat, getAssistantTurn, getLocalModelAvailable,
  markChatSeen, patchChat, readIdeConversation, sendAssistantMessage, titleChatLocally,
  type ApiResult, type Brain, type ChatRow, type ConnectionState, type Said,
} from "./api";
import { runIsLive } from "./derive";
import BrainPicker from "./chat/BrainPicker";
import ChatList from "./chat/ChatList";
import IdeSessions from "./chat/IdeSessions";
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
  /**
   * Which conversation is open, owned by `App` for the same reason the transcripts are: leaving the
   * tab unmounts this page, and a selection kept here came back null — so you returned to a column
   * of names rather than to the conversation you were having.
   */
  selected: string | null;
  onSelect: (chatId: string | null) => void;
  /**
   * The conversations, owned by `App` — because the tab strip draws the waiting count while this
   * page is unmounted, which is exactly when that number matters.
   */
  chats: ChatRow[] | null;
  /** Reads the list again now, instead of at `App`'s next 3-second tick. */
  refreshChats: () => Promise<void>;
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
function Chats({
  token, connection, turnsByChat, setTurnsForChat, selected, onSelect, chats, refreshChats,
}: ChatsProps) {
  /** The conversations whose transcript has been read back at least once. */
  const [read, setRead] = useState<Set<string>>(new Set());
  /**
   * What was said in the conversation each picked-up chat came from, by chat.
   *
   * By chat and not one for the open one, so returning to a conversation draws its editor half
   * straight away instead of blanking it while the file is read again. Local to this page, unlike
   * the transcripts: nothing here is at risk of being lost on a tab switch — it is a file on disk
   * that no turn of ours changes, and re-reading it costs a read.
   */
  const [pickedUpByChat, setPickedUpByChat] = useState<Record<string, Said[] | null>>({});
  const [localAvailable, setLocalAvailable] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  /** Whether the picker of IDE conversations is open, taking the place of the open conversation. */
  const [picking, setPicking] = useState(false);

  const ready = connection === "connected" && token !== null;

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

  /**
   * Which conversation had in the editor the open one was picked up from, or null.
   *
   * Pulled out as a string so the read below depends on the FACT and not on the list carrying it:
   * `chats` is a new array every three seconds, and an effect watching it would re-read the whole
   * conversation on every tick of a list that had not changed.
   */
  const ideSessionId = chats?.find((chat) => chat.chat_id === selected)?.ide_session_id ?? null;

  // Reads the open conversation back, once per conversation opened: the turns out of the núcleo,
  // and — on one picked up from the editor — what was said in it before it was picked up.
  //
  // Both before the chat counts as read, so the "nothing said yet" below is never shown over a
  // conversation whose editor half is still arriving. Together rather than in two effects for that
  // reason alone: it is one answer to one question, and two effects would race to say it.
  useEffect(() => {
    if (!ready || token === null || selected === null) return;
    let cancelled = false;
    void (async () => {
      const [history, hadInTheEditor] = await Promise.all([
        getAssistantChat(token, selected),
        ideSessionId === null ? null : readIdeConversation(token, ideSessionId),
      ]);
      if (cancelled) return;
      if (history !== null) {
        setTurnsForChat(selected, (current) => merge(history.map(turnFromRow), current));
      }
      setPickedUpByChat((current) => ({ ...current, [selected]: hadInTheEditor }));
      setRead((current) => new Set(current).add(selected));
    })();
    return () => {
      cancelled = true;
    };
  }, [ideSessionId, ready, selected, setTurnsForChat, token]);

  /**
   * Records that the open conversation has been read, whenever it has anything unread.
   *
   * One rule covering both moments, rather than one call when you open a chat and another when a
   * turn lands in it: what matters is that the conversation IN FRONT OF YOU never claims to be
   * waiting. Written as a condition on the state rather than as two events, so a turn that lands
   * while you are looking at it is covered by the same line that covers opening a stale one.
   *
   * It terminates: marking sets the count to zero, the refresh brings the zero back, and the guard
   * below returns. A turn landing in between raises it again and the next pass clears that too.
   */
  useEffect(() => {
    if (!ready || token === null || selected === null) return;
    const current = chats?.find((chat) => chat.chat_id === selected);
    if (current === undefined || current.waiting === 0) return;
    let cancelled = false;
    void (async () => {
      await markChatSeen(token, selected);
      if (!cancelled) await refreshChats();
    })();
    return () => {
      cancelled = true;
    };
  }, [chats, ready, refreshChats, selected, token]);

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

  /**
   * Opens a conversation — a fresh one, or one continuing a session had in the IDE.
   *
   * One function for both, because from here they differ by a single argument and the daemon does
   * the rest. A second one would be the same four lines with a different failure message.
   */
  async function openChat(continueSession?: string) {
    if (token === null) return;
    setFailed(null);
    const created = await createChat(token, "cloud", continueSession);
    if (!created.ok) {
      setFailed(
        continueSession === undefined
          ? "The daemon did not open a conversation."
          : "That conversation is no longer on this machine — its folder may have been removed.",
      );
      return;
    }
    setPicking(false);
    await refreshChats();
    onSelect(created.value);
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
          // Both null for the same reason: the daemon has not answered yet, so nothing is known
          // about which model took it or which session it landed in. A guess here would draw a
          // "restarted" line under a turn that has not run.
          sessionId: null,
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
    if (chatId === selected) onSelect(null);
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
        onSelect={(chatId) => {
          setPicking(false);
          onSelect(chatId);
        }}
        onNew={() => void openChat()}
        onContinueFromIde={() => setPicking(true)}
        onArchive={(chatId) => void archive(chatId)}
      />
      <div className="chat-open">
        {picking && token !== null ? (
          <IdeSessions
            token={token}
            onContinue={(sessionId) => void openChat(sessionId)}
            onClose={() => setPicking(false)}
          />
        ) : current === null ? (
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
              {/*
                Where it runs, on the conversations that run somewhere. It is not decoration: this
                is what says the turns reach that directory's files rather than nothing at all.
              */}
              {current.cwd != null && (
                <span>
                  in <b>{current.cwd}</b>
                </span>
              )}
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
              <Transcript
                turns={turns}
                loaded={read.has(current.chat_id)}
                pickedUp={pickedUpByChat[current.chat_id] ?? null}
              />
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
