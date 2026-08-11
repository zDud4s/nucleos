import type { ChatRow } from "../api";
import { Button, ConfirmButton } from "../ui";

interface ChatListProps {
  chats: ChatRow[];
  selected: string | null;
  /**
   * The conversations with a turn in flight.
   *
   * A set and not a single id, because the daemon holds one turn slot PER CHAT: two conversations
   * can be thinking at the same time, and a window that showed one would be describing a daemon
   * that does not exist.
   */
  busy: Set<string>;
  onSelect: (chatId: string) => void;
  onNew: () => void;
  onArchive: (chatId: string) => void;
}

/**
 * What a conversation is called.
 *
 * The name somebody wrote wins; otherwise the first thing said in it. The fallback is computed here
 * from what the daemon sends rather than stored as the title when the chat is opened, so it cannot
 * end up naming a conversation after a message that stopped being what it is about.
 */
function chatLabel(chat: ChatRow): { text: string; unused: boolean } {
  if (chat.title !== null && chat.title.trim() !== "") return { text: chat.title, unused: false };
  if (chat.first_message !== null && chat.first_message.trim() !== "") {
    return { text: chat.first_message, unused: false };
  }
  return { text: "Nothing said yet", unused: true };
}

/** The conversations this app opened, and the door to a new one. */
function ChatList({ chats, selected, busy, onSelect, onNew, onArchive }: ChatListProps) {
  return (
    <nav className="chat-list" aria-label="Conversations">
      <Button variant="approve" size="sm" onClick={onNew}>
        New conversation
      </Button>
      <ul>
        {chats.map((chat) => {
          const label = chatLabel(chat);
          return (
            <li
              key={chat.chat_id}
              className={chat.chat_id === selected ? "cl-row is-open" : "cl-row"}
            >
              <button
                type="button"
                className="cl-open"
                aria-current={chat.chat_id === selected ? "true" : undefined}
                onClick={() => onSelect(chat.chat_id)}
              >
                <span className={label.unused ? "cl-name a-note" : "cl-name"}>{label.text}</span>
                <span className="cl-meta">
                  <span className="b-run">{chat.brain}</span>
                  {busy.has(chat.chat_id) && <span className="cl-busy">thinking…</span>}
                </span>
              </button>
              {/*
                Archive, not delete. Every turn is a billed run, and the daemon keeps them where the
                money is recorded — this only takes the conversation off the list. Two clicks because
                there is no way back to it from here.
              */}
              <ConfirmButton
                variant="ghost"
                size="sm"
                intent="stop"
                confirmLabel="Archive?"
                onConfirm={() => onArchive(chat.chat_id)}
              >
                Archive
              </ConfirmButton>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

export default ChatList;
