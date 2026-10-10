import { describe, expect, it } from "vitest";
import type { Agreement, CouncilView, SeatView, StepView, Synthesis } from "../../data/council";
import * as court from "./court-model";

/* ------------------------------------------------------------ fixtures -- */

function step(overrides: Partial<StepView> = {}): StepView {
  return {
    round: 0,
    phase: "answer",
    run_id: 1,
    status: "ok",
    error: null,
    answer: "yes, ship it",
    critique: null,
    changed: null,
    why: null,
    ...overrides,
  };
}

function seat(overrides: Partial<SeatView> = {}): SeatView {
  return {
    seat_idx: 0,
    kind: "cloud",
    ref: "claude-opus-4",
    agent_id: null,
    agent_name: null,
    role: null,
    steps: [step()],
    ...overrides,
  };
}

function view(overrides: Partial<CouncilView> = {}): CouncilView {
  return {
    id: "c-1",
    created_at: "2026-08-18T09:00:00Z",
    question: "should we ship the frontend rewrite?",
    status: "running",
    rounds: 1,
    rounds_run: 0,
    stopped_early: false,
    current_round: 1,
    current_phase: "critique",
    error: null,
    chairman_kind: "cloud",
    chairman_ref: "claude-opus-4",
    chairman_agent_id: null,
    chairman_agent_name: null,
    agreement: null,
    leaderboard: [],
    leaderboard_by_round: [],
    agreement_by_round: [],
    ballot_rounds: [],
    synthesis_run_id: null,
    synthesis: null,
    synthesis_structured: null,
    synthesis_status: null,
    anon_map: { A: 0, B: 1 },
    seats: [seat()],
    ...overrides,
  };
}

/* ------------------------------------------------------------ tests -- */

const close = (a: number, b: number) => expect(Math.abs(a - b)).toBeLessThan(1e-9);

describe("seat geometry", () => {
  it("spreads the eight setup chairs over the whole arc, 168° to 12°", () => {
    const angles = Array.from({ length: 8 }, (_, i) => court.setupAngle(i));
    close(angles[0], 168);
    close(angles[7], 12);
    for (let i = 1; i < 8; i += 1) close(angles[i - 1] - angles[i], 156 / 7);
  });

  it("centres n session seats on 90° with the full room's step, for every n from 1 to 8", () => {
    for (let n = 1; n <= 8; n += 1) {
      const angles = Array.from({ length: n }, (_, i) => court.sessionAngle(i, n));
      close((angles[0] + angles[n - 1]) / 2, 90);
      for (let i = 1; i < n; i += 1) close(angles[i - 1] - angles[i], 156 / 7);
    }
    close(court.sessionAngle(0, 1), 90);
  });

  it("puts eight session seats exactly where the eight setup chairs are", () => {
    for (let i = 0; i < 8; i += 1) close(court.sessionAngle(i, 8), court.setupAngle(i));
  });

  it("places an angle on the ellipse centred (280,118) with radii 232×150, below the centre", () => {
    const bottom = court.pointAt(90);
    close(bottom.x, 280);
    close(bottom.y, 268);
    const left = court.pointAt(168);
    expect(left.x).toBeLessThan(280);
    close(left.y, 118 + 150 * Math.sin((168 * Math.PI) / 180));
  });
});

const agreement = (tau: number | null, level: string, ballots = 3): Agreement => ({
  tau, level, ballots, comparisons: 6,
});

describe("the scales", () => {
  it("is level at τ = 1 and tips 22° at τ ≤ 0, linearly between", () => {
    expect(court.balanceAngle(1)).toBe(0);
    expect(court.balanceAngle(0)).toBe(22);
    expect(court.balanceAngle(-0.5)).toBe(22);
    expect(court.balanceAngle(0.5)).toBeCloseTo(11);
    expect(court.balanceAngle(1.2)).toBe(0);
  });

  it("changes colour exactly at tally.rs's thresholds", () => {
    expect(court.balanceTone(agreement(0.6, "strong"))).toBe("strong");
    expect(court.balanceTone(agreement(0.5999, "split"))).toBe("split");
    expect(court.balanceTone(agreement(0.2, "split"))).toBe("split");
    expect(court.balanceTone(agreement(0.1999, "none"))).toBe("none");
    expect(court.balanceTone(agreement(-1, "none"))).toBe("none");
  });

  it("is level, grey and dashed when the ballots are insufficient", () => {
    const b = court.balanceOf(agreement(null, "insufficient", 2));
    expect(b).toEqual({ angle: 0, tone: "insufficient", label: "too few votes", sub: "fewer than 3 ballots" });
  });

  it("reads 'split · τ 0.21' under the scales", () => {
    expect(court.balanceOf(agreement(0.21, "split")).label).toBe("split · τ 0.21");
  });

  it("says there are no votes yet when there is no agreement at all", () => {
    expect(court.balanceOf(null)).toEqual({ angle: 0, tone: "empty", label: "no votes yet", sub: null });
  });
});

describe("the process line", () => {
  it("is Opening, then Deliberation/Rebuttal per round with no rebuttal after the last, then Verdict", () => {
    expect(court.processPhases(3).map(court.phaseLabel)).toEqual([
      "Opening", "Deliberation I", "Rebuttal I", "Deliberation II", "Rebuttal II", "Deliberation III", "Verdict",
    ]);
    expect(court.processPhases(1).map(court.phaseLabel)).toEqual(["Opening", "Deliberation I", "Verdict"]);
  });

  it("makes a phase reachable only when the council has steps for it", () => {
    // Stopped early in round 1: critique ran, no revise.
    const v = view({
      rounds: 2, rounds_run: 1, stopped_early: true, status: "done", current_round: 1, current_phase: "synthesis",
      synthesis: "done", seats: [seat({ steps: [step(), step({ round: 1, phase: "critique" })] })],
    });
    const line = court.processLine(v);
    expect(line.map((p) => [p.label, p.reachable])).toEqual([
      ["Opening", true], ["Deliberation I", true], ["Rebuttal I", false],
      ["Deliberation II", false], ["Verdict", true],
    ]);
    expect(line.find((p) => p.stoppedHere)?.label).toBe("Deliberation I");
  });

  it("leaves the verdict unreachable on a council that failed before the synthesis", () => {
    const v = view({ status: "error", error: "boom", current_round: 1, current_phase: "critique" });
    expect(court.processLine(v).at(-1)).toMatchObject({ label: "Verdict", reachable: false });
  });

  it("marks the live moment as current", () => {
    const v = view({ current_round: 1, current_phase: "critique", seats: [seat({ steps: [step(), step({ round: 1, phase: "critique", status: "pending" })] })] });
    expect(court.processLine(v).filter((p) => p.current).map((p) => p.label)).toEqual(["Deliberation I"]);
  });

  it("keeps the live phase reachable before any of its steps exists", () => {
    const v = view({ current_round: 1, current_phase: "critique", seats: [seat({ steps: [step()] })] });
    expect(court.processLine(v).find((p) => p.current)).toMatchObject({ label: "Deliberation I", reachable: true });
  });
});

describe("a chair's state", () => {
  const at = (overrides: Partial<StepView>) => step({ round: 1, phase: "critique", ...overrides });
  it("is thinking while pending with no run", () => {
    expect(court.stepState(at({ status: "pending", run_id: null }), "running")).toEqual({ state: "thinking", reason: null });
  });
  it("is speaking only while pending, with a run, in a running council", () => {
    expect(court.stepState(at({ status: "pending", run_id: 7 }), "running").state).toBe("speaking");
    expect(court.stepState(at({ status: "pending", run_id: 7 }), "error").state).toBe("cancelled");
    expect(court.stepState(at({ status: "pending", run_id: 7 }), "cancelled").state).toBe("cancelled");
    expect(court.stepState(at({ status: "pending", run_id: null }), "error").state).toBe("cancelled");
  });
  it("reads ok, error, timeout, invalid, cancelled and skipped", () => {
    expect(court.stepState(at({ status: "ok" }), "running").state).toBe("ok");
    expect(court.stepState(at({ status: "error", error: "it broke" }), "running")).toEqual({ state: "failed", reason: "it broke" });
    expect(court.stepState(at({ status: "timeout", error: null }), "running")).toEqual({ state: "failed", reason: "timeout" });
    expect(court.stepState(at({ status: "invalid", error: "x" }), "running")).toEqual({ state: "failed", reason: "unreadable answer" });
    expect(court.stepState(at({ status: "cancelled" }), "done").state).toBe("cancelled");
    expect(court.stepState(at({ status: "skipped" }), "done").state).toBe("skipped");
  });
  it("is waiting with no step, and cancelled with no step at the moment a dead council stopped", () => {
    expect(court.stepState(undefined, "running")).toEqual({ state: "waiting", reason: null });
    expect(court.stepState(undefined, "error", true)).toEqual({ state: "cancelled", reason: null });
  });
});

describe("the tally at a moment", () => {
  const a1 = agreement(0.7, "strong");
  const a3 = agreement(0.1, "none");
  const b1 = [{ seat_idx: 0, score: 1, n: 2 }, { seat_idx: 1, score: 0.5, n: 2 }];
  const b3 = [{ seat_idx: 1, score: 1, n: 2 }, { seat_idx: 0, score: 0, n: 2 }];
  const v = view({
    rounds: 3, status: "done", current_phase: "synthesis",
    leaderboard_by_round: [b1, b3], agreement_by_round: [a1, a3], ballot_rounds: [1, 3],
    leaderboard: b3, agreement: a3,
  });

  it("has no votes at the opening", () => {
    expect(court.tallyAt(v, { round: 0, phase: "answer" })).toEqual({ board: null, agreement: null });
  });
  it("reads round r through ballot_rounds, not by index", () => {
    expect(court.tallyAt(v, { round: 3, phase: "critique" })).toEqual({ board: b3, agreement: a3 });
    expect(court.tallyAt(v, { round: 1, phase: "revise" })).toEqual({ board: b1, agreement: a1 });
  });
  it("reads a round with no ballots as even sizes and insufficient", () => {
    const t = court.tallyAt(v, { round: 2, phase: "critique" });
    expect(t.board).toBeNull();
    expect(t.agreement?.level).toBe("insufficient");
  });
  it("reads the verdict as the last tally and the last agreement", () => {
    expect(court.tallyAt(v, { round: 3, phase: "synthesis" })).toEqual({ board: b3, agreement: a3 });
  });
});

describe("a chair's size", () => {
  it("is even with no tally", () => {
    expect(court.radiusFor(null, 0)).toBe(court.R_EVEN);
  });
  it("runs 14 to 24 over the score against max(1, best)", () => {
    const board = [{ seat_idx: 0, score: 1, n: 3 }, { seat_idx: 1, score: 0.5, n: 3 }, { seat_idx: 2, score: 0, n: 3 }];
    expect(court.radiusFor(board, 0)).toBe(24);
    expect(court.radiusFor(board, 1)).toBe(19);
    expect(court.radiusFor(board, 2)).toBe(14);
    expect(court.radiusFor(board, 9)).toBe(14);
  });
  it("ranks ties together", () => {
    const board = [{ seat_idx: 0, score: 1, n: 3 }, { seat_idx: 1, score: 1, n: 3 }, { seat_idx: 2, score: 0.2, n: 3 }];
    expect([0, 1, 2].map((s) => court.rankIn(board, s))).toEqual([1, 1, 3]);
  });
});

const synthesis = (overrides: Partial<Synthesis> = {}): Synthesis => ({
  answer: "ship", consensus: [], disagreements: [], minority: null,
  confidence: { level: "high", why: "w" }, open_questions: [], ...overrides,
});

describe("the verdict's lines", () => {
  const board = [{ seat_idx: 0, score: 1, n: 2 }, { seat_idx: 2, score: 1, n: 2 }, { seat_idx: 1, score: 0.3, n: 2 }];
  it("links the judge to every seat tied at the top Borda", () => {
    expect(court.topLinks(board)).toEqual([
      { kind: "top", from: "judge", to: "seat-0", title: "most voted" },
      { kind: "top", from: "judge", to: "seat-2", title: "most voted" },
    ]);
  });
  it("draws one dissent line per pair of seats on different positions, titled by the topic", () => {
    const s = synthesis({ disagreements: [{ topic: "cost", positions: [{ seats: [0, 1], view: "a" }, { seats: [2], view: "b" }] }] });
    expect(court.dissentLinks(s)).toEqual([
      { kind: "dissent", from: "seat-0", to: "seat-2", title: "cost" },
      { kind: "dissent", from: "seat-1", to: "seat-2", title: "cost" },
    ]);
  });
  it("marks dissent only with a minority, and only the strictly smaller position", () => {
    const d = { topic: "t", positions: [{ seats: [0, 1], view: "a" }, { seats: [2], view: "b" }] };
    expect(court.dissenters(synthesis({ disagreements: [d] }))).toEqual(new Set());
    expect(court.dissenters(synthesis({ minority: "m", disagreements: [d] }))).toEqual(new Set([2]));
    const tie = { topic: "t", positions: [{ seats: [0], view: "a" }, { seats: [1], view: "b" }] };
    expect(court.dissenters(synthesis({ minority: "m", disagreements: [tie] }))).toEqual(new Set());
    expect(court.dissenters(synthesis({ minority: "m" }))).toEqual(new Set());
  });
});
