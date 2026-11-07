/**
 * The chat-to-machine link store: a leaf module (it imports nothing from `data/`), so both the
 * machine list and the chat hooks can use it without an import cycle. It is subscribable, so a
 * hook that reads a link re-renders when a new one is recorded.
 */
export const CHAT_MACHINES_KEY = "nucleos-chat-machines";
const CHAT_LINKS_CHANGED = "nucleos-chat-links-changed";

function raw(): string {
  try {
    return localStorage.getItem(CHAT_MACHINES_KEY) ?? "{}";
  } catch {
    return "{}";
  }
}

let parsedFrom = "";
let parsed: Record<string, string> = {};

function links(): Record<string, string> {
  const text = raw();
  if (text !== parsedFrom) {
    parsedFrom = text;
    try {
      const value: unknown = JSON.parse(text);
      parsed =
        typeof value === "object" && value !== null && !Array.isArray(value)
          ? (value as Record<string, string>)
          : {};
    } catch {
      parsed = {};
    }
  }
  return parsed;
}

/** The machine id a chat is linked to, or "here" when nobody linked it. */
export function linkedMachineOf(chatId: string): string {
  const machine = links()[chatId];
  return typeof machine === "string" ? machine : "here";
}

/** Subscribe to link changes, in this document and in other tabs. */
export function subscribeChatLinks(listener: () => void): () => void {
  window.addEventListener(CHAT_LINKS_CHANGED, listener);
  window.addEventListener("storage", listener);
  return () => {
    window.removeEventListener(CHAT_LINKS_CHANGED, listener);
    window.removeEventListener("storage", listener);
  };
}

/** The snapshot for `useSyncExternalStore`: the stored text, stable until a link changes. */
export function chatLinksSnapshot(): string {
  return raw();
}

/** Link chats to a machine. Chats on this machine are the default and are never stored. */
export function writeChatLinks(chatIds: string[], machine: string): void {
  const current = { ...links() };
  let changed = false;
  for (const chatId of chatIds) {
    if (machine === "here") {
      if (chatId in current) {
        delete current[chatId];
        changed = true;
      }
    } else if (current[chatId] !== machine) {
      current[chatId] = machine;
      changed = true;
    }
  }
  if (!changed) return;
  storeLinks(current);
}

/** Persist the links and tell subscribers; a full or disabled store must not throw. */
function storeLinks(current: Record<string, string>): void {
  try {
    localStorage.setItem(CHAT_MACHINES_KEY, JSON.stringify(current));
  } catch {
    // Nothing to persist to; the links stay as the last successful write left them.
  }
  window.dispatchEvent(new Event(CHAT_LINKS_CHANGED));
}

/** Drop every link to a machine that was forgotten. */
export function forgetChatLinks(machine: string): void {
  const current = { ...links() };
  let changed = false;
  for (const [chatId, linked] of Object.entries(current)) {
    if (linked === machine) {
      delete current[chatId];
      changed = true;
    }
  }
  if (!changed) return;
  storeLinks(current);
}
