import type { ChatGroup, ChatSummary } from "../data/chats";

export type Dot = "needs_input" | "working" | "unread" | "seen";
export type SessionStatus = "needs_input" | "working" | "completed";
export type TabState = "open" | "closed";

export interface SessionFilters {
  statuses: Set<SessionStatus>;
  tabs: Set<TabState>;
  query: string;
}

export interface SessionCounts {
  needs_input: number;
  working: number;
  completed: number;
  open: number;
  closed: number;
  /** needs_input + working */
  active: number;
}

/**
 * The dot beside a session (D4). Colour comes from the tone tokens in the stylesheet; this only
 * names the state. A closed tab with nothing to report shows no dot at all.
 */
export function dotFor(chat: ChatSummary, tabOpen: boolean): Dot | null {
  const activity = chat.activity ?? "idle";
  if (activity === "needs_input") return "needs_input";
  if (activity === "working") return "working";
  if (activity === "unread") return "unread";
  return tabOpen ? "seen" : null;
}

/**
 * The hues a conversation's own colour is drawn from. Eight, spread round the wheel with cyan
 * (Signal Cyan, links only) and pure red (danger) left out; saturation and lightness come from the
 * stylesheet so the same hue reads on both grounds.
 */
export const CHAT_HUES = [24, 45, 95, 140, 215, 250, 285, 325] as const;

/** A conversation's colour, stable per `chat_id`: FNV-1a over the id, into `CHAT_HUES`. */
export function chatHue(chatId: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < chatId.length; i += 1) {
    hash ^= chatId.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return CHAT_HUES[(hash >>> 0) % CHAT_HUES.length];
}

export function statusOf(chat: ChatSummary): SessionStatus {
  const activity = chat.activity ?? "idle";
  return activity === "needs_input" || activity === "working" ? activity : "completed";
}

export function countSessions(rows: ChatSummary[], openTabs: string[]): SessionCounts {
  const open = new Set(openTabs);
  const counts: SessionCounts = {
    needs_input: 0,
    working: 0,
    completed: 0,
    open: 0,
    closed: 0,
    active: 0,
  };
  for (const row of rows) {
    counts[statusOf(row)] += 1;
    if (open.has(row.chat_id)) counts.open += 1;
    else counts.closed += 1;
  }
  counts.active = counts.needs_input + counts.working;
  return counts;
}

/** An empty set means "all"; the query matches title, first message and cwd, ignoring case. */
export function applyFilters(
  rows: ChatSummary[],
  filters: SessionFilters,
  openTabs: string[],
): ChatSummary[] {
  const open = new Set(openTabs);
  const query = filters.query.trim().toLowerCase();
  return rows.filter((row) => {
    if (filters.statuses.size > 0 && !filters.statuses.has(statusOf(row))) return false;
    if (filters.tabs.size > 0 && !filters.tabs.has(open.has(row.chat_id) ? "open" : "closed")) {
      return false;
    }
    if (query === "") return true;
    return [row.title, row.first_message, row.cwd].some(
      (field) => typeof field === "string" && field.toLowerCase().includes(query),
    );
  });
}

/** A `group_id` that names no known group is ungrouped rather than dropped. */
export function byGroup(
  rows: ChatSummary[],
  groups: ChatGroup[],
): { groups: { group: ChatGroup; rows: ChatSummary[] }[]; ungrouped: ChatSummary[] } {
  const known = new Set(groups.map((g) => g.id));
  return {
    groups: groups.map((group) => ({ group, rows: rows.filter((r) => r.group_id === group.id) })),
    ungrouped: rows.filter((r) => r.group_id == null || !known.has(r.group_id)),
  };
}

