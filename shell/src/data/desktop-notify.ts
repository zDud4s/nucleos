import { policyAllows, readFeedKind, type FeedEntry, type NotifyPolicy } from "./feed";
import type { ChatSummary } from "./chats";
import type { Proposal } from "./system";

/**
 * What the desktop notifier decides, read purely — no React, no Tauri, no clock.
 *
 * The notifier is the second channel next to Telegram: it tells the owner something is waiting
 * when the window is not in front. Whether to speak is a decision about three lists and a flag,
 * so it lives here where a test can hand it the lists; the component only feeds it and sends
 * what it returns.
 */

/** The most toasts one reading shows; anything past them is folded into a single summary. */
export const MAX_TOASTS = 3;

/** A body is cut here. A toast is a nudge to look, not the place to read. */
const BODY_LIMIT = 140;

/**
 * What a reading has already accounted for, per list.
 *
 * `null` is "this list has not answered yet" and is different from an empty list: the first
 * answer of each list only seeds, so a feed of twenty old lines never arrives as twenty toasts.
 * Seeded per list because the three queries answer on their own schedules.
 */
export interface NotifySeen {
  proposals: Set<number> | null;
  /** The newest feed line accounted for. */
  feedAfter: number | null;
  /** Each conversation's unread count at the last reading. */
  chats: Map<string, number> | null;
}

export const EMPTY_SEEN: NotifySeen = { proposals: null, feedAfter: null, chats: null };

export interface NotifyInputs {
  /** The window is in front of the owner. */
  focused: boolean;
  /** The owner's switch for this channel. */
  enabled: boolean;
  policy: NotifyPolicy;
  /** `undefined` while a list has not answered yet. */
  proposals: Proposal[] | undefined;
  feed: FeedEntry[] | undefined;
  chats: ChatSummary[] | undefined;
  seen: NotifySeen;
}

export interface Toast {
  /** Stable per thing announced, so a sender can tell a repeat from news. */
  key: string;
  title: string;
  body: string;
  /** Where the window goes when the toast is acted on. */
  to: string;
}

/** One line of plain text, cut to the body limit with an ellipsis. */
function brief(text: string): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > BODY_LIMIT ? `${flat.slice(0, BODY_LIMIT - 1)}…` : flat;
}

/**
 * The toasts this reading calls for, and what has now been accounted for.
 *
 * `seen` advances on EVERY reading, shown or not: what arrived while the window was in front, or
 * while the switch was off, was seen (or chosen not to be) and must not come back as news the
 * moment the window is covered or the switch is turned on.
 *
 * Proposals ignore the policy, as they do for Telegram: the policy silences feed lines, never a
 * decision somebody has to make. Everything that reaches a toast is handed over as plain text —
 * an email subject is somebody else's words, and a toast has no markup to interpret them in.
 */
export function toastsFor(inputs: NotifyInputs): { toasts: Toast[]; seen: NotifySeen } {
  const { focused, enabled, policy, seen } = inputs;
  const speak = enabled && !focused;
  const found: Toast[] = [];

  let proposals = seen.proposals;
  if (inputs.proposals !== undefined) {
    const known = seen.proposals;
    proposals = new Set(inputs.proposals.map((p) => p.id));
    if (known !== null && speak) {
      for (const p of inputs.proposals) {
        if (known.has(p.id)) continue;
        found.push({
          key: `proposal:${p.id}`,
          title: `Waiting: ${p.tool_name ?? p.kind}`,
          body: brief(p.reasoning || p.kind),
          to: "/waiting",
        });
      }
    }
  }

  let chats = seen.chats;
  if (inputs.chats !== undefined) {
    const known = seen.chats;
    chats = new Map(inputs.chats.map((c) => [c.chat_id, c.waiting]));
    if (known !== null && speak) {
      for (const c of inputs.chats) {
        if (c.archived_at) continue;
        // An answer is news when the unread count rose; opening the chat lowers it and says nothing.
        if (c.waiting <= (known.get(c.chat_id) ?? 0)) continue;
        found.push({
          key: `chat:${c.chat_id}:${c.waiting}`,
          title: `${c.title?.trim() || "Conversation"}: answered`,
          body: "A new answer is waiting.",
          to: `/chats/${c.chat_id}`,
        });
      }
    }
  }

  let feedAfter = seen.feedAfter;
  if (inputs.feed !== undefined) {
    const known = seen.feedAfter;
    feedAfter = inputs.feed.reduce((top, row) => Math.max(top, row.id), known ?? 0);
    if (known !== null && speak) {
      const fresh = inputs.feed.filter((row) => row.id > known).sort((a, b) => a.id - b.id);
      for (const row of fresh) {
        if (!policyAllows(policy, row.kind)) continue;
        found.push({
          key: `feed:${row.id}`,
          title: readFeedKind(row.kind)?.label ?? row.kind,
          body: brief(row.summary),
          to: row.kind === "capture_digest" ? "/brain" : row.run_id === null ? "/feed" : `/runs/${row.run_id}`,
        });
      }
    }
  }

  const next: NotifySeen = { proposals, feedAfter, chats };
  if (found.length <= MAX_TOASTS) return { toasts: found, seen: next };

  const shown = found.slice(0, MAX_TOASTS);
  const rest = found.slice(MAX_TOASTS);
  shown.push({
    key: `more:${rest[0].key}`,
    title: `and ${rest.length} more`,
    body: "Open NucleOS to see them.",
    to: rest.some((toast) => toast.to === "/waiting") ? "/waiting" : "/feed",
  });
  return { toasts: shown, seen: next };
}
