import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import type { Agreement, CouncilConfig, CouncilView, SeatView, StepView, Synthesis } from "../../data/council";
import { Courtroom, type CourtSelection } from "./Courtroom";
import { buildSessionCourt, buildSetupCourt } from "./court-model";
import { draftFromConfig, type CourtDraft } from "./court-draft";

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
    current_round: 0,
    current_phase: "answer",
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

const agreement = (tau: number, level: string): Agreement => ({ tau, level, ballots: 2, comparisons: 1 });

function config(overrides: Partial<CouncilConfig> = {}): CouncilConfig {
  return {
    configured: true,
    default_rounds: 2,
    max_rounds: 3,
    roles: ["skeptic", "pragmatist"],
    default_roster: {
      chairman: { agent: "opus" },
      members: [{ agent: "opus" }, { agent: "gpt-5" }, { agent: "sonnet" }],
    },
    ...overrides,
  };
}

function setupDraft(): CourtDraft {
  return draftFromConfig(config());
}

const noop = () => undefined;

function renderSetup(over: { selected?: CourtSelection; onSelect?: (s: CourtSelection) => void; onSubmit?: () => void; draft?: CourtDraft } = {}) {
  const court = buildSetupCourt(config(), over.draft ?? setupDraft(), "", 2);
  return render(
    <Courtroom
      court={court}
      mode="setup"
      selected={over.selected ?? null}
      onSelect={over.onSelect ?? noop}
      question={{ value: "", onChange: noop, onSubmit: over.onSubmit ?? noop }}
    />,
  );
}

function renderSession(v: CouncilView, over: { selected?: CourtSelection; onSelect?: (s: CourtSelection) => void } = {}) {
  return render(
    <Courtroom
      court={buildSessionCourt(v, "live")}
      mode="session"
      selected={over.selected ?? null}
      onSelect={over.onSelect ?? noop}
    />,
  );
}

/* ------------------------------------------------------------ Courtroom -- */

describe("Courtroom", () => {
  it("names every chair for a screen reader", () => {
    renderSetup();
    expect(screen.getByRole("button", { name: "Seat 1, opus" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Seat 4, empty" })).toBeDefined();
  });

  it("names a chair in session with its role and state", () => {
    renderSession(
      view({
        seats: [
          seat({ ref: "gpt-5", role: "skeptic", steps: [step({ status: "pending", run_id: 5 })] }),
          seat({ seat_idx: 1, ref: "opus", steps: [step({ status: "error", error: "boom" })] }),
          seat({ seat_idx: 2, ref: "sonnet" }),
        ],
      }),
    );
    expect(screen.getByRole("button", { name: "Seat 1, gpt-5, skeptic, speaking" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Seat 2, opus, failed: boom" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Seat 3, sonnet, done" })).toBeDefined();
  });

  it("does not unmask a chair in its accessible name", () => {
    const v = view({
      current_round: 1,
      current_phase: "critique",
      seats: [
        seat({ ref: "gpt-5", steps: [step(), step({ round: 1, phase: "critique", status: "pending", run_id: null })] }),
        seat({ seat_idx: 1, ref: "opus", steps: [step(), step({ round: 1, phase: "critique", status: "pending", run_id: null })] }),
      ],
    });
    renderSession(v);
    const first = screen.getByRole("button", { name: "Seat 1, A, thinking" });
    expect(first.querySelector("title")?.textContent).toBe("gpt-5");
    expect(first.getAttribute("data-masked")).toBe("true");
  });

  it("moves along the arc with the arrow keys and selects with Enter", () => {
    const onSelect = vi.fn();
    renderSetup({ onSelect });
    const first = screen.getByRole("button", { name: "Seat 1, opus" });
    first.focus();
    fireEvent.keyDown(first, { key: "ArrowRight" });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Seat 2, gpt-5" }));
    fireEvent.keyDown(document.activeElement as Element, { key: "End" });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Seat 8, empty" }));
    fireEvent.keyDown(document.activeElement as Element, { key: "ArrowRight" });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Seat 8, empty" }));
    fireEvent.keyDown(document.activeElement as Element, { key: "Home" });
    expect(document.activeElement).toBe(first);
    fireEvent.keyDown(first, { key: "ArrowDown" });
    fireEvent.keyDown(document.activeElement as Element, { key: "ArrowUp" });
    expect(document.activeElement).toBe(first);

    fireEvent.keyDown(screen.getByRole("button", { name: "Seat 2, gpt-5" }), { key: "Enter" });
    expect(onSelect).toHaveBeenCalledWith({ kind: "seat", index: 1 });
  });

  it("selects the judge, and the lectern when its question field is focused", () => {
    const onSelect = vi.fn();
    renderSetup({ onSelect });
    fireEvent.click(screen.getByRole("button", { name: "Chairman, opus" }));
    expect(onSelect).toHaveBeenLastCalledWith({ kind: "judge" });
    fireEvent.focus(screen.getByRole("textbox", { name: "Question" }));
    expect(onSelect).toHaveBeenLastCalledWith({ kind: "lectern" });
  });

  it("submits the question on ctrl+enter but not while an IME composes", () => {
    const onSubmit = vi.fn();
    renderSetup({ onSubmit });
    const box = screen.getByRole("textbox", { name: "Question" });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(onSubmit).not.toHaveBeenCalled();
    fireEvent.keyDown(box, { key: "Enter", ctrlKey: true, isComposing: true });
    expect(onSubmit).not.toHaveBeenCalled();
    fireEvent.keyDown(box, { key: "Enter", ctrlKey: true });
    expect(onSubmit).toHaveBeenCalledTimes(1);
  });

  it("marks the selected chair pressed", () => {
    renderSetup({ selected: { kind: "seat", index: 3 } });
    expect(screen.getByRole("button", { name: "Seat 4, empty" }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: "Seat 1, opus" }).getAttribute("aria-pressed")).toBe("false");
  });

  it("draws the scales' label and the ballots tray", () => {
    const critique = { reviews: [], ranking: ["A"] };
    const v = view({
      current_round: 1,
      current_phase: "critique",
      ballot_rounds: [1],
      agreement_by_round: [agreement(0.21, "split")],
      leaderboard_by_round: [[]],
      seats: [
        seat({ steps: [step(), step({ round: 1, phase: "critique", critique, answer: null })] }),
        seat({ seat_idx: 1, steps: [step(), step({ round: 1, phase: "critique", status: "pending", answer: null })] }),
      ],
    });
    const { container } = renderSession(v);
    expect(screen.getByText("split · τ 0.21")).toBeDefined();
    expect(screen.getByText("ballots 1/2")).toBeDefined();
    expect(container.querySelectorAll("rect.council-court-ballot")).toHaveLength(1);
  });

  it("draws a titled line per link", () => {
    const synthesis: Synthesis = {
      answer: "ship",
      consensus: [],
      disagreements: [
        {
          topic: "timing",
          positions: [
            { seats: [0], view: "now" },
            { seats: [1], view: "later" },
          ],
        },
      ],
      minority: "later",
      confidence: { level: "high", why: "" },
      open_questions: [],
    };
    const v = view({
      status: "done",
      rounds_run: 1,
      current_round: 1,
      current_phase: "synthesis",
      agreement: agreement(0.7, "strong"),
      leaderboard: [
        { seat_idx: 0, score: 1, n: 2 },
        { seat_idx: 1, score: 0.4, n: 2 },
      ],
      synthesis_structured: synthesis,
      seats: [seat(), seat({ seat_idx: 1, ref: "opus" })],
    });
    const { container } = renderSession(v);
    const top = container.querySelectorAll("line.council-court-link-top");
    expect(top).toHaveLength(1);
    expect(top[0].querySelector("title")?.textContent).toBe("most voted");
    const dissent = container.querySelectorAll("line.council-court-link-dissent");
    expect(dissent).toHaveLength(1);
    expect(dissent[0].querySelector("title")?.textContent).toBe("timing");
  });

  it("falls back to dashed slots for ballots not yet cast and tilts the beam with the agreement", () => {
    const v = view({
      current_round: 1,
      current_phase: "critique",
      ballot_rounds: [1],
      agreement_by_round: [agreement(0, "none")],
      leaderboard_by_round: [[]],
      seats: [seat(), seat({ seat_idx: 1 })],
    });
    const { container } = renderSession(v);
    expect(container.querySelectorAll("rect.council-court-ballot-slot")).toHaveLength(2);
    expect((container.querySelector("g.council-court-beam") as SVGElement).style.transform).toBe("rotate(-22deg)");
  });
});
