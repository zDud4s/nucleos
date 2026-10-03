import { Link } from "@tanstack/react-router";
import { X } from "lucide-react";
import type { ChatSummary } from "../data/chats";
import { StateDot, sessionTitle } from "./SessionColumn";
import { dotFor } from "./sessions";

/**
 * The conversations held open, one tab each.
 *
 * A tab is a `<Link>` and not a button with a handler, for the reason the column's rows are: the
 * routing stays the router's, so middle-click and copy-link keep working. Closing is its own button
 * beside it, never inside it — a button in an anchor is an interactive element in an interactive
 * element, which a screen reader reads as one control and a keyboard cannot reach.
 */
export function ChatTabs({
  tabs,
  rows,
  current,
  selectedLive,
  onOpenChat,
  onClose,
}: {
  tabs: string[];
  rows: ChatSummary[];
  current: string | null;
  /** The open conversation is running, which the list may not have heard of yet. */
  selectedLive: boolean;
  /** Told before the link navigates; see `openingAChat` on the page. */
  onOpenChat: () => void;
  onClose: (id: string) => void;
}) {
  if (tabs.length === 0) return null;
  const byId = new Map(rows.map((row) => [row.chat_id, row]));

  return (
    <div className="chats-tabs" role="tablist" aria-label="Open conversations">
      {tabs.map((id) => {
        const row = byId.get(id);
        const active = id === current;
        const lit =
          row !== undefined && active && selectedLive && row.working !== true
            ? { ...row, working: true, activity: "working" as const }
            : row;
        const name = row === undefined ? "New session" : sessionTitle(row);
        return (
          <div
            key={id}
            className={active ? "chats-tab chats-tab-active" : "chats-tab"}
          >
            <Link
              to={`/chats/${id}`}
              role="tab"
              aria-selected={active}
              className="chats-tab-link"
              onClick={onOpenChat}
            >
              <StateDot dot={lit === undefined ? null : dotFor(lit, true)} />
              <span className="chats-tab-title">{name}</span>
            </Link>
            <button
              type="button"
              className="chats-tab-close"
              aria-label={`Close ${name}`}
              onClick={() => onClose(id)}
            >
              <X className="chats-tab-close-icon" aria-hidden="true" />
            </button>
          </div>
        );
      })}
    </div>
  );
}
