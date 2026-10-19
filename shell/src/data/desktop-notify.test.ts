import { describe, expect, it } from "vitest";
import {
  EMPTY_SEEN,
  MAX_TOASTS,
  toastsFor,
  type NotifyInputs,
  type NotifySeen,
} from "./desktop-notify";
import { policyAllows, type FeedEntry, type NotifyPolicy } from "./feed";
import type { ChatSummary } from "./chats";
import type { Proposal } from "./system";

const OPEN: NotifyPolicy = { families: [], kinds: [] };

function proposal(id: number, over: Partial<Proposal> = {}): Proposal {
  return { id, kind: "action-approval", tool_name: "Bash", reasoning: "run the tests", ...over } as Proposal;
}

function line(id: number, kind: string, over: Partial<FeedEntry> = {}): FeedEntry {
  return { id, kind, summary: `${kind} happened`, run_id: null, project_id: null, subject: null, created_at: "", ...over };
}

function chat(id: string, waiting: number, over: Partial<ChatSummary> = {}): ChatSummary {
  return { chat_id: id, title: `Chat ${id}`, waiting, archived_at: null, ...over } as ChatSummary;
}

/** Inputs for a window that is behind another one, with nothing in the three lists unless given. */
function inputs(over: Partial<NotifyInputs> = {}): NotifyInputs {
  return {
    focused: false,
    enabled: true,
    policy: OPEN,
    proposals: [],
    feed: [],
    chats: [],
    seen: EMPTY_SEEN,
    ...over,
  };
}

/** Reads once so the next reading is a "new" one — what the app does at launch. */
function seeded(over: Partial<NotifyInputs> = {}): NotifySeen {
  return toastsFor(inputs({ focused: true, ...over })).seen;
}

describe("the desktop notifier's decision", () => {
  it("the first reading only remembers what is there", () => {
    const out = toastsFor(
      inputs({
        proposals: [proposal(1), proposal(2)],
        feed: [line(7, "job_failed")],
        chats: [chat("a", 2)],
      }),
    );

    expect(out.toasts).toEqual([]);
    // ...and a second reading of the same lists is not news either.
    const again = toastsFor(
      inputs({
        proposals: [proposal(1), proposal(2)],
        feed: [line(7, "job_failed")],
        chats: [chat("a", 2)],
        seen: out.seen,
      }),
    );
    expect(again.toasts).toEqual([]);
  });

  it("a new proposal while the window is behind makes one toast", () => {
    const seen = seeded({ proposals: [proposal(1)] });

    const out = toastsFor(
      inputs({ proposals: [proposal(1), proposal(2, { tool_name: "Write", reasoning: "save notes" })], seen }),
    );

    expect(out.toasts).toEqual([
      { key: "proposal:2", title: "Waiting: Write", body: "save notes", to: "/waiting" },
    ]);
    // The same list again says nothing: the toast was the news, once.
    expect(toastsFor(inputs({ proposals: [proposal(1), proposal(2)], seen: out.seen })).toasts).toEqual([]);
  });

  it("nothing is shown while the window is in front", () => {
    const seen = seeded({ proposals: [], feed: [line(1, "job_failed")] });

    const out = toastsFor(
      inputs({ focused: true, proposals: [proposal(5)], feed: [line(1, "job_failed"), line(2, "job_failed")], seen }),
    );
    expect(out.toasts).toEqual([]);

    // Whatever arrived while somebody was looking has been seen: stepping away does not replay it.
    const later = toastsFor(inputs({ proposals: [proposal(5)], feed: [line(1, "job_failed"), line(2, "job_failed")], seen: out.seen }));
    expect(later.toasts).toEqual([]);
  });

  it("nothing is shown while switched off, and switching on does not replay the gap", () => {
    const seen = seeded({});
    const off = toastsFor(inputs({ enabled: false, proposals: [proposal(1)], seen }));
    expect(off.toasts).toEqual([]);

    const on = toastsFor(inputs({ proposals: [proposal(1)], seen: off.seen }));
    expect(on.toasts).toEqual([]);
  });

  it("a feed line the policy silences makes no toast", () => {
    const policy: NotifyPolicy = { families: [{ selector: "job_", enabled: false }], kinds: [] };
    const seen = seeded({ policy, feed: [line(1, "run_done")] });

    const out = toastsFor(
      inputs({
        policy,
        feed: [line(1, "run_done"), line(2, "job_started"), line(3, "run_done", { run_id: 41, summary: "run 41 finished" })],
        seen,
      }),
    );

    expect(out.toasts).toHaveLength(1);
    expect(out.toasts[0]).toMatchObject({ key: "feed:3", body: "run 41 finished", to: "/runs/41" });
  });

  it("a feed line with no run opens the feed", () => {
    const seen = seeded({ feed: [line(1, "worktree_gc")] });
    const out = toastsFor(inputs({ feed: [line(1, "worktree_gc"), line(2, "worktree_gc")], seen }));
    expect(out.toasts.map((t) => t.to)).toEqual(["/feed"]);
  });

  it("more than three new things become three toasts and a summary", () => {
    const seen = seeded({ proposals: [proposal(1)] });
    const news = [proposal(2), proposal(3), proposal(4), proposal(5), proposal(6)];

    const out = toastsFor(inputs({ proposals: [proposal(1), ...news], seen }));

    expect(MAX_TOASTS).toBe(3);
    expect(out.toasts).toHaveLength(4);
    expect(out.toasts.slice(0, 3).map((t) => t.key)).toEqual(["proposal:2", "proposal:3", "proposal:4"]);
    expect(out.toasts[3]).toMatchObject({ title: "and 2 more", to: "/waiting" });
    // All five are now seen, shown or summarised.
    expect(toastsFor(inputs({ proposals: [proposal(1), ...news], seen: out.seen })).toasts).toEqual([]);
  });

  it("a conversation answered while away makes one toast that opens it", () => {
    const seen = seeded({ chats: [chat("a", 0), chat("b", 1)] });

    const out = toastsFor(inputs({ chats: [chat("a", 1, { title: "Plan the move" }), chat("b", 1)], seen }));

    expect(out.toasts).toHaveLength(1);
    expect(out.toasts[0]).toMatchObject({ title: "Plan the move: answered", to: "/chats/a" });
  });

  it("opening a conversation lowers its count without making a toast, and the next answer still does", () => {
    const seen = seeded({ chats: [chat("a", 3)] });
    const read = toastsFor(inputs({ chats: [chat("a", 0)], seen }));
    expect(read.toasts).toEqual([]);

    const next = toastsFor(inputs({ chats: [chat("a", 1)], seen: read.seen }));
    expect(next.toasts).toHaveLength(1);
  });

  it("cuts a long body at 140 characters", () => {
    const seen = seeded({});
    const out = toastsFor(inputs({ proposals: [proposal(1, { reasoning: "x".repeat(400) })], seen }));
    expect(out.toasts[0].body.length).toBeLessThanOrEqual(140);
    expect(out.toasts[0].body.endsWith("…")).toBe(true);
  });

  it("passes a third party's text through as plain text", () => {
    const seen = seeded({ feed: [line(1, "email_received")] });
    const hostile = '<img src=x onerror="alert(1)"> **bold**';
    const out = toastsFor(inputs({ feed: [line(1, "email_received"), line(2, "email_received", { summary: hostile })], seen }));
    expect(out.toasts[0].body).toBe(hostile);
  });

  it("waits for a list that has not answered yet instead of treating it as empty", () => {
    // Proposals answered; the feed has not. If `undefined` seeded an empty feed, its whole
    // history would arrive a poll later as new.
    const first = toastsFor(inputs({ proposals: [proposal(1)], feed: undefined, chats: undefined }));
    const second = toastsFor(
      inputs({ proposals: [proposal(1)], feed: [line(1, "job_failed"), line(2, "job_failed")], chats: [chat("a", 4)], seen: first.seen }),
    );
    expect(second.toasts).toEqual([]);
  });

  it("an archived conversation never makes a toast", () => {
    const seen = seeded({ chats: [chat("a", 0)] });
    const out = toastsFor(inputs({ chats: [chat("a", 2, { archived_at: "2026-10-01T00:00:00Z" })], seen }));
    expect(out.toasts).toEqual([]);
  });
});

/**
 * The rows of `sidecars/telegram/notifier/policy_test.go`, copied as a table. The shell resolves
 * the policy a second time (the sidecar decides for Telegram, this decides for the desktop), and
 * two implementations are only the same thing while a test holds them to the same answers.
 */
describe("the policy reads the same as the Telegram notifier", () => {
  const layered: NotifyPolicy = {
    families: [
      { selector: "job_", enabled: false },
      { selector: "job_item_", enabled: true },
      { selector: "worktree_", enabled: true },
    ],
    kinds: [
      { selector: "job_failed", enabled: true },
      { selector: "worktree_gc", enabled: false },
    ],
  };

  const table: [string, string, boolean][] = [
    ["a kind rule beats the family that silenced it", "job_failed", true],
    ["a kind rule beats the family that allowed it", "worktree_gc", false],
    ["a kind with no exception follows its family", "job_started", false],
    ["the longest matching prefix wins, not the first", "job_item_done", true],
    ["an allowing family allows", "worktree_pruned", true],
    ["a kind no rule claims passes", "council_opened", true],
    ["a prefix that is not at the start does not match", "nightly_job_started", true],
  ];

  it.each(table)("%s", (_name, kind, want) => {
    expect(policyAllows(layered, kind)).toBe(want);
  });

  it("the longest prefix wins with the rules listed in the other order", () => {
    const reversed: NotifyPolicy = {
      families: [
        { selector: "job_item_", enabled: true },
        { selector: "job_", enabled: false },
      ],
      kinds: [],
    };
    expect(policyAllows(reversed, "job_item_done")).toBe(true);
    expect(policyAllows(reversed, "job_started")).toBe(false);
  });

  it("the empty policy passes everything", () => {
    for (const kind of ["job_failed", "web.fetched", "anything_at_all"]) {
      expect(policyAllows({ families: [], kinds: [] }, kind)).toBe(true);
    }
  });

  it("an empty kind passes", () => {
    expect(policyAllows({ families: [{ selector: "job_", enabled: false }], kinds: [] }, "")).toBe(true);
  });

  it("a rule with an empty selector is ignored", () => {
    const muted: NotifyPolicy = {
      families: [{ selector: "", enabled: false }],
      kinds: [{ selector: "", enabled: false }],
    };
    for (const kind of ["job_failed", "council_opened", ""]) {
      expect(policyAllows(muted, kind)).toBe(true);
    }
  });
});
