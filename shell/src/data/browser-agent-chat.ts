import type { UseQueryResult } from "@tanstack/react-query";

/**
 * The shell's side of the linked browser agent: which conversation drives an
 * agent tab, the "New task" flow, relinking a browser conversation to a mother
 * conversation, and the panel's reads (the active link and the last delivery).
 */

/** `Link` of `GET /assistant/chats/{id}/link`: who a browser conversation reports to. */
export interface BrowserChatLink {
  id: number;
  child_chat_id: string;
  /** `null` = linked only to the panel (a "New task" with no mother conversation). */
  parent_chat_id: string | null;
  kind: string;
  created_by_run: number | null;
  created_at: string;
  unlinked_at: string | null;
}

/** `Delivery` of `GET /assistant/chats/{id}/link`: what happened to the last reply sent up. */
export interface BrowserChatDelivery {
  run_id: number;
  outcome: "delivered" | "queued" | "refused";
  /** The `relay_refusal_response` slug when `outcome` is `refused`; `null` otherwise. */
  reason: string | null;
  created_at: string;
}

/** The full answer of `GET /assistant/chats/{id}/link`. */
export interface BrowserChatLinkState {
  link: BrowserChatLink | null;
  last_delivery: BrowserChatDelivery | null;
}

/** One item of `GET /assistant/browser-agents`: an actively linked browser conversation. */
export interface BrowserAgentChatItem {
  chat_id: string;
  title: string | null;
  parent_chat_id: string | null;
  parent_title: string | null;
  project_id: string | null;
  created_at: string;
}

/** What `POST /assistant/message` answers, carried back by {@link createBrowserTask}. */
export interface BrowserTaskStarted {
  chat_id: string;
  turn_id?: number;
  queued?: boolean;
}

/** The conversation an agent tab's session answers to, or `null` when there is none. */
export function chatForSession(
  _sessions: ReadonlyArray<{ id: number; chat_id: string | null }> | undefined,
  _sessionId: number | string,
): string | null {
  // RED stub
  throw new Error("RED stub");
}

/** "New task": a new chat, linked to the panel only (null parent), then the task sent as its first message. */
export async function createBrowserTask(_text: string): Promise<BrowserTaskStarted> {
  // RED stub
  throw new Error("RED stub");
}

/** Link a browser conversation (the child) to a new mother conversation. */
export async function relinkBrowserChat(_childChatId: string, _parentChatId: string): Promise<BrowserChatLink> {
  // RED stub
  throw new Error("RED stub");
}

/** Unlink a browser conversation from its mother: it then reports to the panel only. */
export async function unlinkBrowserChat(_childChatId: string): Promise<BrowserChatLink> {
  // RED stub
  throw new Error("RED stub");
}

/** The active link of a browser conversation and its last delivery; idle while `chatId` is null. */
export function useBrowserChatLink(_chatId: string | null): UseQueryResult<BrowserChatLinkState> {
  // RED stub
  throw new Error("RED stub");
}

/** The newest actively linked browser conversation of a scope key (a project id or `NO_PROJECT`), or null. */
export function useLastBrowserChat(_scope: string): UseQueryResult<BrowserAgentChatItem | null> {
  // RED stub
  throw new Error("RED stub");
}
