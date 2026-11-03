import type { QueryKey } from "@tanstack/react-query";
import { useSyncExternalStore } from "react";
import {
  CHAT_MACHINES_KEY,
  chatLinksSnapshot,
  linkedMachineOf,
  subscribeChatLinks,
  writeChatLinks,
} from "./chatLinks";
import { apiFetch } from "./client";
import type { ChatSummary } from "./chats";
import { withMachine, type MachineId } from "./machines";

/**
 * Which machine each chat lives on.
 *
 * A chat id is only meaningful to the machine that made it, so every call about a chat has to go to
 * that machine. The link is kept in the same non-secret place as the machine list (local storage),
 * written when a chat is created or first listed, so a reload reads and writes the same machine.
 */
export { CHAT_MACHINES_KEY };

/** The machine a chat runs on; a chat nobody linked is on this one. */
export function chatMachineOf(chatId: string): MachineId {
  return linkedMachineOf(chatId);
}

/**
 * Subscribe a component to the link store, so a link recorded later (the chat list arriving for a
 * chat opened by URL) re-renders it and every key and request below follows the new machine.
 */
export function useChatLinks(): string {
  return useSyncExternalStore(subscribeChatLinks, chatLinksSnapshot);
}

/** The machine a chat runs on, read reactively. */
export function useChatMachine(chatId: string): MachineId {
  useChatLinks();
  return chatMachineOf(chatId);
}

/** Link chats to a machine. Chats on this machine are the default and are never stored. */
export function rememberChatMachine(chatIds: string[], machine: MachineId): void {
  writeChatLinks(chatIds, machine);
}

/**
 * A query key scoped to a machine. This machine keeps its historical keys, so the existing
 * invalidations still reach it; any other machine goes through `withMachine`.
 */
export function machineKey(key: QueryKey, machine: MachineId): QueryKey {
  return machine === "here" ? key : withMachine(key, machine);
}

/** The key of a chat-scoped record, on the machine that chat lives on. */
export function chatKey<K extends QueryKey>(key: K, chatId: string): K {
  return machineKey(key, chatMachineOf(chatId)) as unknown as K;
}

/** `apiFetch` addressed to a chat's own machine; this machine's call is the unchanged two-argument one. */
export function chatFetch<T>(chatId: string, path: string, init?: RequestInit): Promise<T> {
  const machine = chatMachineOf(chatId);
  if (machine !== "here") return apiFetch<T>(path, init, machine);
  // Exactly the call made before machines existed, so nothing observes a trailing `undefined`.
  return init === undefined ? apiFetch<T>(path) : apiFetch<T>(path, init);
}

/** One list row: the chat, the machine it runs on, and whether that machine is answering. */
export interface ChatRow extends ChatSummary {
  machine: MachineId;
  /** Present only when more than one machine is listed, so a single install draws nothing new. */
  machineName?: string;
  /** The row comes from the last result held, because its machine did not answer. */
  unreachable: boolean;
}

export interface MachineChats {
  machine: MachineId;
  name?: string;
  chats: ChatSummary[] | undefined;
  /** The machine's latest read failed: `chats` is the last good result, if any. */
  failed: boolean;
}

/**
 * One list from every machine's list. A machine that failed keeps its last rows, marked
 * unreachable, and never takes the others down with it. With one source the order is untouched;
 * with several, rows are interleaved by last activity, newest first.
 */
export function mergeChats(byMachine: MachineChats[]): ChatRow[] {
  const rows = byMachine.flatMap(({ machine, name, chats, failed }) =>
    (chats ?? []).filter((chat) => chat.moved_to == null).map(
      (chat): ChatRow => ({
        ...chat,
        machine,
        ...(name === undefined ? {} : { machineName: name }),
        unreachable: failed,
      }),
    ),
  );
  if (byMachine.filter((entry) => (entry.chats ?? []).length > 0).length < 2) return rows;
  return rows
    .map((row, index) => ({ row, index }))
    .sort((a, b) => (b.row.last_activity ?? "").localeCompare(a.row.last_activity ?? "") || a.index - b.index)
    .map(({ row }) => row);
}

/** A project as the picker sees it: where it was found and its git remote, if it has one. */
export interface ProjectRef {
  machine: MachineId;
  remote: string | null;
}

export interface MachineProjects {
  machine: MachineId;
  projects: { remote?: string | null }[] | undefined;
}

/**
 * The machines a project can run on: its own, plus any other machine holding a project with the
 * same git remote. A project with no remote exists only where it is. With no project (a new
 * conversation that has not chosen one) every machine is offered.
 */
export function machinesForProject(
  project: ProjectRef | null | undefined,
  all: MachineId[],
  held: MachineProjects[],
): MachineId[] {
  if (project === null || project === undefined) return all;
  return all.filter((machine) => {
    if (machine === project.machine) return true;
    if (project.remote === null) return false;
    const there = held.find((entry) => entry.machine === machine);
    const wanted = normalizeRemote(project.remote);
    return (there?.projects ?? []).some(
      (candidate) => candidate.remote != null && normalizeRemote(candidate.remote) === wanted,
    );
  });
}

/**
 * A git remote reduced to what names the repository: `host/path`, host lowercased, without scheme,
 * credentials, a trailing `.git` or a trailing slash, so `git@host:a/b.git` and `https://host/a/b`
 * compare equal.
 */
export function normalizeRemote(remote: string): string {
  let text = remote.trim();
  const scp = /^(?:[^@/\s]+@)?([^:/\s]+):(?!\/\/)(.+)$/.exec(text);
  if (scp !== null && !/^[a-z][a-z0-9+.-]*:\/\//i.test(text)) {
    text = `${scp[1]}/${scp[2]}`;
  } else {
    text = text.replace(/^[a-z][a-z0-9+.-]*:\/\//i, "").replace(/^[^@/]*@/, "");
  }
  text = text.replace(/\/+$/, "").replace(/\.git$/i, "").replace(/\/+$/, "");
  const slash = text.indexOf("/");
  const host = (slash < 0 ? text : text.slice(0, slash)).replace(/:\d+$/, "").toLowerCase();
  return slash < 0 ? host : `${host}${text.slice(slash)}`;
}
