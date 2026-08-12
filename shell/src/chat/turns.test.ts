import { describe, expect, it } from "vitest";
import { merge, replyText, turnFromRow, type Turn } from "./turns";
import type { AssistantTurnRow, RunDetail } from "../api";

function turn(overrides: Partial<Turn> & { id: number }): Turn {
  return {
    asked: "a",
    answer: "A",
    status: "completed",
    cost_usd: null,
    failed: false,
    answeredBy: "cloud",
    ...overrides,
  };
}

function row(overrides: Partial<AssistantTurnRow> & { id: number }): AssistantTurnRow {
  return {
    asked: "a",
    answer: null,
    error: null,
    status: "completed",
    cost_usd: null,
    answered_by: "cloud",
    created_at: "2026-08-11T10:00:00+00:00",
    ...overrides,
  };
}

describe("merge", () => {
  it("keeps a turn the daemon has not caught up with yet", () => {
    // The daemon inserts a turn's row while the request that created it is still open, so a history
    // read that overtakes that insert comes back without it. Replacing would lose the message you
    // just sent — the exact loss this whole thing exists to prevent.
    const history = [turn({ id: 1 })];
    const local = [turn({ id: 2, answer: null, status: "running", answeredBy: null })];

    expect(merge(history, local).map((t) => t.id)).toEqual([1, 2]);
  });

  it("lets the daemon win wherever both know a turn", () => {
    const history = [turn({ id: 1, answer: "the real answer" })];
    const local = [turn({ id: 1, answer: null, status: "running" })];

    expect(merge(history, local)).toEqual([turn({ id: 1, answer: "the real answer" })]);
  });
});

describe("turnFromRow", () => {
  it("leaves a live turn without an answer so the poll knows to keep going", () => {
    const live = turnFromRow(row({ id: 3, status: "running", answered_by: "local" }));

    expect(live.answer).toBeNull();
    expect(live.answeredBy).toBe("local");
  });

  it("shows what a failed turn failed with rather than an empty bubble", () => {
    const failed = turnFromRow(row({ id: 4, status: "failed", error: "the CLI exited 1" }));

    expect(failed.answer).toBe("the CLI exited 1");
    expect(failed.failed).toBe(true);
  });

  it("carries a null model through instead of guessing one", () => {
    // Turns from before the daemon recorded this have no model. Filling in "cloud" would be an
    // invented fact, and the transcript draws its change-of-model mark from this field.
    expect(turnFromRow(row({ id: 5, answered_by: null })).answeredBy).toBeNull();
  });
});

describe("replyText", () => {
  it("prefers stdout and falls back to stderr", () => {
    const detail = (over: Partial<RunDetail>) => ({ stdout: null, stderr: null, ...over }) as RunDetail;

    expect(replyText(detail({ stdout: "said", stderr: "noise" }))).toBe("said");
    expect(replyText(detail({ stdout: "   ", stderr: "it broke" }))).toBe("it broke");
    expect(replyText(detail({}))).toBeNull();
  });
});
