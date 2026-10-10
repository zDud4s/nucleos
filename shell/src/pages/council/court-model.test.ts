import { describe, expect, it } from "vitest";
import type { Agreement, CouncilConfig, CouncilView, SeatView, StepView, Synthesis } from "../../data/council";
import * as court from "./court-model";
import { chairsOf, rememberChairs } from "./court-chairs";
import { draftFromConfig, release } from "./court-draft";

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

const cfg: CouncilConfig = {
  configured: true, default_rounds: 2, max_rounds: 3, roles: ["skeptic"],
  default_roster: {
    chairman: { kind: "cloud", ref: "opus" },
    members: [{ kind: "cloud", ref: "opus" }, { agent: "ag-1" }, { kind: null, ref: null }],
  },
};

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

describe("a chair's name inside its circle", () => {
  const widest = (fit: { text: string; fontSize: number }) => fit.text.length * fit.fontSize * court.LABEL_EM;

  it("drops the vendor prefix, so two Claude models read apart", () => {
    expect(court.shortModelName("claude-opus-4")).toBe("opus-4");
    expect(court.shortModelName("claude-sonnet-4")).toBe("sonnet-4");
    expect(court.shortModelName("gpt-5-codex")).toBe("gpt-5-codex");
    expect(court.shortModelName("claude-")).toBe("claude-");
  });

  it("never lets a label run past its circle, at any radius", () => {
    for (const name of ["claude-opus-4", "claude-sonnet-4", "gpt-5-codex", "gemini-2.5-pro-preview-0605", "Marketing Strategist"]) {
      for (const r of [court.R_MIN, court.R_EVEN, court.R_MAX]) {
        expect(widest(court.fitSeatLabel(name, r))).toBeLessThanOrEqual(2 * r - 4 + 1e-9);
      }
    }
  });

  it("keeps a name whole when it fits, at the house size", () => {
    expect(court.fitSeatLabel("claude-opus-4", court.R_MAX)).toEqual({ text: "opus-4", fontSize: court.LABEL_FONT_MAX });
  });

  it("keeps distinct refs distinct once it has to cut", () => {
    const names = ["claude-opus-4-1", "claude-opus-4-5", "claude-sonnet-4", "gemini-2.5-pro", "gemini-2.5-flash"];
    for (const r of [court.R_MIN, court.R_EVEN, court.R_MAX]) {
      const texts = names.map((n) => court.fitSeatLabel(n, r).text);
      expect(new Set(texts).size).toBe(names.length);
    }
  });
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
    expect(court.processLine(v).slice(-1)[0]).toMatchObject({ label: "Verdict", reachable: false });
  });

  it("strikes only the phases that will never happen", () => {
    // Running: everything after the live phase is still to come, and a future phase is not struck.
    const running = view({ rounds: 2, current_round: 1, current_phase: "critique", seats: [seat({ steps: [step(), step({ round: 1, phase: "critique", status: "pending", run_id: 4 })] })] });
    expect(court.processLine(running).map((p) => [p.label, p.struck])).toEqual([
      ["Opening", false], ["Deliberation I", false], ["Rebuttal I", false], ["Deliberation II", false], ["Verdict", false],
    ]);
    // Stopped early: what never ran is struck, what ran is not.
    const stopped = view({
      rounds: 2, rounds_run: 1, stopped_early: true, status: "done", current_round: 1, current_phase: "synthesis",
      synthesis: "done", seats: [seat({ steps: [step(), step({ round: 1, phase: "critique" })] })],
    });
    expect(court.processLine(stopped).map((p) => [p.label, p.struck])).toEqual([
      ["Opening", false], ["Deliberation I", false], ["Rebuttal I", true], ["Deliberation II", true], ["Verdict", false],
    ]);
    // Failed: the verdict that never came is struck.
    const failed = view({ status: "error", error: "boom", current_round: 1, current_phase: "critique" });
    expect(court.processLine(failed).slice(-1)[0]).toMatchObject({ label: "Verdict", struck: true });
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
  it("draws no judge line when every Borda score is 0", () => {
    expect(court.topLinks([{ seat_idx: 0, score: 0, n: 2 }, { seat_idx: 1, score: 0, n: 2 }])).toEqual([]);
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

describe("the session court", () => {
  const two = [
    seat({ seat_idx: 0, ref: "gpt-5", role: "skeptic", steps: [step(), step({ round: 1, phase: "critique", status: "ok", critique: { reviews: [], ranking: ["B"] } as never })] }),
    seat({ seat_idx: 1, ref: "opus", steps: [step(), step({ round: 1, phase: "critique", status: "pending", run_id: 9 })] }),
  ];
  const running = view({ current_round: 1, current_phase: "critique", anon_map: { A: 0, B: 1 }, seats: two });

  it("masks seats with their anonymous letter in a critique, and only there", () => {
    const c = court.buildSessionCourt(running, "live");
    expect(c.seats.map((s) => [s.label, s.masked, s.name])).toEqual([["A", true, "gpt-5"], ["B", true, "opus"]]);
    const opening = court.buildSessionCourt(running, { round: 0, phase: "answer" });
    expect(opening.seats.map((s) => [s.label, s.masked])).toEqual([["gpt-5", false], ["opus", false]]);
  });

  it("centres the seats it shows and states each one", () => {
    const c = court.buildSessionCourt(running, "live");
    expect(c.seats.map((s) => s.state)).toEqual(["ok", "speaking"]);
    expect(c.seats[0].x).toBeLessThan(280);
    expect(c.seats[1].x).toBeGreaterThan(280);
    expect(c.seats[0].sublabel).toBe("skeptic");
  });

  it("settles each seat in from the setup chair it occupied, gaps and all", () => {
    const three = view({ seats: [0, 1, 2].map((i) => seat({ seat_idx: i, ref: `m${i}`, steps: [step()] })) });
    const c = court.buildSessionCourt(three, "live", [0, 2, 5]);
    expect(c.seats.map((s) => s.from)).toEqual([0, 2, 5].map((chair) => court.pointAt(court.setupAngle(chair))));
  });

  it("remembers the occupied chairs of the draft a council was convened from", () => {
    const d = draftFromConfig(null);
    const o = { seat: null, name: "x" };
    d.chairs[0] = o; d.chairs[2] = o; d.chairs[5] = o;
    rememberChairs("c-1", d);
    expect(chairsOf("c-1")).toEqual([0, 2, 5]);
    expect(chairsOf("c-2")).toBeUndefined();
  });

  it("falls back to the compact chairs when the setup layout is unknown or does not fit", () => {
    const compact = [0, 1].map((chair) => court.pointAt(court.setupAngle(chair)));
    expect(court.buildSessionCourt(running, "live").seats.map((s) => s.from)).toEqual(compact);
    expect(court.buildSessionCourt(running, "live", [4]).seats.map((s) => s.from)).toEqual(compact);
  });

  it("counts the ballots of the round's critique", () => {
    expect(court.buildSessionCourt(running, "live").ballots).toMatchObject({ got: 1, total: 2 });
    expect(court.buildSessionCourt(running, "live").ballots?.cast).toHaveLength(1);
    expect(court.buildSessionCourt(running, { round: 0, phase: "answer" }).ballots).toBeNull();
  });

  it("keeps the judge waiting until the synthesis, then speaking, then the verdict", () => {
    expect(court.buildSessionCourt(running, "live").judge.state).toBe("waiting");
    const writing = view({ ...running, current_phase: "synthesis" });
    expect(court.buildSessionCourt(writing, "live").judge.state).toBe("speaking");
    const done = view({ ...running, status: "done", current_phase: "synthesis", synthesis_structured: synthesis() });
    expect(court.buildSessionCourt(done, "live").judge).toMatchObject({ state: "verdict", label: "VERDICT", sub: "confidence high" });
  });

  it("says where the session is on the right wing", () => {
    expect(court.buildSessionCourt(running, "live").wing).toEqual({ title: "ROUND I · deliberation", sub: "1 of 2 seats" });
    const done = view({ ...running, status: "done", rounds: 2, rounds_run: 2, stopped_early: true, current_phase: "synthesis", agreement: agreement(0.7, "strong") });
    expect(court.buildSessionCourt(done, "live").wing.title).toBe("2 ROUNDS · strong consensus · stopped early");
  });

  it("draws one seat alone at 90° whose scales never leave insufficient", () => {
    const one = view({ seats: [seat()], current_round: 1, current_phase: "critique" });
    const c = court.buildSessionCourt(one, "live");
    expect(c.seats[0].x).toBeCloseTo(280);
    expect(c.balance.tone).toBe("insufficient");
  });

  it("turns an error council's unfinished steps cancelled and the judge red", () => {
    const failed = view({ ...running, status: "error", error: "boom" });
    const c = court.buildSessionCourt(failed, "live");
    expect(c.seats.map((s) => s.state)).toEqual(["ok", "cancelled"]);
    expect(c.judge.state).toBe("failed");
  });

  it("carries who cast each ballot, so a ballot keeps its own chair as its origin", () => {
    // Seat 1 finished its critique BEFORE seat 0: the tray holds one ballot, and it is seat 1's.
    const crit = (status: string) => step({ round: 1, phase: "critique", status, critique: status === "ok" ? ({ reviews: [], ranking: ["A"] } as never) : null });
    const late = view({ ...running, seats: [
      seat({ seat_idx: 0, ref: "gpt-5", steps: [step(), crit("pending")] }),
      seat({ seat_idx: 1, ref: "opus", steps: [step(), crit("ok")] }),
    ] });
    const b = court.buildSessionCourt(late, "live").ballots!;
    expect(b.cast.map((c) => c.seat)).toEqual([1]);
    const chair1 = court.buildSessionCourt(late, "live").seats[1];
    expect(b.cast[0].from).toEqual({ x: chair1.x, y: chair1.y });
    const both = view({ ...late, seats: [
      seat({ seat_idx: 0, ref: "gpt-5", steps: [step(), crit("ok")] }),
      seat({ seat_idx: 1, ref: "opus", steps: [step(), crit("ok")] }),
    ] });
    expect(court.buildSessionCourt(both, "live").ballots!.cast.map((c) => c.seat)).toEqual([0, 1]);
  });

  it("says COUNCIL FAILED on the right wing of an error council, even at the synthesis", () => {
    const failed = view({ ...running, status: "error", error: "boom", current_phase: "synthesis" });
    expect(court.buildSessionCourt(failed, "live").wing).toEqual({ title: "COUNCIL FAILED", sub: null });
  });

  it("dims a cancelled council", () => {
    expect(court.buildSessionCourt(view({ status: "cancelled" }), "live").dimmed).toBe(true);
  });

  it("sizes, ranks, links and marks dissent only at the verdict", () => {
    const board = [{ seat_idx: 0, score: 1, n: 1 }, { seat_idx: 1, score: 0, n: 1 }];
    const s = synthesis({ minority: "m", disagreements: [{ topic: "t", positions: [{ seats: [0], view: "a" }, { seats: [1, 1], view: "b" }] }] });
    const done = view({ ...running, status: "done", current_phase: "synthesis", leaderboard: board,
      leaderboard_by_round: [board], agreement_by_round: [agreement(1, "strong")], ballot_rounds: [1], agreement: agreement(1, "strong"),
      synthesis_structured: s });
    const c = court.buildSessionCourt(done, "live");
    expect(c.seats.map((x) => [x.r, x.rank, x.dissent])).toEqual([[24, 1, true], [14, 2, false]]);
    expect(c.links.map((l) => l.kind)).toEqual(["top", "dissent"]);
    const mid = court.buildSessionCourt(done, { round: 1, phase: "critique" });
    expect(mid.seats.map((x) => x.rank)).toEqual([null, null]);
    expect(mid.links).toEqual([]);
  });
});

describe("the setup court", () => {
  it("always draws eight chairs over the whole arc, occupied or empty", () => {
    const c = court.buildSetupCourt(cfg, draftFromConfig(cfg), "why?", 2);
    expect(c.seats).toHaveLength(8);
    expect(c.seats.map((s) => s.state)).toEqual(["seated", "seated", "seated", "empty", "empty", "empty", "empty", "empty"]);
    expect(c.seats[3].label).toBe("+");
    expect(c.seats[0].x).toBeCloseTo(court.pointAt(168).x);
  });
  it("says on the left wing that fewer than three seats cannot measure agreement", () => {
    const two = release(draftFromConfig(cfg), 2);
    expect(court.buildSetupCourt(cfg, two, "", 2).balance).toMatchObject({ tone: "insufficient", label: "no agreement measure", sub: "fewer than 3 seats" });
    expect(court.buildSetupCourt(cfg, draftFromConfig(cfg), "", 2).balance.tone).toBe("empty");
  });
  it("shows rounds and the call ceiling on the right wing", () => {
    expect(court.buildSetupCourt(cfg, draftFromConfig(cfg), "", 2).wing).toEqual({ title: "ROUNDS 2", sub: "up to 14 calls · 5 steps in sequence" });
  });
  it("truncates the question on the lectern and keeps it whole for the dossier", () => {
    const long = "x".repeat(60);
    const c = court.buildSetupCourt(cfg, draftFromConfig(cfg), long, 2);
    expect(c.lectern.text).toHaveLength(court.LECTERN_CHARS);
    expect(c.lectern.full).toBe(long);
  });
  it("puts the chairman on the bench, or leaves it empty", () => {
    expect(court.buildSetupCourt(cfg, draftFromConfig(cfg), "", 2).judge).toMatchObject({ state: "seated", name: "opus" });
    expect(court.buildSetupCourt(null, draftFromConfig(null), "", undefined).judge.state).toBe("empty");
  });
});
