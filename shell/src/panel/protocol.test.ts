import { describe, expect, it } from "vitest";
import {
  giveBack,
  initialState,
  pendingSay,
  reduce,
  say,
  type Incoming,
} from "./protocol";

const agentLine: Incoming = {
  v: 1,
  kind: "message",
  role: "agent",
  text: "Opened the page.",
  ts: "2026-10-09T10:00:00Z",
};

describe("panel protocol", () => {
  it("panel protocol appends a pushed message once across a history replay", () => {
    let state = reduce(initialState(), agentLine);
    expect(state.messages).toHaveLength(1);
    // The sidecar replays the whole history into a fresh context: the same
    // role+ts+text must not show twice.
    state = reduce(state, agentLine);
    expect(state.messages).toHaveLength(1);
    // Same text at another instant is a different message.
    state = reduce(state, { ...agentLine, ts: "2026-10-09T10:00:05Z" });
    expect(state.messages).toHaveLength(2);
    expect(state.messages[0]).toMatchObject({ role: "agent", text: "Opened the page." });
  });

  it("panel protocol marks an undelivered say for retry", () => {
    let state = pendingSay(initialState(), "c1", "hello there", "2026-10-09T10:01:00Z");
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0]).toMatchObject({ role: "person", text: "hello there" });
    expect(state.messages[0].failed).toBeFalsy();

    state = reduce(state, { v: 1, kind: "delivery", id: "c1", ok: false });
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0].failed).toBe(true);

    // A later success for the same id clears the mark, and a delivery for an
    // unknown id changes nothing.
    const stranger = reduce(state, { v: 1, kind: "delivery", id: "zz", ok: true });
    expect(stranger.messages[0].failed).toBe(true);
    const cleared = reduce(state, { v: 1, kind: "delivery", id: "c1", ok: true });
    expect(cleared.messages[0].failed).toBeFalsy();
  });

  it("panel protocol folds state, ask_wheel and ask_keep", () => {
    let state = reduce(initialState(), {
      v: 1,
      kind: "state",
      mode: "human",
      host: "example.org",
      collapsed: true,
    });
    expect(state).toMatchObject({ mode: "human", host: "example.org", collapsed: true });
    state = reduce(state, { v: 1, kind: "ask_wheel", reason: "login needed" });
    expect(state.askWheel).toBe("login needed");
    state = reduce(state, { v: 1, kind: "ask_keep", hosts: ["https://example.org"] });
    expect(state.askKeep).toEqual(["https://example.org"]);
  });

  it("panel protocol builds the outgoing messages with the version", () => {
    expect(say("c1", "hi")).toEqual({ v: 1, kind: "say", id: "c1", text: "hi" });
    expect(giveBack("log in done")).toEqual({ v: 1, kind: "give_back", note: "log in done" });
  });
});
