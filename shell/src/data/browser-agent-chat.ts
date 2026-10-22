import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { UseQueryResult } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { scopeProject } from "./browser-tabs";
import { keys } from "./keys";

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

/**
 * The query keys of this file, kept local rather than in `keys.ts` (contract E3(a)).
 * Everything sits under one root so a link change can refresh both reads at once.
 */
export const browserAgentKeys = {
  all: ["browser-agent"] as const,
  link: (chatId: string) => ["browser-agent", "link", chatId] as const,
  last: (scope: string) => ["browser-agent", "last", scope] as const,
};

/** The conversation an agent tab's session answers to, or `null` when there is none. */
export function chatForSession(
  sessions: ReadonlyArray<{ id: number; chat_id?: string | null }> | undefined,
  sessionId: number | string,
): string | null {
  if (sessions === undefined) return null;
  // Embedded requests name the session as a string; compare as numbers.
  const id = typeof sessionId === "number" ? sessionId : Number(sessionId);
  if (!Number.isFinite(id)) return null;
  const row = sessions.find((session) => session.id === id);
  return row?.chat_id ?? null;
}

/** `POST /assistant/chats/{id}/link`: set (or clear, with `null`) the mother of a browser conversation. */
async function postLink(childChatId: string, parentChatId: string | null): Promise<BrowserChatLink> {
  const answer = await apiFetch<{ link: BrowserChatLink }>(
    `/assistant/chats/${encodeURIComponent(childChatId)}/link`,
    {
      method: "POST",
      body: JSON.stringify({ parent_chat_id: parentChatId }),
    },
  );
  return answer.link;
}

/** "New task": a new chat, linked to the panel only (null parent), then the task sent as its first message. */
export async function createBrowserTask(text: string): Promise<BrowserTaskStarted> {
  const opened = await apiFetch<{ chat_id: string }>("/assistant/chats", {
    method: "POST",
    body: JSON.stringify({}),
  });
  // Linked before the first message, so the run the message starts already knows it reports to
  // the panel rather than to nobody.
  await postLink(opened.chat_id, null);
  const sent = await apiFetch<{ turn_id?: number; queued?: boolean }>("/assistant/message", {
    method: "POST",
    body: JSON.stringify({ chat_id: opened.chat_id, text, images: [], wait_if_busy: true }),
  });
  return { chat_id: opened.chat_id, turn_id: sent?.turn_id, queued: sent?.queued };
}

/** Link a browser conversation (the child) to a new mother conversation. */
export async function relinkBrowserChat(childChatId: string, parentChatId: string): Promise<BrowserChatLink> {
  return postLink(childChatId, parentChatId);
}

/** Unlink a browser conversation from its mother: it then reports to the panel only. */
export async function unlinkBrowserChat(childChatId: string): Promise<BrowserChatLink> {
  return postLink(childChatId, null);
}

/** The active link of a browser conversation and its last delivery; idle while `chatId` is null. */
export function useBrowserChatLink(chatId: string | null): UseQueryResult<BrowserChatLinkState> {
  return useQuery({
    queryKey: browserAgentKeys.link(chatId ?? ""),
    queryFn: () =>
      apiFetch<BrowserChatLinkState>(`/assistant/chats/${encodeURIComponent(chatId ?? "")}/link`),
    enabled: chatId !== null,
  });
}

/** The newest actively linked browser conversation of a scope key (a project id or `NO_PROJECT`), or null. */
export function useLastBrowserChat(scope: string): UseQueryResult<BrowserAgentChatItem | null> {
  return useQuery({
    queryKey: browserAgentKeys.last(scope),
    queryFn: async () => {
      // The scope key never reaches the wire as such: no project is `no_project=1` (contract E-D1).
      const project = scopeProject(scope);
      const params = new URLSearchParams({ limit: "20" });
      if (project === null) params.set("no_project", "1");
      else params.set("project_id", project);
      const items = await apiFetch<BrowserAgentChatItem[]>(`/assistant/browser-agents?${params.toString()}`);
      // The daemon answers newest link first.
      return items?.[0] ?? null;
    },
  });
}

/**
 * The three writes as mutations that refresh what they change: the chat list (a new
 * conversation, or a renamed parent) and this file's own reads.
 */
export function useBrowserAgentActions() {
  const queryClient = useQueryClient();
  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: keys.chats.all });
    void queryClient.invalidateQueries({ queryKey: browserAgentKeys.all });
  };
  const createTask = useMutation({
    mutationFn: (text: string) => createBrowserTask(text),
    retry: false,
    onSettled: refresh,
  });
  const relink = useMutation({
    mutationFn: ({ childChatId, parentChatId }: { childChatId: string; parentChatId: string }) =>
      relinkBrowserChat(childChatId, parentChatId),
    retry: false,
    onSettled: refresh,
  });
  const unlink = useMutation({
    mutationFn: (childChatId: string) => unlinkBrowserChat(childChatId),
    retry: false,
    onSettled: refresh,
  });
  return { createTask, relink, unlink };
}
