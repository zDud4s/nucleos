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
