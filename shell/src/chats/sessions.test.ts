// @vitest-environment node
import { describe, expect, it } from "vitest";
import type { ChatGroup, ChatSummary } from "../data/chats";
import { applyFilters, byGroup, countSessions, dotFor, statusOf } from "./sessions";

function chat(id: string, over: Partial<ChatSummary> = {}): ChatSummary {
  return { chat_id: id, title: id, unread: 0, ...over } as unknown as ChatSummary;
}

describe("sessions", () => {
  it("derives the dot from activity and tab state", () => {
    expect(dotFor(chat("a", { activity: "needs_input" }), false)).toBe("needs_input");
    expect(dotFor(chat("a", { activity: "needs_input" }), true)).toBe("needs_input");
    expect(dotFor(chat("a", { activity: "working" }), false)).toBe("working");
    expect(dotFor(chat("a", { activity: "unread" }), true)).toBe("unread");
    expect(dotFor(chat("a", { activity: "idle" }), true)).toBe("seen");
    // A closed tab with nothing to say shows no dot, but unread still does.
    expect(dotFor(chat("a", { activity: "idle" }), false)).toBeNull();
    expect(dotFor(chat("a", { activity: "unread" }), false)).toBe("unread");
    // An older daemon sends no activity: it reads as idle.
    expect(dotFor(chat("a"), true)).toBe("seen");
    expect(dotFor(chat("a"), false)).toBeNull();
  });

  const rows = [
    chat("n", { activity: "needs_input", title: "Deploy plan", cwd: "C:/work/alpha" }),
    chat("w", { activity: "working", title: "Refactor", first_message: "Rename the Widget" }),
    chat("u", { activity: "unread", title: "Notes" }),
    chat("i", { activity: "idle", title: "Old" }),
  ];

  it("counts and filters by status, tab state and search", () => {
    expect(statusOf(rows[0])).toBe("needs_input");
    expect(statusOf(rows[1])).toBe("working");
    expect(statusOf(rows[2])).toBe("completed");
    expect(statusOf(rows[3])).toBe("completed");

    const counts = countSessions(rows, ["w", "i"]);
    expect(counts).toMatchObject({ needs_input: 1, working: 1, completed: 2, open: 2, closed: 2, active: 2 });

    const none = { statuses: new Set<never>(), tabs: new Set<never>(), query: "" };
    expect(applyFilters(rows, none, ["w"])).toHaveLength(4);
    expect(applyFilters(rows, { ...none, statuses: new Set(["completed" as const]) }, []).map((r) => r.chat_id)).toEqual(["u", "i"]);
    expect(applyFilters(rows, { ...none, tabs: new Set(["open" as const]) }, ["w", "i"]).map((r) => r.chat_id)).toEqual(["w", "i"]);
    expect(applyFilters(rows, { ...none, tabs: new Set(["closed" as const]) }, ["w", "i"]).map((r) => r.chat_id)).toEqual(["n", "u"]);
    expect(applyFilters(rows, { ...none, query: "WIDGET" }, []).map((r) => r.chat_id)).toEqual(["w"]);
    expect(applyFilters(rows, { ...none, query: "alpha" }, []).map((r) => r.chat_id)).toEqual(["n"]);
    expect(applyFilters(rows, { ...none, query: "deploy" }, []).map((r) => r.chat_id)).toEqual(["n"]);
    expect(
      applyFilters(rows, { statuses: new Set(["completed" as const]), tabs: new Set(["open" as const]), query: "old" }, ["i"]).map((r) => r.chat_id),
    ).toEqual(["i"]);
  });

  it("splits rows into user groups and ungrouped", () => {
    const g = (id: number, name: string): ChatGroup => ({ id, name, position: id, created_at: "" });
    const list = [
      chat("a", { group_id: 1 }),
      chat("b", { group_id: 2 }),
      chat("c", { group_id: null }),
      chat("d"),
      chat("e", { group_id: 99 }),
      chat("f", { group_id: 1 }),
    ];
    const out = byGroup(list, [g(1, "One"), g(2, "Two"), g(3, "Empty")]);
    expect(out.groups.map((x) => [x.group.name, x.rows.map((r) => r.chat_id)])).toEqual([
      ["One", ["a", "f"]],
      ["Two", ["b"]],
      ["Empty", []],
    ]);
    // A group_id that names no group is ungrouped, not lost.
    expect(out.ungrouped.map((r) => r.chat_id)).toEqual(["c", "d", "e"]);
  });
});
