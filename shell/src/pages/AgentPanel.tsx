// §spec pilar-de-browser
import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useBrowserSessions } from "../data/browser";
import {
  chatForSession,
  createBrowserTask,
  relinkBrowserChat,
  unlinkBrowserChat,
  useBrowserChatLink,
  useLastBrowserChat,
} from "../data/browser-agent-chat";
import { useActiveTab } from "../data/browser-store";
import { NO_PROJECT } from "../data/browser-tabs";
import { useChats, useChatTranscript, useSendMessage } from "../data/chats";
import { keys } from "../data/keys";
import { ComposerBox } from "../chats/ComposerBox";
import { MessageList, type ListedMessage } from "../chats/MessageList";
import type { Turn } from "../lib/turns";
import { Button, ErrorNote, Select } from "../ui";
import "./agent-panel.css";

const PLACEHOLDER = "Message the agent…";

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** A transcript as the plain run of messages `MessageList` draws. */
function listed(turns: readonly Turn[] | undefined): ListedMessage[] {
  const out: ListedMessage[] = [];
  for (const turn of turns ?? []) {
    out.push({ key: `${turn.id}-asked`, role: "person", text: turn.asked });
    if (turn.answer !== null && turn.answer !== "") {
      out.push({ key: `${turn.id}-answer`, role: "agent", text: turn.answer });
    }
  }
  return out;
}

/**
 * The Agent panel of the Web page: the conversation behind the tab in front (read from the
 * browser store's `activeTab`, never from BrowserPage state), its link to a mother conversation,
 * and a composer. Takes no props: everything it shows comes from the store and the data hooks.
 *
 * - An agent tab shows the conversation its session answers to; with none, it says so and offers
 *   no composer (there is nobody to talk to).
 * - An owner tab, or no tab, shows the newest browser conversation of the scope, or an empty
 *   state whose "New task" starts a conversation linked to the panel only.
 */
export function AgentPanel() {
  const active = useActiveTab();
  const scope = active?.scope ?? NO_PROJECT;
  const tab = active?.tab ?? null;
  const tabKey = `${scope}|${tab?.id ?? ""}`;
  const agentTab = tab !== null && tab.role === "agent";

  // `chat_id` is optional on a session row: absent reads as "no conversation".
  const sessionRows = useBrowserSessions().data?.map((row) => ({
    id: row.id,
    chat_id: (row as { chat_id?: string | null }).chat_id ?? null,
  }));
  const last = useLastBrowserChat(scope).data ?? null;
  const chats = useChats().data;

  // The conversation a "New task" started, kept only while the same tab stays in front.
  const [created, setCreated] = useState<{ key: string; chatId: string } | null>(null);
  const createdChat = created !== null && created.key === tabKey ? created.chatId : null;

  const chatId = agentTab
    ? chatForSession(sessionRows, tab.sessionId ?? "")
    : (createdChat ?? last?.chat_id ?? null);

  const transcript = useChatTranscript(chatId).data;
  const send = useSendMessage(chatId ?? "");
  const linkState = useBrowserChatLink(chatId).data;

  const queryClient = useQueryClient();
  // This file's reads (link, last) and the chat list. The root is spelled out rather than
  // imported: the panel's tests mock `browser-agent-chat` whole.
  const refreshLinks = () => {
    void queryClient.invalidateQueries({ queryKey: ["browser-agent"] });
    void queryClient.invalidateQueries({ queryKey: keys.chats.all });
  };

  const [text, setText] = useState("");
  const [drafting, setDrafting] = useState<string | null>(null);
  const [relinking, setRelinking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const boxRef = useRef<HTMLTextAreaElement | null>(null);

  // What the panel shows: the draft, the relink picker and the error belong to one tab and one
  // conversation, so a change of either starts them afresh (adjusted during render, not in an
  // effect, so the old draft never paints under the new conversation).
  const viewKey = `${tabKey}|${chatId ?? ""}`;
  const [shownKey, setShownKey] = useState(viewKey);
  if (shownKey !== viewKey) {
    setShownKey(viewKey);
    setText("");
    setRelinking(false);
    setError(null);
  }
  // The same key for answers that land later: a write that settles after the view moved on
  // must not drop its text or its error into another conversation.
  const viewRef = useRef(viewKey);
  useEffect(() => {
    viewRef.current = viewKey;
  }, [viewKey]);

  const titleOf = (id: string): string | null =>
    chats?.find((chat) => chat.chat_id === id)?.title ?? null;

  // An agent tab whose session the rows do not carry yet (still loading, or opened since the
  // last poll): not knowing is not the same as "no conversation".
  const tabSession = agentTab ? (tab.sessionId ?? null) : null;
  const sessionPending =
    agentTab &&
    (tabSession === null ||
      sessionRows === undefined ||
      !sessionRows.some((row) => row.id === Number(tabSession)));
  if (sessionPending) {
    return (
      <section className="agent-panel" aria-label="Agent">
        <p className="agent-panel-note">Loading the conversation of this tab…</p>
      </section>
    );
  }

  if (agentTab && chatId === null) {
    return (
      <section className="agent-panel" aria-label="Agent">
        <p className="agent-panel-note">This tab belongs to a task without a conversation.</p>
      </section>
    );
  }

  const newTaskOpen = chatId === null && drafting === tabKey;

  if (chatId === null && !newTaskOpen) {
    return (
      <section className="agent-panel" aria-label="Agent">
        <div className="agent-panel-empty">
          <p className="agent-panel-note">No browser conversation in this scope yet.</p>
          <Button
            variant="primary"
            onClick={() => {
              setError(null);
              setDrafting(tabKey);
            }}
          >
            New task
          </Button>
        </div>
      </section>
    );
  }

  const submit = () => {
    const words = text.trim();
    if (words === "" || busy) return;
    setError(null);
    if (chatId === null) {
      // "New task": a conversation with no mother, its first message carried by the creation.
      setBusy(true);
      const key = tabKey;
      const view = viewKey;
      createBrowserTask(words)
        .then((started) => {
          // The scope's newest conversation and the chat list both changed: without this a
          // return to the tab would show the empty state again and invite a duplicate task.
          refreshLinks();
          setCreated({ key, chatId: started.chat_id });
          setDrafting(null);
          setText("");
        })
        .catch((e: unknown) => {
          if (viewRef.current === view) setError(message(e));
        })
        .finally(() => setBusy(false));
      return;
    }
    const view = viewKey;
    send.mutate(
      { text: words, images: [] },
      {
        // A refused send gives the words back rather than losing them.
        onError: (e: unknown) => {
          if (viewRef.current !== view) return;
          setText(words);
          setError(message(e));
        },
      },
    );
    setText("");
  };

  const link = linkState?.link ?? null;
  const parentId = link !== null && link.unlinked_at === null ? link.parent_chat_id : null;
  const delivery = linkState?.last_delivery ?? null;
  const title = chatId !== null ? (titleOf(chatId) ?? (last?.chat_id === chatId ? last.title : null)) : null;

  const unlink = () => {
    if (chatId === null) return;
    setError(null);
    const view = viewKey;
    unlinkBrowserChat(chatId)
      .then(refreshLinks)
      .catch((e: unknown) => {
        if (viewRef.current === view) setError(message(e));
      });
  };
  const relink = (parent: string) => {
    if (chatId === null || parent === "") return;
    setError(null);
    setRelinking(false);
    const view = viewKey;
    relinkBrowserChat(chatId, parent)
      .then(refreshLinks)
      .catch((e: unknown) => {
        if (viewRef.current === view) setError(message(e));
      });
  };

  return (
    <section className="agent-panel" aria-label="Agent">
      {chatId !== null && (
        <header className="agent-panel-header">
          <h2 className="agent-panel-title">{title ?? "Browser conversation"}</h2>
          <p className="agent-panel-link">
            {parentId !== null ? (
              <>
                Linked to:{" "}
                <Link to="/chats/$chatId" params={{ chatId: parentId }}>
                  {titleOf(parentId) ?? parentId}
                </Link>
              </>
            ) : (
              "Reports to this panel only."
            )}
          </p>
          <div className="agent-panel-actions">
            {parentId !== null && <Button onClick={unlink}>Unlink</Button>}
            <Button aria-expanded={relinking} onClick={() => setRelinking(!relinking)}>
              Relink
            </Button>
          </div>
          {relinking && (
            <Select
              density="sm"
              block
              aria-label="Link to conversation"
              value=""
              onChange={(event) => relink(event.target.value)}
            >
              <option value="">Pick a conversation…</option>
              {(chats ?? [])
                .filter((chat) => chat.chat_id !== chatId)
                .map((chat) => (
                  <option key={chat.chat_id} value={chat.chat_id}>
                    {chat.title ?? chat.chat_id}
                  </option>
                ))}
            </Select>
          )}
          {delivery?.outcome === "refused" && (
            <p className="agent-panel-refused" role="status">
              Not delivered to the conversation: {delivery.reason ?? "unknown"}
            </p>
          )}
        </header>
      )}
      <div className="agent-panel-body">
        {chatId !== null && <MessageList messages={listed(transcript?.turns)} />}
      </div>
      {error !== null && <ErrorNote>{error}</ErrorNote>}
      <div className="agent-panel-composer">
        <ComposerBox
          text={text}
          onText={(next) => setText(next)}
          onSubmit={submit}
          boxRef={boxRef}
          placeholder={PLACEHOLDER}
          sendDisabled={text.trim() === "" || busy || (chatId !== null && send.isPending)}
        />
      </div>
    </section>
  );
}
