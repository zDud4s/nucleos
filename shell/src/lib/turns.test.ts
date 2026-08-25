import { describe, expect, it } from "vitest";
import {
  anyTurnLive,
  marksBetween,
  merge,
  turnFromRow,
  unreadTotal,
  type AssistantTurnRow,
  type Turn,
} from "./turns";

/* ------------------------------------------------------------- fixtures -- */

function row(overrides: Partial<AssistantTurnRow> = {}): AssistantTurnRow {
  return {
    id: 1,
    asked: "hello",
    answer: null,
    error: null,
    status: "completed",
    cost_usd: null,
    answered_by: null,
    session_id: null,
    created_at: "2026-08-18T09:00:00Z",
    did: [],
    images: [],
    thought: [],
    thought_tokens: null,
    context_fill: null,
    context_rotates_at: 140000,
    relayed_from_chat_id: null,
    relayed_from_title: null,
    ...overrides,
  };
}

function turn(overrides: Partial<Turn> = {}): Turn {
  return {
    id: 1,
    asked: "hello",
    answer: null,
    status: "completed",
    cost_usd: null,
    answeredBy: null,
    sessionId: null,
    did: [],
    images: [],
    thought: [],
    thoughtTokens: null,
    contextFill: null,
    rotatesAt: 140000,
    relayedFrom: null,
    ...overrides,
  };
}

describe("turnFromRow, on where a turn came from", () => {
  // A relayed turn is not something the person reading it said. Drawn without this the transcript
  // attributes another conversation's words to them, under their own name — the transcript getting
  // the speaker wrong, in the one place somebody goes to find out who said what.
  it("carries the conversation that handed a turn over", () => {
    const relayed = turnFromRow(
      row({
        relayed_from_chat_id: "0d2b-…-9f1",
        relayed_from_title: "the planning conversation",
      }),
    );

    expect(relayed.relayedFrom).toEqual({
      chatId: "0d2b-…-9f1",
      title: "the planning conversation",
    });
  });

  // Most conversations have no title: the daemon writes one only once it has summarised the chat.
  // The id is what always resolves, so it travels beside the title rather than being replaced by
  // it — a page given only the title could say nothing at all about an unnamed conversation.
  it("keeps the id when the conversation has no name yet", () => {
    const relayed = turnFromRow(
      row({ relayed_from_chat_id: "0d2b-…-9f1", relayed_from_title: null }),
    );

    expect(relayed.relayedFrom).toEqual({ chatId: "0d2b-…-9f1", title: null });
  });

  // The ordinary case, and the one worth pinning: a stray object here would put a "handed over"
  // note above every message the person ever typed.
  it("reads a turn nobody relayed as one the person typed", () => {
    expect(turnFromRow(row()).relayedFrom).toBeNull();
  });

  // A daemon older than the columns sends neither key — the same rule `did` and `images` already
  // follow. A conversation that will not draw over one missing field is the worse answer.
  it("reads a daemon that does not send the columns as no relay", () => {
    const older = row();
    delete (older as Partial<AssistantTurnRow>).relayed_from_chat_id;
    delete (older as Partial<AssistantTurnRow>).relayed_from_title;

    expect(turnFromRow(older).relayedFrom).toBeNull();
  });
});

describe("turnFromRow, on what a turn thought", () => {
  // The size, because the words do not exist: the CLI sends every thinking block with its text
  // stripped and a running token estimate beside it. This is the whole of what a turn can be
  // asked about, so it is the thing that must survive the read.
  it("carries how much it thought, beside an answer that stays its own", () => {
    const thoughtful = turnFromRow(
      row({ thought: [], thought_tokens: 177, answer: "é o parser de datas" }),
    );

    expect(thoughtful.thoughtTokens).toBe(177);
    expect(thoughtful.thought).toEqual([]);
    expect(thoughtful.answer).toBe("é o parser de datas");
  });

  // A daemon older than the columns sends neither key. A conversation that refuses to draw over
  // one missing field is a worse answer than a conversation that draws without it.
  it("reads a turn that carries neither as one that did not think", () => {
    const older = row();
    delete (older as Partial<AssistantTurnRow>).thought;
    delete (older as Partial<AssistantTurnRow>).thought_tokens;

    expect(turnFromRow(older).thought).toEqual([]);
    // Null, never zero: "did not think" and "thought nothing measurable" are different claims,
    // and only one of them is knowable here.
    expect(turnFromRow(older).thoughtTokens).toBeNull();
  });
});

/* ------------------------------------------------------------- turnFromRow -- */

describe("turnFromRow", () => {
  it("leaves answer null while the turn is live", () => {
    for (const status of ["pending", "running"]) {
      const live = turnFromRow(row({ status, answer: "not yet visible", error: null }));
      expect(live.answer).toBeNull();
    }
  });

  it("prefers error over an empty answer once settled", () => {
    const failed = turnFromRow(row({ status: "failed", answer: "", error: "the tool crashed" }));
    expect(failed.answer).toBe("the tool crashed");

    const failedNull = turnFromRow(row({ status: "failed", answer: null, error: "boom" }));
    expect(failedNull.answer).toBe("boom");
  });

  it("uses the answer over stray error text once settled", () => {
    const completed = turnFromRow(
      row({ status: "completed", answer: "here you go", error: "unrelated stderr noise" }),
    );
    expect(completed.answer).toBe("here you go");
  });

  it("carries a genuinely empty settled turn as null, not as an empty string", () => {
    const empty = turnFromRow(row({ status: "cancelled", answer: null, error: null }));
    expect(empty.answer).toBeNull();
  });
});

/* ------------------------------------------------------------------ merge -- */

describe("merge", () => {
  it("keeps a locally-known turn the daemon's history has not caught up with, sorted back by id", () => {
    const history = [turn({ id: 1 }), turn({ id: 2 })];
    const local = [turn({ id: 1 }), turn({ id: 2 }), turn({ id: 3, asked: "just sent" })];

    const result = merge(history, local);

    expect(result.map((t) => t.id)).toEqual([1, 2, 3]);
    expect(result[2].asked).toBe("just sent");
  });

  it("lets the daemon's version of a turn win where both know it", () => {
    const history = [turn({ id: 1, answer: "the real answer" })];
    const local = [turn({ id: 1, answer: null })];

    const result = merge(history, local);

    expect(result).toHaveLength(1);
    expect(result[0].answer).toBe("the real answer");
  });
});

/* ------------------------------------------------------------- anyTurnLive -- */

describe("anyTurnLive", () => {
  it("is true for pending and running only", () => {
    expect(anyTurnLive([turn({ status: "pending" })])).toBe(true);
    expect(anyTurnLive([turn({ status: "running" })])).toBe(true);

    for (const status of ["completed", "failed", "cancelled", "interrupted", "timed_out"]) {
      expect(anyTurnLive([turn({ status })])).toBe(false);
    }
  });

  it("reads no answer yet as live, so the fast poll never turns itself off before it starts", () => {
    expect(anyTurnLive(undefined)).toBe(true);
  });

  it("is true if any one turn in the transcript is live, not only the last", () => {
    expect(anyTurnLive([turn({ id: 1, status: "completed" }), turn({ id: 2, status: "running" })])).toBe(
      true,
    );
  });
});

/* ------------------------------------------------------------ marksBetween -- */

describe("marksBetween", () => {
  it("draws nothing before the transcript's first turn", () => {
    expect(marksBetween(null, turn())).toEqual([]);
  });

  it("emits no brain mark when either answered_by is null", () => {
    const a = turn({ answeredBy: null });
    const b = turn({ answeredBy: "cloud" });
    expect(marksBetween(a, b)).toEqual([]);
    expect(marksBetween(b, a)).toEqual([]);
  });

  it("emits an asymmetric brain mark for cloud to local and local to cloud", () => {
    const cloud = turn({ answeredBy: "cloud" });
    const local = turn({ answeredBy: "local" });

    expect(marksBetween(cloud, local)).toEqual([{ kind: "brain", from: "cloud", to: "local" }]);
    expect(marksBetween(local, cloud)).toEqual([{ kind: "brain", from: "local", to: "cloud" }]);
  });

  it("emits no brain mark when the model did not change", () => {
    const first = turn({ answeredBy: "cloud" });
    const second = turn({ answeredBy: "cloud" });
    expect(marksBetween(first, second)).toEqual([]);
  });

  it("emits a restart mark on a changed session_id", () => {
    const first = turn({ sessionId: "s-1" });
    const second = turn({ sessionId: "s-2" });
    expect(marksBetween(first, second)).toEqual([{ kind: "restart" }]);
  });

  it("emits no restart mark when either session_id is null, or when they match", () => {
    expect(marksBetween(turn({ sessionId: null }), turn({ sessionId: "s-1" }))).toEqual([]);
    expect(marksBetween(turn({ sessionId: "s-1" }), turn({ sessionId: null }))).toEqual([]);
    expect(marksBetween(turn({ sessionId: "s-1" }), turn({ sessionId: "s-1" }))).toEqual([]);
  });

  it("can emit both marks together, at most two", () => {
    const previous = turn({ answeredBy: "cloud", sessionId: "s-1" });
    const next = turn({ answeredBy: "local", sessionId: "s-2" });
    expect(marksBetween(previous, next)).toEqual([
      { kind: "brain", from: "cloud", to: "local" },
      { kind: "restart" },
    ]);
  });
});

/* -------------------------------------------------------------- unreadTotal -- */

describe("unreadTotal", () => {
  it("sums waiting across every conversation", () => {
    expect(unreadTotal([{ waiting: 2 }, { waiting: 0 }, { waiting: 5 }])).toBe(7);
  });

  it("is zero for an empty list", () => {
    expect(unreadTotal([])).toBe(0);
  });
});
