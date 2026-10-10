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
