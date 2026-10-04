import { useSyncExternalStore } from "react";
import type { Attachment } from "../data/chats";

/**
 * Words said to a conversation that has no project yet, kept in this window until it has one.
 *
 * A conversation does not start before it knows where it runs: a first turn taken with no
 * directory is a turn with no tools, and the session it opens is the one every later turn
 * resumes. So a message typed before a project is chosen is HELD — on screen, never sent to the
 * daemon — and goes out on its own the moment the project is set.
 *
 * Module state and not component state, for two reasons. The front door creates the conversation
 * and navigates to it, so the words have to survive the box they were typed in unmounting. And
 * clicking away to another conversation and back must not lose them either.
 *
 * One message per conversation, not a queue: a second thing said before the project is chosen is
 * appended to the first. Two held messages would go out as two turns back to back, and the second
 * would only ever be a continuation of the first.
 */
export interface HeldMessage {
  text: string;
  images: Attachment[];
}

const held = new Map<string, HeldMessage>();
const listeners = new Set<() => void>();

function changed() {
  for (const listener of listeners) listener();
}

/** Hold `message` for `chatId`, appended to anything already held there. */
export function holdMessage(chatId: string, message: HeldMessage, maxImages = Infinity): void {
  const was = held.get(chatId);
  const next: HeldMessage =
    was === undefined
      ? { text: message.text, images: message.images.slice(0, maxImages) }
      : {
          text: [was.text, message.text].filter((part) => part !== "").join("\n\n"),
          images: [...was.images, ...message.images].slice(0, maxImages),
        };
  held.set(chatId, next);
  changed();
}

/** What is held for `chatId`, or null. */
export function heldMessage(chatId: string): HeldMessage | null {
  return held.get(chatId) ?? null;
}

/** Remove what is held for `chatId` and return it, so exactly one caller ever sends it. */
export function takeHeld(chatId: string): HeldMessage | null {
  const was = held.get(chatId) ?? null;
  if (was !== null) {
    held.delete(chatId);
    changed();
  }
  return was;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The held message for `chatId`, re-rendering when it is held, appended to, or taken. */
export function useHeldMessage(chatId: string): HeldMessage | null {
  return useSyncExternalStore(
    subscribe,
    () => heldMessage(chatId),
    () => heldMessage(chatId),
  );
}

/** For tests: forget everything held. */
export function resetHeld(): void {
  held.clear();
  changed();
}
