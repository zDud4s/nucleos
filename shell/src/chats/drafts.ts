import type { Attachment } from "../data/chats";

/**
 * What was typed into a conversation's box, and where its transcript was scrolled to, kept in this
 * window while another conversation is open.
 *
 * Module state and not component state, like `held.ts`: the page keeps one composer and one
 * scroller and re-points them at whichever conversation is open, so anything they held for the one
 * just left is gone unless it lives outside them. Coming back finds the words and the place again.
 *
 * Not persisted: a draft belongs to this window's session. A draft with no text and no pictures is
 * not stored at all, so an emptied box leaves nothing behind.
 */
export interface Draft {
  text: string;
  images: Attachment[];
}

const drafts = new Map<string, Draft>();
const scrolls = new Map<string, number>();

/** The draft for `chatId`; an empty one when there is none. */
export function readDraft(chatId: string): Draft {
  return drafts.get(chatId) ?? { text: "", images: [] };
}

/** Keep `draft` for `chatId`; a draft with no text and no images is dropped instead. */
export function saveDraft(chatId: string, draft: Draft): void {
  if (draft.text === "" && draft.images.length === 0) {
    drafts.delete(chatId);
    return;
  }
  drafts.set(chatId, draft);
}

/** Forget the draft for `chatId` (it was sent). */
export function clearDraft(chatId: string): void {
  drafts.delete(chatId);
}

/** Where `chatId`'s transcript was scrolled to, or null when it never was. */
export function readScroll(chatId: string): number | null {
  return scrolls.get(chatId) ?? null;
}

/** Remember the scroll position of `chatId`'s transcript. */
export function saveScroll(chatId: string, top: number): void {
  scrolls.set(chatId, top);
}
