/**
 * The one conversation the Voice page speaks into.
 *
 * **The owner's decision, 2026-09-21: a spoken conversation belongs to a single dedicated chat.** The
 * daemon refuses a turn that names no chat (`core/src/voice.rs`, `CaptureError::NoChat`), so the page
 * has to answer "which one?" before anybody can say a word, and the three possible answers were
 * weighed: the last chat somebody had open, a new conversation each session, or one that is always
 * the same. The last one won because opening the page is then the whole gesture — nothing to pick,
 * nothing to remember — and because a conversation is a billed row that is archived rather than
 * deleted, so a session that opens one and hears nothing must not leave anything behind. The price,
 * accepted knowingly, is one long conversation with no subject that grows for as long as it is used.
 *
 * **Two ways home, because neither is reliable alone.** The id is remembered in this browser's
 * storage, which is instant and survives nothing: a private window, a cleared profile or a reinstall
 * all come back empty, and each would otherwise open a second conversation that looks exactly like
 * the first. So the conversation is also NAMED, and a lost id is found again by its name. That is the
 * whole reason it is named at all — `POST /assistant/chats` takes no title, and left alone the daemon
 * would eventually title it after whatever was said into it, which is a good title and an unfindable
 * one.
 *
 * **Why the list is fetched rather than read from `useChats`.** That query is polled, so on a page
 * somebody opened a moment ago it has not answered yet — and a decision made against an empty list is
 * the decision to open a second conversation. One request, at the moment the question is actually
 * asked, cannot be early.
 */

import { useCallback, useRef, useState } from "react";

import { apiFetch } from "./client";
import type { ChatSummary } from "./chats";

/** What the one spoken conversation is called, and the name it is found again by. */
export const VOICE_CHAT_TITLE = "Voice";

const KEY = "nucleos.voice-chat";

export interface VoiceChat {
  /** The conversation spoken turns go to, or `null` until one has been opened this session. */
  chatId: string | null;
  /** Why there is nothing to speak into. Cleared by the next attempt, never by a timer. */
  trouble: string | null;
  /**
   * The conversation to speak into, opening one if this machine has never spoken before.
   *
   * Called when the mode is switched ON rather than when the first sentence is ready. Both orders
   * need the same `POST /assistant/chats` with no message in it, so neither is more honest about
   * "a conversation is opened by saying something" — and with one conversation reused for ever, the
   * worst this order can leave behind is a single empty conversation, once, which the next word
   * fills.
   */
  open: () => Promise<string | null>;
}

/**
 * The remembered id, or `null`.
 *
 * A storage that throws — a private window, a locked-down webview, a quota — is not an error path:
 * the id is a shortcut, and the name below finds the same conversation without it.
 */
function remembered(): string | null {
  try {
    const stored = window.localStorage.getItem(KEY);
    return stored === null || stored === "" ? null : stored;
  } catch {
    return null;
  }
}

function remember(chatId: string): void {
  try {
    window.localStorage.setItem(KEY, chatId);
  } catch {
    // Nothing worth saying: the next session finds the same conversation by its name instead.
  }
}

export function useVoiceChat(): VoiceChat {
  const [chatId, setChatId] = useState<string | null>(null);
  const [trouble, setTrouble] = useState<string | null>(null);
  /* Answered once per session: the conversation cannot stop existing while this page holds it, and
     the person who archives it mid-session gets the daemon's own refusal on the next turn, which says
     more than a guess made here would. */
  const settledRef = useRef<string | null>(null);

  const open = useCallback(async (): Promise<string | null> => {
    setTrouble(null);
    if (settledRef.current !== null) return settledRef.current;

    const settle = (found: string) => {
      settledRef.current = found;
      remember(found);
      setChatId(found);
      return found;
    };

    try {
      const listed = await apiFetch<ChatSummary[]>("/assistant/chats");
      const mine = remembered();
      // The id first and the name second: an id is exact, while a name is a thing a person can
      // change. The list leaves archived conversations out, so a remembered id that is missing from
      // it is one somebody has finished with — which is a conversation to open, not a fault.
      const found =
        listed.find((chat) => chat.chat_id === mine) ??
        listed.find((chat) => chat.title === VOICE_CHAT_TITLE);
      if (found !== undefined) return settle(found.chat_id);
    } catch (error) {
      setTrouble(sentenceFor(error));
      return null;
    }

    let opened: string;
    try {
      opened = (
        await apiFetch<{ chat_id: string }>("/assistant/chats", {
          method: "POST",
          body: JSON.stringify({}),
        })
      ).chat_id;
    } catch (error) {
      setTrouble(sentenceFor(error));
      return null;
    }

    try {
      await apiFetch<void>(`/assistant/chats/${encodeURIComponent(opened)}`, {
        method: "PATCH",
        body: JSON.stringify({ title: VOICE_CHAT_TITLE }),
      });
    } catch {
      // Deliberately not fatal, and deliberately not reported. The conversation exists and works;
      // what it lacks is the label that finds it again after this browser forgets the id. Refusing to
      // talk over a missing label would throw away the working half of the two calls.
    }

    return settle(opened);
  }, []);

  return { chatId, trouble, open };
}

function sentenceFor(error: unknown): string {
  return error instanceof Error ? error.message : "no conversation could be opened for this";
}
