import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import { createAppQueryClient } from "../../app/queryClient";
import type { Agreement, CouncilConfig, CouncilView, SeatView, StepView, Synthesis } from "../../data/council";
import { Courtroom, type CourtSelection } from "./Courtroom";
import { Dossier } from "./Dossier";
import { ProcessLine } from "./ProcessLine";
import { buildSessionCourt, buildSetupCourt, liveMoment, momentKey, processLine } from "./court-model";
import { draftFromConfig, type CourtDraft } from "./court-draft";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../../data/client", async (original) => ({
  ...(await original<typeof import("../../data/client")>()),
  ...daemon,
}));

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
    expect(first.getAttribute("title")).toBe("gpt-5");
    expect(first.getAttribute("data-masked")).toBe("true");
  });

  it("is made of real buttons that Space also selects", () => {
    const onSelect = vi.fn();
    renderSetup({ onSelect });
    const seatButton = screen.getByRole("button", { name: "Seat 1, opus" });
    expect(seatButton.tagName).toBe("BUTTON");
    expect(screen.getByRole("button", { name: /^Chairman, / }).tagName).toBe("BUTTON");
    fireEvent.keyDown(seatButton, { key: " " });
    expect(onSelect).toHaveBeenCalledWith({ kind: "seat", index: 0 });
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

/* ------------------------------------------------------------ ProcessLine -- */

describe("ProcessLine", () => {
  const convene = { disabled: false, reason: null, onConvene: noop, pending: false };
  const phases = (list: HTMLElement) => within(list).getAllByRole("listitem").map((li) => li.textContent);

  it("lists the phases of the council to be, and steps the rounds", () => {
    const onRounds = vi.fn();
    render(<ProcessLine mode="setup" rounds={2} maxRounds={3} onRounds={onRounds} convene={convene} />);
    expect(phases(screen.getByRole("list", { name: "Process" }))).toEqual([
      "Opening",
      "Deliberation I",
      "Rebuttal I",
      "Deliberation II",
      "Verdict",
    ]);
    fireEvent.click(screen.getByRole("button", { name: "Add a round" }));
    expect(onRounds).toHaveBeenLastCalledWith(3);
    fireEvent.click(screen.getByRole("button", { name: "Remove the last round" }));
    expect(onRounds).toHaveBeenLastCalledWith(1);
  });

  it("stops the round buttons at the ends", () => {
    const { rerender } = render(<ProcessLine mode="setup" rounds={3} maxRounds={3} onRounds={noop} convene={convene} />);
    expect((screen.getByRole("button", { name: "Add a round" }) as HTMLButtonElement).disabled).toBe(true);
    rerender(<ProcessLine mode="setup" rounds={1} maxRounds={3} onRounds={noop} convene={convene} />);
    expect((screen.getByRole("button", { name: "Remove the last round" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("draws only the opening and the verdict while the rounds are unknown", () => {
    render(<ProcessLine mode="setup" rounds={undefined} maxRounds={undefined} onRounds={noop} convene={convene} />);
    expect(phases(screen.getByRole("list", { name: "Process" }))).toEqual(["Opening", "Verdict"]);
    expect((screen.getByRole("button", { name: "Add a round" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Remove the last round" }) as HTMLButtonElement).disabled).toBe(true);
    render(<ProcessLine mode="setup" rounds={2} maxRounds={undefined} onRounds={noop} convene={convene} />);
    expect((screen.getAllByRole("button", { name: "Add a round" })[1] as HTMLButtonElement).disabled).toBe(true);
  });

  it("says why Convene is disabled, to the eye and to a screen reader", () => {
    const onConvene = vi.fn();
    const reason = "Seat at least one member.";
    const { container } = render(
      <ProcessLine mode="setup" rounds={2} maxRounds={3} onRounds={noop} convene={{ disabled: true, reason, onConvene, pending: false }} />,
    );
    const button = screen.getByRole("button", { name: "Convene" });
    expect(button.getAttribute("title")).toBe(reason);
    const described = container.querySelector(`#${CSS.escape(button.getAttribute("aria-describedby") ?? "")}`);
    expect(described?.textContent).toBe(reason);
    expect(described?.className).toBe("sr-only");
    expect((button as HTMLButtonElement).disabled).toBe(true);
  });

  it("convenes when it is not blocked", () => {
    const onConvene = vi.fn();
    render(<ProcessLine mode="setup" rounds={2} maxRounds={3} onRounds={noop} convene={{ ...convene, onConvene }} />);
    fireEvent.click(screen.getByRole("button", { name: "Convene" }));
    expect(onConvene).toHaveBeenCalledTimes(1);
  });

  describe("in a session", () => {
    // Round 1 deliberation is under way: the opening has steps, the rest has not happened yet.
    const live = view({
      rounds: 2,
      current_round: 1,
      current_phase: "critique",
      seats: [seat({ steps: [step(), step({ round: 1, phase: "critique", status: "pending", run_id: 4 })] })],
    });

    it("keeps phases still to come out of reach, plain and not struck, and the viewed one current", () => {
      render(<ProcessLine mode="session" phases={processLine(live)} liveKey={momentKey(liveMoment(live))} viewing="live" onView={noop} />);
      const rebuttal = screen.getByText("Rebuttal I");
      expect(rebuttal.tagName).toBe("SPAN");
      expect(rebuttal.className).toContain("council-process-phase-ahead");
      expect(rebuttal.className).not.toContain("council-process-phase-unreachable");
      expect(screen.queryByRole("button", { name: "Rebuttal I" })).toBeNull();
      expect(screen.getByRole("button", { name: "Deliberation I" }).getAttribute("aria-current")).toBe("step");
      expect(screen.getByRole("button", { name: "Opening" }).getAttribute("aria-current")).toBeNull();
    });

    it("goes back to a phase by its key and returns to live by the current one", () => {
      const onView = vi.fn();
      render(<ProcessLine mode="session" phases={processLine(live)} liveKey={momentKey(liveMoment(live))} viewing="live" onView={onView} />);
      fireEvent.click(screen.getByRole("button", { name: "Opening" }));
      expect(onView).toHaveBeenLastCalledWith("0:answer");
      fireEvent.click(screen.getByRole("button", { name: "Deliberation I" }));
      expect(onView).toHaveBeenLastCalledWith("live");
    });

    it("marks the viewed phase when the reader has stepped back", () => {
      render(<ProcessLine mode="session" phases={processLine(live)} liveKey={momentKey(liveMoment(live))} viewing="0:answer" onView={noop} />);
      expect(screen.getByRole("button", { name: "Opening" }).getAttribute("aria-current")).toBe("step");
      expect(screen.getByRole("button", { name: "Deliberation I" }).getAttribute("aria-current")).toBeNull();
    });

    it("treats the phase an ended council stopped in as its live one", () => {
      // An error mid-deliberation: nothing is `current`, yet round 1's deliberation is where it stopped.
      const failed = view({ ...live, status: "error", error: "boom" });
      const onView = vi.fn();
      render(<ProcessLine mode="session" phases={processLine(failed)} liveKey={momentKey(liveMoment(failed))} viewing="live" onView={onView} />);
      expect(screen.getByRole("button", { name: "Deliberation I" }).getAttribute("aria-current")).toBe("step");
      expect(screen.getByRole("button", { name: "Opening" }).getAttribute("aria-current")).toBeNull();
      fireEvent.click(screen.getByRole("button", { name: "Deliberation I" }));
      expect(onView).toHaveBeenLastCalledWith("live");
    });

    it("says where a council stopped early", () => {
      const stopped = view({
        status: "done",
        rounds: 3,
        rounds_run: 1,
        stopped_early: true,
        current_round: 1,
        current_phase: "synthesis",
        seats: [seat({ steps: [step(), step({ round: 1, phase: "critique", answer: null })] })],
      });
      render(<ProcessLine mode="session" phases={processLine(stopped)} liveKey={momentKey(liveMoment(stopped))} viewing="live" onView={noop} />);
      expect(screen.getByRole("button", { name: "Deliberation I · stopped early" })).toBeDefined();
      // What will never happen now is struck.
      expect(screen.getByText("Rebuttal I").className).toContain("council-process-phase-unreachable");
      // A finished council is at its verdict, and that is the live view.
      expect(screen.getByRole("button", { name: "Verdict" }).getAttribute("aria-current")).toBe("step");
    });
  });
});

/* --------------------------------------------------------------- Dossier -- */

describe("Dossier", () => {
  beforeEach(() => {
    daemon.apiFetch.mockReset();
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/agents") {
        return [
          { id: "ag-1", name: "the sceptic", speciality: "", prompt: "", engine: "claude", model: "claude-opus-5", tool_policy: "mcp_only", created_at: "", updated_at: "" },
          { id: "ag-2", name: "the mute", speciality: "", prompt: "", engine: "claude", model: null, tool_policy: "mcp_only", created_at: "", updated_at: "" },
        ];
      }
      if (path === "/assistant/models") {
        return {
          choices: [
            { id: "claude-opus-5", label: "Opus 5", brain: "cloud", efforts: [] },
            { id: "qwen3.5:4b", label: "Qwen 4b", brain: "local", efforts: [] },
          ],
          configured: "claude-opus-5",
          efforts: [],
        };
      }
      return undefined;
    });
  });

  function renderDossier(ui: React.ReactElement) {
    return render(<QueryClientProvider client={createAppQueryClient()}>{ui}</QueryClientProvider>);
  }

  function setupDossier(over: { selection?: CourtSelection; onDraft?: (d: CourtDraft) => void } = {}) {
    const draft = setupDraft();
    return renderDossier(
      <Dossier
        mode="setup"
        selection={over.selection ?? null}
        draft={draft}
        question=""
        court={buildSetupCourt(config(), draft, "", 2)}
        config={config()}
        refusal={null}
        onDraft={over.onDraft ?? noop}
        onClear={noop}
      />,
    );
  }

  function sessionDossier(v: CouncilView, selection: CourtSelection, following = true, onFollowLive = noop) {
    return renderDossier(
      <Dossier
        mode="session"
        view={v}
        court={buildSessionCourt(v, "live")}
        moment={liveMoment(v)}
        selection={selection}
        following={following}
        onFollowLive={onFollowLive}
      />,
    );
  }

  it("says so when neither catalogue could be read", async () => {
    daemon.apiFetch.mockReset();
    daemon.apiFetch.mockRejectedValue(new Error("down"));
    setupDossier({ selection: { kind: "seat", index: 3 } });
    expect(await screen.findByText("Couldn't load who can sit.")).toBeDefined();
  });

  it("asks for neither catalogue while nothing is chosen", () => {
    setupDossier();
    expect(screen.getByRole("heading", { name: "The case" })).toBeDefined();
    expect(screen.getByText("Write the question on the lectern.")).toBeDefined();
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });

  it("calls a model or an agent to an empty chair, never an agent without a model", async () => {
    const onDraft = vi.fn();
    setupDossier({ selection: { kind: "seat", index: 3 }, onDraft });
    expect(screen.getByRole("heading", { name: "Call to seat 4" })).toBeDefined();
    const agents = await screen.findByRole("group", { name: "Agents" });
    expect(within(agents).getByRole("button", { name: "the sceptic" })).toBeDefined();
    expect(within(agents).queryByRole("button", { name: "the mute" })).toBeNull();

    fireEvent.click(await screen.findByRole("button", { name: "Qwen 4b" }));
    const next = onDraft.mock.calls[0][0] as CourtDraft;
    expect(next.chairs[3]).toEqual({ seat: { kind: "local", ref: "qwen3.5:4b" }, name: "Qwen 4b" });
    expect(next.touched).toBe(true);
  });

  it("hands out a role badge", () => {
    const onDraft = vi.fn();
    setupDossier({ selection: { kind: "seat", index: 3 }, onDraft });
    fireEvent.click(screen.getByRole("button", { name: "skeptic" }));
    expect((onDraft.mock.calls[0][0] as CourtDraft).roles[3]).toBe("skeptic");
  });

  it("releases an occupied chair", () => {
    const onDraft = vi.fn();
    setupDossier({ selection: { kind: "seat", index: 0 }, onDraft });
    expect(screen.getByRole("heading", { name: "Seat 1" })).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Release" }));
    expect((onDraft.mock.calls[0][0] as CourtDraft).chairs[0]).toBeNull();
  });

  it("calls someone to the bench", async () => {
    const onDraft = vi.fn();
    setupDossier({ selection: { kind: "judge" }, onDraft });
    expect(screen.getByRole("heading", { name: "Chairman" })).toBeDefined();
    fireEvent.click(await screen.findByRole("button", { name: "Opus 5" }));
    expect((onDraft.mock.calls[0][0] as CourtDraft).chairman).toEqual({
      seat: { kind: "cloud", ref: "claude-opus-5" },
      name: "Opus 5",
    });
  });

  it("tails a seat that is speaking", async () => {
    const v = view({
      seats: [seat({ ref: "gpt-5", steps: [step({ status: "pending", run_id: 5, answer: null })] })],
    });
    sessionDossier(v, { kind: "seat", index: 0 });
    expect(await screen.findByRole("log", { name: "Live tail of gpt-5" })).toBeDefined();
  });

  it("reads a settled answer, a failure and an expired transcript", () => {
    const v = view({
      seats: [
        seat({ ref: "a", steps: [step({ answer: "**ship it**" })] }),
        seat({ seat_idx: 1, ref: "b", steps: [step({ status: "error", error: "timed out", answer: null })] }),
        seat({ seat_idx: 2, ref: "c", steps: [step({ answer: null })] }),
      ],
    });
    const first = sessionDossier(v, { kind: "seat", index: 0 });
    expect(screen.getByText("ship it").tagName).toBe("STRONG");
    first.unmount();
    const second = sessionDossier(v, { kind: "seat", index: 1 });
    expect(screen.getAllByText("timed out").length).toBeGreaterThan(0);
    second.unmount();
    sessionDossier(v, { kind: "seat", index: 2 });
    expect(screen.getByText("transcript expired")).toBeDefined();
  });

  it("deanonymises a ballot", () => {
    const v = view({
      current_round: 1,
      current_phase: "critique",
      seats: [
        seat({
          ref: "a",
          steps: [step(), step({ round: 1, phase: "critique", answer: null, critique: { reviews: [], ranking: ["B", "A"] } })],
        }),
        seat({ seat_idx: 1, ref: "b" }),
      ],
    });
    sessionDossier(v, { kind: "seat", index: 0 });
    const ballot = screen.getByRole("list", { name: "Ballot of A" });
    expect(within(ballot).getAllByRole("listitem").map((li) => li.textContent)).toEqual(["b", "a"]);
    // The masked chair is titled by its letter in the dossier too, and its ballot is named by it:
    // the real name never reaches the accessible tree while the round is masked.
    expect(screen.queryByRole("list", { name: "Ballot of a" })).toBeNull();
    expect(screen.getByRole("heading", { name: "A" })).toBeDefined();
  });

  it("shows the error of a council that failed mid-round at the top of the dossier", () => {
    const failed = view({ rounds: 2, current_round: 1, current_phase: "critique", status: "error", error: "the panel lost its connection" });
    sessionDossier(failed, null);
    expect(screen.getByText("the panel lost its connection")).toBeDefined();
  });

  it("shows the error of a failed council once, where the verdict already renders it", () => {
    const failed = view({ rounds: 1, current_round: 1, current_phase: "synthesis", status: "error", error: "the chair failed" });
    sessionDossier(failed, { kind: "judge" });
    expect(screen.getAllByText("the chair failed")).toHaveLength(1);
  });

  it("tails the chairman while the verdict is written, once its run is known", async () => {
    const writing = view({ current_round: 1, current_phase: "synthesis", synthesis_run_id: 77 });
    sessionDossier(writing, { kind: "judge" });
    expect(await screen.findByRole("log", { name: "Live tail of claude-opus-4" })).toBeDefined();
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/runs/77/tail?since=0"));
  });

  it("says the chair is writing while the synthesis run is not open yet", () => {
    sessionDossier(view({ current_round: 1, current_phase: "synthesis", synthesis_run_id: null }), { kind: "judge" });
    expect(screen.getByText("The chair is writing the verdict…")).toBeDefined();
    expect(screen.queryByRole("log")).toBeNull();
  });

  it("says when a finished council has no transcript left", () => {
    sessionDossier(view({ status: "done", rounds_run: 1, current_phase: "synthesis" }), { kind: "judge" });
    expect(screen.getByText("transcript expired")).toBeDefined();
    expect(screen.getByText("chaired by claude-opus-4")).toBeDefined();
  });

  it("offers Follow live only when the reader is not following", () => {
    const onFollowLive = vi.fn();
    const v = view();
    const first = sessionDossier(v, null, true, onFollowLive);
    expect(screen.queryByRole("button", { name: "Follow live" })).toBeNull();
    first.unmount();
    sessionDossier(v, null, false, onFollowLive);
    fireEvent.click(screen.getByRole("button", { name: "Follow live" }));
    expect(onFollowLive).toHaveBeenCalledTimes(1);
  });

  it("repeats the room's readings, which a narrow screen hides from the room", () => {
    const v = view({
      current_round: 1,
      current_phase: "critique",
      ballot_rounds: [1],
      agreement_by_round: [agreement(0.21, "split")],
      leaderboard_by_round: [[]],
      seats: [
        seat({ steps: [step(), step({ round: 1, phase: "critique", answer: null, critique: { reviews: [], ranking: ["B"] } })] }),
        seat({ seat_idx: 1 }),
      ],
    });
    sessionDossier(v, null);
    const summary = document.querySelector("dl.council-dossier-summary") as HTMLElement;
    expect(summary.textContent).toContain("split · τ 0.21");
    expect(summary.textContent).toContain("ROUND I");
    expect(summary.textContent).toContain("ballots 1/2");
  });
});
