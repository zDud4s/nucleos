import { describe, expect, it } from "vitest";
import type { CouncilConfig } from "../../data/council";
import * as draft from "./court-draft";

const config = (over: Partial<CouncilConfig> = {}): CouncilConfig => ({
  configured: true, default_rounds: 2, max_rounds: 3, roles: ["skeptic"],
  default_roster: {
    chairman: { kind: "cloud", ref: "opus" },
    members: [{ kind: "cloud", ref: "opus" }, { agent: "ag-1" }, { kind: null, ref: null }],
  },
  ...over,
});

describe("the court draft", () => {
  it("seats the configured roster in chairs 0..n-1 of eight, untouched", () => {
    const d = draft.draftFromConfig(config());
    expect(d.chairs).toHaveLength(8);
    expect(d.chairs.map((c) => c?.name ?? null)).toEqual(["opus", "ag-1", "default", null, null, null, null, null]);
    expect(d.chairs[2]?.seat).toBeNull(); // ref-less: shown seated, cannot be restated
    expect(d.touched).toBe(false);
    expect(draft.occupiedCount(d)).toBe(3);
  });

  it("is an empty room when nothing is configured", () => {
    const d = draft.draftFromConfig(config({ configured: false, default_roster: null }));
    expect(draft.occupiedCount(d)).toBe(0);
    expect(d.chairman).toBeNull();
  });

  it("marks the roster touched when a chair or the judge changes, and not when a role does", () => {
    const d = draft.draftFromConfig(config());
    expect(draft.withRole(d, 0, "skeptic").touched).toBe(false);
    expect(draft.seatAt(d, 4, { seat: { kind: "local", ref: "qwen" }, name: "Qwen" }).touched).toBe(true);
    expect(draft.release(d, 0).touched).toBe(true);
    expect(draft.release(d, 0).roles[0]).toBeNull();
    expect(draft.withJudge(d, { seat: { agent: "ag-2" }, name: "two" }).touched).toBe(true);
  });

  it("brings an 'Ask again' draft back touched, deleted seats as empty chairs", () => {
    const d = draft.draftFromAskAgain({
      question: "q", chairman: { agent: "ag-1" }, members: [{ kind: "cloud", ref: "opus" }, null],
      roles: { 0: "skeptic" }, rounds: 2,
    });
    expect(d.touched).toBe(true);
    expect(d.chairs.slice(0, 2).map((c) => c?.name ?? null)).toEqual(["opus", null]);
    expect(d.roles[0]).toBe("skeptic");
  });

  it("drops seats the catalogues no longer offer, and leaves the draft alone when nothing changed", () => {
    const d = draft.draftFromAskAgain({ question: "q", chairman: { agent: "gone" }, members: [{ kind: "cloud", ref: "opus" }], roles: {}, rounds: 1 });
    const pruned = draft.pruneUnoffered(d, new Set(["ag-1"]), new Set(["opus"]));
    expect(pruned.chairman).toBeNull();
    expect(pruned.chairs[0]?.name).toBe("opus");
    const kept = draft.pruneUnoffered(pruned, new Set(["ag-1"]), new Set(["opus"]));
    expect(kept).toBe(pruned);
  });

  it("maps a model choice to local only for the local brain", () => {
    expect(draft.seatOfModel({ id: "q", label: "Q", brain: "local" } as never)).toEqual({ kind: "local", ref: "q" });
    expect(draft.seatOfModel({ id: "o", label: "O", brain: "openrouter" } as never)).toEqual({ kind: "cloud", ref: "o" });
  });
});

describe("convening from the court", () => {
  const c = config();
  it("sends the question and nothing else from the untouched configured room", () => {
    const req = draft.requestFrom("  well? ", draft.draftFromConfig(c), null, c);
    expect(req).toEqual({ question: "well?" });
    expect(Object.keys(req)).toEqual(["question"]);
  });

  it("sends the roster once touched: occupied chairs in chair order, agent or kind/ref only", () => {
    let d = draft.draftFromConfig(config({ default_roster: null }));
    d = draft.withJudge(d, { seat: { agent: "ag-1" }, name: "the sceptic" });
    d = draft.seatAt(d, 3, { seat: { kind: "local", ref: "qwen" }, name: "Qwen" });
    d = draft.seatAt(d, 1, { seat: { agent: "ag-1" }, name: "the sceptic" });
    expect(draft.requestFrom("q", d, null, c)).toEqual({
      question: "q",
      roster: { chairman: { agent: "ag-1" }, members: [{ agent: "ag-1" }, { kind: "local", ref: "qwen" }] },
    });
  });

  it("keys roles by the member's index in the request, with or without a roster", () => {
    let d = draft.draftFromConfig(c);
    d = draft.withRole(d, 1, "skeptic");
    expect(draft.requestFrom("q", d, null, c)).toEqual({ question: "q", roles: { "1": "skeptic" } });
    let t = draft.draftFromConfig(config({ default_roster: null }));
    t = draft.withJudge(t, { seat: { agent: "a" }, name: "a" });
    t = draft.seatAt(t, 5, { seat: { agent: "b" }, name: "b" });
    t = draft.withRole(t, 5, "skeptic");
    expect(draft.requestFrom("q", t, null, c).roles).toEqual({ "0": "skeptic" });
  });

  it("sends rounds only when they differ from the default, held to [1, max]", () => {
    const d = draft.draftFromConfig(c);
    expect(draft.requestFrom("q", d, 2, c).rounds).toBeUndefined();
    expect(draft.requestFrom("q", d, 3, c).rounds).toBe(3);
    expect(draft.roundsOf(9, c)).toBe(3);
    expect(draft.roundsOf(null, c)).toBe(2);
  });

  it("blocks for the config's reasons, then for an empty room, a missing judge or an unrestateable seat", () => {
    const d = draft.draftFromConfig(c);
    expect(draft.conveneBlocked(true, false, null, d)).toBe("Reading the council config…");
    expect(draft.conveneBlocked(false, true, null, d)).toBe("The council config could not be read.");
    expect(draft.conveneBlocked(false, false, config({ configured: false }), d)).toMatch(/No roster in ~\/\.nucleos\/council\.yaml/);
    expect(draft.conveneBlocked(false, false, c, d)).toBeNull(); // untouched: the ref-less seat is the file's business
    const touched = draft.seatAt(d, 5, { seat: { agent: "x" }, name: "x" });
    expect(draft.conveneBlocked(false, false, c, touched)).toBe("Seat 3 keeps the file's default model — choose who sits there.");
    const empty = [0, 1, 2].reduce((acc, i) => draft.release(acc, i), d);
    expect(draft.conveneBlocked(false, false, c, empty)).toBe("Seat at least one member.");
    const judgeless = { ...draft.seatAt(empty, 0, { seat: { agent: "x" }, name: "x" }), chairman: null };
    expect(draft.conveneBlocked(false, false, c, judgeless)).toBe("Choose a chairman.");
  });

  it("estimates the ceiling from the occupied chairs", () => {
    // ceilingCalls(3, 2) = 3 answers + 6 critiques + 3 revisions + 2 chairman calls; stepsInSequence(2) = 5.
    expect(draft.ceilingLine(draft.draftFromConfig(c), 2)).toBe("up to 14 calls · 5 steps in sequence");
  });
});
